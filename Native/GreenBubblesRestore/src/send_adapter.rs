//! The send control plane: PRECHECK, capability minting, dispatch supervision,
//! audit, and reconciliation.
//!
//! This module holds no input or capture grant and performs no input itself.
//! It resolves the recipient from data it already owns, evaluates the offline
//! action-safety contract, mints one single-use bound capability, reserves the
//! idempotency key durably, hands the capability to the privilege-separated
//! helper across a bounded, watchdogged call, and then decides the action's
//! lifecycle state from *authoritative* evidence. The helper's own report is
//! evidence, never a verdict: `observedSent` is created only by replica
//! reconciliation, exactly as `ACTION_SAFETY_CONTRACT.md` requires.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::action::{
    assess_action_attempt, expected_approval_binding, ActionAdapterBinding, ActionAllowList,
    ActionAttemptIntent, ActionCapability, ActionGateEvidence, ActionGuardContext,
    ActionGuardDecision, ActionGuardDenial, ActionLifecycleState, ActionRateState,
    ExternalApprovalEvidence, ACTION_SAFETY_CONTRACT_VERSION,
};
use crate::archive::ensure_private_regular_file;
use crate::connector::{
    append_owner_only_connector_event, audit_connector_log, ActionDraft, ConnectorAuditEvent,
    ConnectorAuditOutcome, ConnectorAuditStage, ConnectorDestination,
};
use crate::model::{MessageDirection, TypedPayload};
use crate::replica::{search_replica_messages, ReplicaMessageFilter};
use crate::secret::ReplicaKey;
use crate::send_contract::{
    capability_binding_sha256, normalized_send_text_sha256, ActionAttachment,
    ActionCapabilityEnvelope, CalibrationSelfTestReport, HelperCapabilityStatus,
    HelperGateEvidence, HelperSendOutcome, SendCompletionKind, SendCompletionOutcome,
    SendFailureCode, SendRolloutStage, SendStage, VisualConfirmation, SEND_CONTRACT_VERSION,
};
use crate::send_outbox::{
    OutboxEntry, OutboxEntryState, SendCompletionRecord, SendOutbox, SendOutboxRecovery,
    SendOutboxState,
};
use crate::send_profile::{
    bind_profile_to_compatibility, compatibility_decision, load_calibration_profile,
    load_compatibility_matrix, CompatibilityDecision, SendTrustRoot, SendTrustTier,
    SignedCalibrationProfile, VerifiedCalibrationProfile, VerifiedCompatibilityMatrix,
};
use crate::send_staging::{
    discard_staging_directory, reviewed_uniform_type_identifier, stage_attachment, StagedAttachment,
};
use crate::tools::summarize_decoded_payload;
use crate::RestoreError;

/// Format version of the send adapter configuration document.
pub const SEND_ADAPTER_CONFIG_VERSION: u32 = 1;
/// Identity this adapter presents in the action-safety contract binding.
pub const SEND_ADAPTER_ID: &str = "greenbubbles-input-helper";
/// Version this adapter presents in the action-safety contract binding.
pub const SEND_ADAPTER_VERSION: &str = "1.0.0";
/// Bound on the configuration document.
pub const MAXIMUM_SEND_CONFIG_BYTES: u64 = 256 * 1024;
/// Bound on any helper response read across the dispatcher boundary.
pub const MAXIMUM_HELPER_RESPONSE_BYTES: u64 = 1024 * 1024;
/// Narrowest and widest permitted helper call timeouts.
pub const MINIMUM_HELPER_TIMEOUT_MILLISECONDS: u64 = 250;
pub const MAXIMUM_HELPER_TIMEOUT_MILLISECONDS: u64 = 120_000;
/// At the allow-listed stage the reviewed set stays deliberately tiny.
pub const MAXIMUM_ALLOW_LISTED_CONVERSATIONS: usize = 2;
/// Replica pages scanned while looking for a sent message.
pub const MAXIMUM_RECONCILIATION_PAGES: usize = 5;
/// Rows per reconciliation page.
pub const RECONCILIATION_PAGE_SIZE: usize = 100;
/// Decoded-payload budget when comparing a replica row to a sent body.
pub const RECONCILIATION_SUMMARY_BYTES: usize = 16 * 1024;

const IDEMPOTENCY_DOMAIN: &str = "greenbubbles.send.idempotency-key.v1";
const ACTION_IDENTITY_DOMAIN: &str = "greenbubbles.send.action-identity.v1";
const CAPABILITY_IDENTITY_DOMAIN: &str = "greenbubbles.send.capability-identity.v1";
const AUDIT_EVENT_DOMAIN: &str = "greenbubbles.send.audit-event.v1";
const SEND_OPERATION: &str = "executeSend";
const DRY_RUN_OPERATION: &str = "executeSendDryRun";
const RECONCILE_OPERATION: &str = "reconcileSend";

/// How to reach the privilege-separated input helper. The dispatcher is a
/// first-party executable inside the signed bundle; the control plane never
/// speaks the XPC protocol itself, and every call is bounded by a timeout the
/// *caller* owns so a helper stall can never block this process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SendHelperConfig {
    pub dispatcher_executable: PathBuf,
    #[serde(default)]
    pub dispatcher_arguments: Vec<String>,
    pub mach_service_name: String,
    pub status_timeout_milliseconds: u64,
    pub selftest_timeout_milliseconds: u64,
    pub send_timeout_milliseconds: u64,
}

/// The owner-only send adapter configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SendAdapterConfig {
    pub format_version: u32,
    pub account_id: String,
    pub rollout_stage: SendRolloutStage,
    pub global_kill_switch_engaged: bool,
    pub gate: ActionGateEvidence,
    pub adapter: ActionAdapterBinding,
    pub allow_list: ActionAllowList,
    pub self_send_conversation_id: String,
    /// Attachments have their own stage ladder, orthogonal to the text one.
    /// Both must be open before a file or image can leave the machine.
    #[serde(default = "dry_run_stage")]
    pub attachment_rollout_stage: SendRolloutStage,
    /// A separate, tighter capacity than text: an attachment send is louder and
    /// less reversible.
    #[serde(default = "one_attempt")]
    pub maximum_attachment_attempts_per_window: u64,
    /// Where single-use staging directories are created. One per attempt,
    /// removed when the attempt reaches a terminal state.
    pub staging_root: PathBuf,
    #[serde(default)]
    pub search_key_overrides: BTreeMap<String, String>,
    pub attempt_window_seconds: u64,
    pub maximum_attempts_per_window: u64,
    pub circuit_breaker_failure_threshold: u32,
    pub circuit_breaker_cooldown_seconds: u64,
    pub capability_validity_seconds: u64,
    pub reconciliation_grace_seconds: u64,
    /// How long the client still offers to recall a sent message. Surfaced so
    /// the owner knows exactly how much time is left; see `send recall-window`.
    pub recall_window_seconds: u64,
    pub expected_macos_build: String,
    pub expected_macos_major: u32,
    pub expected_wechat_build: String,
    pub calibration_profile_path: PathBuf,
    pub compatibility_matrix_path: PathBuf,
    #[serde(default)]
    pub development_trust_root_path: Option<PathBuf>,
    pub outbox_directory: PathBuf,
    pub audit_log_path: PathBuf,
    pub draft_directory: PathBuf,
    pub helper: SendHelperConfig,
}

impl SendAdapterConfig {
    /// Static validation. Every failure keeps the send path closed; there is
    /// no partially usable configuration.
    pub fn validate(&self) -> Result<(), SendFailureCode> {
        let timeouts_valid = [
            self.helper.status_timeout_milliseconds,
            self.helper.selftest_timeout_milliseconds,
            self.helper.send_timeout_milliseconds,
        ]
        .iter()
        .all(|value| {
            (MINIMUM_HELPER_TIMEOUT_MILLISECONDS..=MAXIMUM_HELPER_TIMEOUT_MILLISECONDS)
                .contains(value)
        });
        let paths_absolute = [
            self.calibration_profile_path.as_path(),
            self.compatibility_matrix_path.as_path(),
            self.outbox_directory.as_path(),
            self.audit_log_path.as_path(),
            self.draft_directory.as_path(),
            self.helper.dispatcher_executable.as_path(),
            self.staging_root.as_path(),
        ]
        .iter()
        .all(|path| path.is_absolute())
            && self
                .development_trust_root_path
                .as_ref()
                .is_none_or(|path| path.is_absolute());
        if self.format_version != SEND_ADAPTER_CONFIG_VERSION
            || self.account_id.is_empty()
            || self.adapter.adapter_id.is_empty()
            || self.adapter.adapter_version.is_empty()
            || self.adapter.client_build_profile_id.is_empty()
            || self.self_send_conversation_id.is_empty()
            || self.expected_macos_build.is_empty()
            || self.expected_wechat_build.is_empty()
            || self.expected_macos_major < 10
            || self.helper.mach_service_name.is_empty()
            || self.attempt_window_seconds == 0
            || self.maximum_attempts_per_window == 0
            || self.capability_validity_seconds == 0
            || self.capability_validity_seconds > 3_600
            || self.reconciliation_grace_seconds == 0
            || self.recall_window_seconds == 0
            || self.maximum_attachment_attempts_per_window == 0
            || !self.staging_root.is_absolute()
            || !timeouts_valid
            || !paths_absolute
            || self.allow_list.capabilities.is_empty()
            || self.allow_list.account_ids != BTreeSet::from([self.account_id.clone()])
        {
            return Err(SendFailureCode::ConfigurationInvalid);
        }
        // The rollout stages are cumulative and each one narrows the reachable
        // set of conversations; a wider allow list than the stage permits is a
        // configuration error rather than something to silently intersect.
        match self.rollout_stage {
            SendRolloutStage::DryRun => {}
            SendRolloutStage::SelfSend => {
                if self.allow_list.conversation_ids
                    != BTreeSet::from([self.self_send_conversation_id.clone()])
                {
                    return Err(SendFailureCode::ConfigurationInvalid);
                }
            }
            SendRolloutStage::AllowListed => {
                if !self
                    .allow_list
                    .conversation_ids
                    .contains(&self.self_send_conversation_id)
                    || self.allow_list.conversation_ids.len() > MAXIMUM_ALLOW_LISTED_CONVERSATIONS
                    || self.allow_list.conversation_ids.is_empty()
                {
                    return Err(SendFailureCode::ConfigurationInvalid);
                }
            }
        }
        Ok(())
    }

    /// Whether the attachment ladder permits sending this capability at all.
    /// Text is unaffected; an attachment needs *both* ladders open.
    pub fn stage_permits_capability(
        &self,
        capability: ActionCapability,
        conversation_id: &str,
    ) -> bool {
        if !self.stage_permits_send_to(conversation_id) {
            return false;
        }
        if !capability.carries_attachment() {
            return true;
        }
        match self.attachment_rollout_stage {
            SendRolloutStage::DryRun => false,
            SendRolloutStage::SelfSend => conversation_id == self.self_send_conversation_id,
            SendRolloutStage::AllowListed => {
                self.allow_list.conversation_ids.contains(conversation_id)
            }
        }
    }

    /// Whether this stage may press Return for this conversation at all.
    pub fn stage_permits_send_to(&self, conversation_id: &str) -> bool {
        match self.rollout_stage {
            SendRolloutStage::DryRun => false,
            SendRolloutStage::SelfSend => conversation_id == self.self_send_conversation_id,
            SendRolloutStage::AllowListed => {
                self.allow_list.conversation_ids.contains(conversation_id)
            }
        }
    }

    fn search_key_for(&self, draft: &ActionDraft) -> String {
        self.search_key_overrides
            .get(&draft.conversation_id)
            .cloned()
            .unwrap_or_else(|| draft.recipient.human_label.clone())
    }
}

/// Reads the owner-only configuration document.
pub fn load_send_adapter_config(path: &Path) -> Result<SendAdapterConfig, RestoreError> {
    ensure_private_regular_file(path)?;
    if fs::metadata(path)?.len() > MAXIMUM_SEND_CONFIG_BYTES {
        return Err(RestoreError::Integrity(
            "send adapter configuration exceeds its bounded size".to_string(),
        ));
    }
    let config: SendAdapterConfig = serde_json::from_slice(&fs::read(path)?)?;
    config.validate().map_err(failure_error)?;
    Ok(config)
}

/// A privacy-safe view of the durable outbox for reports and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SendOutboxSummary {
    pub in_flight: bool,
    pub pending_reconciliation_count: u64,
    pub circuit_breaker_open: bool,
    pub consecutive_failure_count: u32,
    pub last_failure: Option<SendFailureCode>,
    pub attempt_window_remaining: u64,
    pub reserved_attempt_total: u64,
    pub completed_attempt_total: u64,
    pub recovered_attempt_total: u64,
}

impl SendOutboxSummary {
    /// Projects the persisted state without releasing identities or digests.
    /// The configured maximum is supplied because an elapsed window will be
    /// rolled at the next reservation, so the honest remaining capacity is the
    /// configured one rather than whatever the stale window recorded.
    pub fn from_state(
        state: &SendOutboxState,
        now_unix_nanoseconds: u128,
        configured_maximum_attempts: u64,
    ) -> Self {
        let window = state.rate_window;
        let remaining = if now_unix_nanoseconds >= window.ends_at_unix_nanoseconds {
            configured_maximum_attempts
        } else {
            window
                .maximum_attempts
                .saturating_sub(window.reserved_attempts)
        };
        Self {
            in_flight: state.in_flight.is_some(),
            pending_reconciliation_count: state.pending_reconciliation.len() as u64,
            circuit_breaker_open: state.circuit_breaker.open(now_unix_nanoseconds),
            consecutive_failure_count: state.circuit_breaker.consecutive_failure_count,
            last_failure: state.circuit_breaker.last_failure,
            attempt_window_remaining: remaining,
            reserved_attempt_total: state.reserved_attempt_total,
            completed_attempt_total: state.completed_attempt_total,
            recovered_attempt_total: state.recovered_attempt_total,
        }
    }
}

/// The complete PRECHECK decision, taken before any effector call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SendPrecheckReport {
    pub format_version: u32,
    pub permitted: bool,
    pub permit_send: bool,
    pub rollout_stage: SendRolloutStage,
    pub failures: BTreeSet<SendFailureCode>,
    pub guard_denials: BTreeSet<ActionGuardDenial>,
    pub operator_actions: Vec<String>,
    pub account_id: String,
    pub conversation_id: String,
    pub draft_id: String,
    pub approval_id: String,
    pub idempotency_key: String,
    pub action_id: String,
    pub calibration_profile_id: String,
    pub calibration_trust_tier: SendTrustTier,
    pub compatibility: CompatibilityDecision,
    pub helper_status_observed: bool,
    pub outbox: SendOutboxSummary,
    pub checked_at_unix_nanoseconds: u128,
}

/// What one dispatched attempt produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SendAttemptReport {
    pub format_version: u32,
    pub precheck: SendPrecheckReport,
    pub dispatched: bool,
    pub attempted: bool,
    pub visual_confirmation: VisualConfirmation,
    pub completion: Option<SendCompletionKind>,
    pub awaiting_reconciliation: bool,
    pub lifecycle_state: Option<ActionLifecycleState>,
    pub stage_reached: Option<SendStage>,
    pub failure: Option<SendFailureCode>,
    pub operator_action: Option<String>,
    pub evidence: Option<HelperGateEvidence>,
    pub capability_id: String,
    pub idempotency_key: String,
    pub audit_event_count: u64,
    /// When the client stops offering to recall this message. Present only for
    /// an attempt that actually pressed Return.
    pub recall_deadline_unix_nanoseconds: Option<u128>,
    pub completed_at_unix_nanoseconds: u128,
}

/// Whether a sent message can still be recalled, and how.
///
/// Recall is performed by the owner in the client, deliberately. It is a
/// context-menu action whose item position varies with message type, age, and
/// locale, and a mis-aimed click there would delete or forward instead of
/// recalling — precisely the catastrophic class the on-screen gates exist to
/// prevent, and one the adapter cannot verify before committing to the click.
/// The adapter therefore surfaces the window and the exact steps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SendRecallWindowReport {
    pub format_version: u32,
    pub idempotency_key: String,
    pub conversation_id: String,
    pub attempted: bool,
    pub recallable: bool,
    pub recall_deadline_unix_nanoseconds: Option<u128>,
    pub remaining_seconds: u64,
    pub procedure: Vec<String>,
    pub checked_at_unix_nanoseconds: u128,
}

/// How strong the evidence linking a replica row to the attempt is. Recorded
/// verbatim in the report, because an image send cannot be matched as
/// precisely as a text send and the audit trail must say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SendMatchStrength {
    /// The message body's normalized digest matched exactly.
    BodyDigest,
    /// An outgoing attachment carrying the approved display name was found.
    AttachmentFileName,
    /// An outgoing attachment of the right kind was found in the window, but
    /// the client re-encoded it, so no name or digest could be compared. This
    /// is the weakest accepted evidence and is only ever used for an image
    /// send, where the single-flight outbox guarantees the adapter had no other
    /// attempt in the same window.
    AttachmentPresenceOnly,
    /// Nothing matched.
    None,
}

/// Where a reconciliation observation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SendObservationSource {
    EncryptedReplica,
    RestorationArchive,
}

/// Authoritative, body-free evidence about whether a message actually exists
/// in the account's own data. Only this can create `observedSent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SendReconciliationObservation {
    pub format_version: u32,
    pub idempotency_key: String,
    pub account_id: String,
    pub conversation_id: String,
    pub source: SendObservationSource,
    pub source_fingerprint: String,
    pub outgoing_message_found: bool,
    pub normalized_body_matched: bool,
    /// True when the matched row carries an artifact reference.
    #[serde(default)]
    pub attachment_reference_found: bool,
    /// True when the approved display name was read back from the replica.
    #[serde(default)]
    pub display_file_name_matched: bool,
    /// How strong the link between the row and the attempt is.
    #[serde(default = "no_match")]
    pub match_strength: SendMatchStrength,
    pub canonical_id: Option<String>,
    pub scanned_message_count: u64,
    pub observed_at_unix_nanoseconds: u128,
}

/// The outcome of one reconciliation pass over a parked attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SendReconciliationReport {
    pub format_version: u32,
    pub idempotency_key: String,
    pub resolved: bool,
    pub completion: Option<SendCompletionKind>,
    pub lifecycle_state: Option<ActionLifecycleState>,
    pub still_awaiting_reconciliation: bool,
    pub grace_elapsed: bool,
    pub observation: SendReconciliationObservation,
    pub outbox: SendOutboxSummary,
    pub resolved_at_unix_nanoseconds: u128,
}

/// The calibration section of the diagnostic report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CalibrationDiagnostics {
    pub calibration_profile_id: String,
    pub calibration_profile_sha256: String,
    pub trust_tier: SendTrustTier,
    pub bound_to_compatibility_matrix: bool,
}

/// `send doctor`: one answer to "why is send disabled or failing".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SendDoctorReport {
    pub format_version: u32,
    pub account_id: String,
    pub rollout_stage: SendRolloutStage,
    pub send_path_open: bool,
    pub kill_switch_engaged: bool,
    pub configuration_valid: bool,
    pub gate_evidence_complete: bool,
    pub blocking_failures: Vec<SendFailureCode>,
    pub operator_actions: Vec<String>,
    pub calibration: Option<CalibrationDiagnostics>,
    pub compatibility: Option<CompatibilityDecision>,
    pub helper: Option<HelperCapabilityStatus>,
    pub helper_failure: Option<SendFailureCode>,
    pub outbox: SendOutboxSummary,
    pub outbox_recovery: SendOutboxRecovery,
    pub generated_at_unix_nanoseconds: u128,
}

/// The bounded, watchdogged boundary to the input helper.
pub trait SendDispatcher {
    /// Read-only preflight: grants, client state, engine health.
    fn capability_status(
        &self,
        timeout: Duration,
    ) -> Result<HelperCapabilityStatus, SendFailureCode>;

    /// Locate and focus the search box, confirm by capture, never send.
    fn run_calibration_selftest(
        &self,
        profile: &SignedCalibrationProfile,
        timeout: Duration,
    ) -> Result<CalibrationSelfTestReport, SendFailureCode>;

    /// Run the mechanical send skill under one bound capability.
    fn execute_send(
        &self,
        capability: &ActionCapabilityEnvelope,
        timeout: Duration,
    ) -> Result<HelperSendOutcome, SendFailureCode>;
}

/// A dispatcher that runs the first-party helper client as a child process and
/// abandons it on timeout. The watchdog is why the observed two-minute engine
/// stall can never block a caller: the child is killed, the call returns
/// `engineStall`, and state is settled out of band.
#[derive(Debug, Clone)]
pub struct ProcessSendDispatcher {
    executable: PathBuf,
    arguments: Vec<String>,
    mach_service_name: String,
}

impl ProcessSendDispatcher {
    /// Validates the dispatcher executable before it can ever be run: a
    /// regular, non-symlink, non-group/world-writable, executable file.
    pub fn new(config: &SendHelperConfig) -> Result<Self, RestoreError> {
        let metadata = fs::symlink_metadata(&config.dispatcher_executable)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.permissions().mode() & 0o022 != 0
            || metadata.permissions().mode() & 0o111 == 0
            || (metadata.uid() != unsafe { libc::geteuid() } && metadata.uid() != 0)
        {
            return Err(RestoreError::Integrity(
                "send dispatcher must be an executable, non-symlink regular file that only its owner can write"
                    .to_string(),
            ));
        }
        Ok(Self {
            executable: config.dispatcher_executable.clone(),
            arguments: config.dispatcher_arguments.clone(),
            mach_service_name: config.mach_service_name.clone(),
        })
    }

    fn call<T: for<'de> Deserialize<'de>>(
        &self,
        subcommand: &str,
        request: &impl Serialize,
        timeout: Duration,
    ) -> Result<T, SendFailureCode> {
        let payload =
            serde_json::to_vec(request).map_err(|_| SendFailureCode::ConfigurationInvalid)?;
        let mut child = Command::new(&self.executable)
            .args(&self.arguments)
            .arg(subcommand)
            .arg("--mach-service")
            .arg(&self.mach_service_name)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| SendFailureCode::EngineUnavailable)?;
        let pid = child.id() as libc::pid_t;
        if let Some(mut stdin) = child.stdin.take() {
            // A failed write is reported by the child's exit status; the read
            // side below turns it into `engineUnavailable`.
            let _ = stdin.write_all(&payload);
            let _ = stdin.flush();
        }
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = child.stdout.take();
            let mut buffer = Vec::new();
            if let Some(stream) = stdout.as_mut() {
                let _ = stream
                    .take(MAXIMUM_HELPER_RESPONSE_BYTES + 1)
                    .read_to_end(&mut buffer);
            }
            let status = child.wait();
            let _ = sender.send((status, buffer));
        });
        match receiver.recv_timeout(timeout) {
            Ok((Ok(status), buffer)) => {
                if !status.success() || buffer.len() as u64 > MAXIMUM_HELPER_RESPONSE_BYTES {
                    return Err(SendFailureCode::EngineUnavailable);
                }
                serde_json::from_slice(&buffer).map_err(|_| SendFailureCode::EngineUnavailable)
            }
            Ok((Err(_), _)) => Err(SendFailureCode::EngineUnavailable),
            Err(_) => {
                // The watchdog fires: abandon the call, kill the child, and
                // never wait on it again.
                unsafe { libc::kill(pid, libc::SIGKILL) };
                Err(SendFailureCode::EngineStall)
            }
        }
    }
}

impl SendDispatcher for ProcessSendDispatcher {
    fn capability_status(
        &self,
        timeout: Duration,
    ) -> Result<HelperCapabilityStatus, SendFailureCode> {
        self.call("capability-status", &serde_json::json!({}), timeout)
    }

    fn run_calibration_selftest(
        &self,
        profile: &SignedCalibrationProfile,
        timeout: Duration,
    ) -> Result<CalibrationSelfTestReport, SendFailureCode> {
        self.call("calibration-selftest", profile, timeout)
    }

    fn execute_send(
        &self,
        capability: &ActionCapabilityEnvelope,
        timeout: Duration,
    ) -> Result<HelperSendOutcome, SendFailureCode> {
        self.call("execute-send", capability, timeout)
    }
}

/// The loaded, validated control plane.
#[derive(Debug, Clone)]
pub struct SendAdapter {
    config: SendAdapterConfig,
    trust_root: SendTrustRoot,
}

impl SendAdapter {
    /// Loads a configuration and its trust root. A development trust root is
    /// accepted only so a dry run can be exercised before a release key
    /// exists; it can never unlock a stage that presses Return.
    pub fn load(config_path: &Path) -> Result<Self, RestoreError> {
        let config = load_send_adapter_config(config_path)?;
        let trust_root = match config.development_trust_root_path.as_deref() {
            Some(path) => SendTrustRoot::load_development(path)?,
            None => SendTrustRoot::pinned().map_err(|_| {
                RestoreError::Integrity(
                    "pinned release trust root is malformed in this build".to_string(),
                )
            })?,
        };
        Ok(Self { config, trust_root })
    }

    /// The validated configuration.
    pub fn config(&self) -> &SendAdapterConfig {
        &self.config
    }

    fn verified_artifacts(
        &self,
        now_unix_nanoseconds: u128,
    ) -> Result<(VerifiedCalibrationProfile, VerifiedCompatibilityMatrix), RestoreError> {
        let now_seconds = (now_unix_nanoseconds / 1_000_000_000) as u64;
        let profile = load_calibration_profile(
            &self.config.calibration_profile_path,
            &self.trust_root,
            now_seconds,
        )?;
        let matrix = load_compatibility_matrix(
            &self.config.compatibility_matrix_path,
            &self.trust_root,
            now_seconds,
        )?;
        Ok((profile, matrix))
    }

    fn open_outbox(
        &self,
        now_unix_nanoseconds: u128,
    ) -> Result<(SendOutbox, SendOutboxRecovery), RestoreError> {
        let (outbox, recovery) = SendOutbox::open(
            &self.config.outbox_directory,
            &self.config.account_id,
            now_unix_nanoseconds,
        )?;
        let state = outbox.state(now_unix_nanoseconds)?;
        self.sweep_staging_root(&state);
        Ok((outbox, recovery))
    }

    /// The deterministic one-use idempotency key for an approved draft. Making
    /// it a function of the gate, draft, and approval means a retry of the very
    /// same approval reuses the key and is refused, rather than sending twice.
    pub fn idempotency_key(&self, draft_id: &str, approval_id: &str) -> String {
        digest(
            IDEMPOTENCY_DOMAIN,
            &[
                self.config.gate.gate_decision_id.as_str(),
                draft_id,
                approval_id,
            ],
        )
    }

    /// The action identity that every lifecycle event for this attempt shares.
    pub fn action_id(&self, draft_id: &str, approval_id: &str) -> String {
        digest(
            ACTION_IDENTITY_DOMAIN,
            &[
                self.config.account_id.as_str(),
                self.config.adapter.adapter_id.as_str(),
                self.config.gate.gate_decision_id.as_str(),
                draft_id,
                approval_id,
            ],
        )
    }

    /// The SHA-256 binding an external approval must repeat for this draft.
    /// Printed by the local approver so the owner can produce evidence without
    /// this process ever minting an approval on its own.
    pub fn expected_approval_binding(&self, draft: &ActionDraft) -> Option<String> {
        expected_approval_binding(
            &self.config.gate.gate_decision_id,
            &self.attempt_intent(draft, ""),
        )
    }

    /// The reviewed capability this draft exercises. Stated by the draft, never
    /// inferred from the attachment's type.
    pub fn draft_capability(draft: &ActionDraft) -> ActionCapability {
        draft
            .attachment_intent
            .unwrap_or(ActionCapability::TextSend)
    }

    fn attempt_intent(&self, draft: &ActionDraft, approval_id: &str) -> ActionAttemptIntent {
        ActionAttemptIntent {
            capability: Self::draft_capability(draft),
            draft_id: draft.draft_id.clone(),
            account_id: self.config.account_id.clone(),
            conversation_id: draft.conversation_id.clone(),
            adapter: self.config.adapter.clone(),
            idempotency_key: self.idempotency_key(&draft.draft_id, approval_id),
            approval: ExternalApprovalEvidence {
                approval_id: approval_id.to_string(),
                immutable_binding_sha256: String::new(),
                approver_id: String::new(),
                approved_at_unix_nanoseconds: 0,
                expires_at_unix_nanoseconds: 1,
            },
        }
    }

    /// The complete PRECHECK. Pure: every input is supplied, so the whole
    /// decision is reproducible in tests without a helper, a clock, or a disk.
    #[allow(clippy::too_many_arguments)]
    pub fn precheck(
        &self,
        draft: &ActionDraft,
        approval: &ExternalApprovalEvidence,
        profile: &VerifiedCalibrationProfile,
        compatibility: &CompatibilityDecision,
        helper_status: Option<&HelperCapabilityStatus>,
        outbox_state: &SendOutboxState,
        now_unix_nanoseconds: u128,
    ) -> SendPrecheckReport {
        let mut failures = BTreeSet::new();
        if let Err(failure) = self.config.validate() {
            failures.insert(failure);
        }
        if self.config.global_kill_switch_engaged || compatibility.field_kill_switch_engaged {
            failures.insert(SendFailureCode::KillSwitchEngaged);
        }
        let capability = Self::draft_capability(draft);
        let payload_valid = match capability {
            ActionCapability::TextSend => {
                !draft.rendered_text.is_empty() && draft.attachments.is_empty()
            }
            ActionCapability::ImageSend | ActionCapability::FileSend => {
                draft.rendered_text.is_empty() && draft.attachments.len() == 1
            }
            ActionCapability::ReplySend => false,
        };
        if draft.account_id != self.config.account_id
            || !payload_valid
            || hex::encode(Sha256::digest(draft.rendered_text.as_bytes()))
                != draft.rendered_text_sha256
            || draft.recipient.conversation_id != draft.conversation_id
            || draft.recipient.human_label.is_empty()
            || now_unix_nanoseconds >= draft.expires_at_unix_nanoseconds
        {
            failures.insert(SendFailureCode::DraftInvalid);
        }
        // An attachment needs a reviewed type and an active profile that knows
        // how to stage one; both are refused rather than guessed at.
        if capability.carries_attachment() {
            if let Some(attachment) = draft.attachments.first() {
                if reviewed_uniform_type_identifier(capability, &attachment.display_file_name)
                    .is_err()
                {
                    failures.insert(SendFailureCode::UnsupportedAttachmentType);
                }
                if !attachment.byte_count.is_some_and(|count| count > 0) {
                    failures.insert(SendFailureCode::AttachmentInvalid);
                }
            }
            if profile.profile.body.attachments.is_none() {
                failures.insert(SendFailureCode::ProfileInvalid);
            }
        }
        let idempotency_key = self.idempotency_key(&draft.draft_id, &approval.approval_id);
        let action_id = self.action_id(&draft.draft_id, &approval.approval_id);
        let mut intent = self.attempt_intent(draft, &approval.approval_id);
        intent.approval = approval.clone();
        let guard = assess_action_attempt(
            &self.guard_context(
                outbox_state,
                self.maximum_attempts_for(capability),
                now_unix_nanoseconds,
            ),
            &intent,
        );
        if !guard.permitted {
            failures.extend(guard_failures(&guard));
        }
        if let Some(failure) = outbox_state.admission_failure(now_unix_nanoseconds) {
            failures.insert(failure);
        }
        // Two independent build gates: the signed matrix must call this exact
        // (host x client) pair supported, and the signed profile must be the
        // one that pair names.
        if !compatibility.state.permits_send() || !compatibility.known_combination {
            failures.insert(SendFailureCode::UnknownBuild);
        }
        if bind_profile_to_compatibility(profile, compatibility, self.config.expected_macos_major)
            .is_err()
            || profile.profile.body.client_build_profile_id
                != self.config.adapter.client_build_profile_id
            || profile.profile.body.wechat_build != self.config.expected_wechat_build
        {
            failures.insert(SendFailureCode::ProfileInvalid);
        }
        match helper_status {
            None => {
                failures.insert(SendFailureCode::EngineUnavailable);
            }
            Some(status) => {
                if let Some(failure) = status.blocking_failure() {
                    failures.insert(failure);
                }
                if status.macos_build != self.config.expected_macos_build
                    || status.macos_major != self.config.expected_macos_major
                    || status.wechat_build != self.config.expected_wechat_build
                    || status.wechat_bundle_identifier
                        != profile.profile.body.wechat_bundle_identifier
                {
                    failures.insert(SendFailureCode::UnknownBuild);
                }
                if status.active_calibration_profile_id != profile.profile.body.profile_id {
                    failures.insert(SendFailureCode::CalibrationDrift);
                }
            }
        }
        // A development-signed profile is usable for rehearsal only.
        let stage_permits = self
            .config
            .stage_permits_capability(capability, &draft.conversation_id);
        let permit_send = stage_permits
            && self.config.rollout_stage.permits_return()
            && profile.trust_tier == SendTrustTier::Release;
        if self.config.rollout_stage.permits_return() && !stage_permits {
            failures.insert(SendFailureCode::StageNotPermitted);
        }
        if self.config.rollout_stage.permits_return()
            && profile.trust_tier != SendTrustTier::Release
        {
            failures.insert(SendFailureCode::ProfileInvalid);
        }
        let permitted = failures.is_empty();
        SendPrecheckReport {
            format_version: SEND_CONTRACT_VERSION,
            permitted,
            permit_send: permitted && permit_send,
            rollout_stage: self.config.rollout_stage,
            operator_actions: failures
                .iter()
                .map(|failure| failure.operator_action().to_string())
                .collect(),
            failures,
            guard_denials: guard.denials,
            account_id: self.config.account_id.clone(),
            conversation_id: draft.conversation_id.clone(),
            draft_id: draft.draft_id.clone(),
            approval_id: approval.approval_id.clone(),
            idempotency_key,
            action_id,
            calibration_profile_id: profile.profile.body.profile_id.clone(),
            calibration_trust_tier: profile.trust_tier,
            compatibility: compatibility.clone(),
            helper_status_observed: helper_status.is_some(),
            outbox: SendOutboxSummary::from_state(
                outbox_state,
                now_unix_nanoseconds,
                self.config.maximum_attempts_per_window,
            ),
            checked_at_unix_nanoseconds: now_unix_nanoseconds,
        }
    }

    /// The window capacity for one capability. Attachments get their own,
    /// tighter allowance because an attachment send is louder and less
    /// reversible than a line of text.
    fn maximum_attempts_for(&self, capability: ActionCapability) -> u64 {
        if capability.carries_attachment() {
            self.config
                .maximum_attachment_attempts_per_window
                .min(self.config.maximum_attempts_per_window)
        } else {
            self.config.maximum_attempts_per_window
        }
    }

    fn guard_context(
        &self,
        outbox_state: &SendOutboxState,
        maximum_attempts: u64,
        now_unix_nanoseconds: u128,
    ) -> ActionGuardContext {
        let window = outbox_state.rate_window.rolled(
            now_unix_nanoseconds,
            u128::from(self.config.attempt_window_seconds).saturating_mul(1_000_000_000),
            maximum_attempts,
        );
        ActionGuardContext {
            format_version: ACTION_SAFETY_CONTRACT_VERSION,
            now_unix_nanoseconds,
            global_kill_switch_engaged: self.config.global_kill_switch_engaged,
            gate: self.config.gate.clone(),
            required_adapter: self.config.adapter.clone(),
            allow_list: self.config.allow_list.clone(),
            rate: ActionRateState {
                window_started_at_unix_nanoseconds: window.started_at_unix_nanoseconds,
                window_ends_at_unix_nanoseconds: window.ends_at_unix_nanoseconds,
                maximum_attempts: window.maximum_attempts,
                reserved_attempts: window.reserved_attempts,
            },
            consumed_approval_ids: outbox_state.consumed_approval_ids.clone(),
            reserved_idempotency_keys: outbox_state.reserved_idempotency_keys.clone(),
        }
    }

    /// The PRECHECK a command-line caller runs: loads the signed artifacts and
    /// the durable outbox itself, then evaluates the same pure decision.
    pub fn precheck_from_disk(
        &self,
        draft: &ActionDraft,
        approval: &ExternalApprovalEvidence,
        dispatcher: Option<&dyn SendDispatcher>,
        now_unix_nanoseconds: u128,
    ) -> Result<SendPrecheckReport, RestoreError> {
        let (profile, matrix) = self.verified_artifacts(now_unix_nanoseconds)?;
        let compatibility = compatibility_decision(
            &matrix,
            &self.config.expected_macos_build,
            &self.config.expected_wechat_build,
        );
        let helper_status = dispatcher.and_then(|dispatcher| {
            dispatcher
                .capability_status(Duration::from_millis(
                    self.config.helper.status_timeout_milliseconds,
                ))
                .ok()
        });
        let (outbox, _) = self.open_outbox(now_unix_nanoseconds)?;
        let state = outbox.state(now_unix_nanoseconds)?;
        Ok(self.precheck(
            draft,
            approval,
            &profile,
            &compatibility,
            helper_status.as_ref(),
            &state,
            now_unix_nanoseconds,
        ))
    }

    /// Mints the single-use bound capability. It carries the recipient the
    /// control plane already resolved, so the helper enforces GATE 1 without
    /// the database, the keys, or any policy.
    pub fn mint_capability(
        &self,
        draft: &ActionDraft,
        approval: &ExternalApprovalEvidence,
        profile: &VerifiedCalibrationProfile,
        staged: Option<&StagedAttachment>,
        permit_send: bool,
        now_unix_nanoseconds: u128,
    ) -> Result<ActionCapabilityEnvelope, SendFailureCode> {
        let action_capability = Self::draft_capability(draft);
        // The staged attachment and the draft's stated intent must agree, or
        // nothing is minted: this is the seam where a file could otherwise be
        // substituted for the one that was approved.
        let attachment: Option<ActionAttachment> =
            match (action_capability.carries_attachment(), staged) {
                (false, None) => None,
                (true, Some(staged)) => {
                    let approved = draft
                        .attachments
                        .first()
                        .ok_or(SendFailureCode::AttachmentInvalid)?;
                    if staged.sha256 != approved.sha256
                        || staged.display_file_name != approved.display_file_name
                        || approved.byte_count != Some(staged.byte_count)
                        || staged.bytes_preserved_in_transit != action_capability.preserves_bytes()
                    {
                        return Err(SendFailureCode::AttachmentDigestMismatch);
                    }
                    Some(staged.as_action_attachment())
                }
                _ => return Err(SendFailureCode::AttachmentInvalid),
            };
        let idempotency_key = self.idempotency_key(&draft.draft_id, &approval.approval_id);
        let valid_until = now_unix_nanoseconds.saturating_add(
            u128::from(self.config.capability_validity_seconds).saturating_mul(1_000_000_000),
        );
        let mut capability = ActionCapabilityEnvelope {
            format_version: SEND_CONTRACT_VERSION,
            capability_id: digest(
                CAPABILITY_IDENTITY_DOMAIN,
                &[
                    self.action_id(&draft.draft_id, &approval.approval_id)
                        .as_str(),
                    idempotency_key.as_str(),
                    now_unix_nanoseconds.to_string().as_str(),
                ],
            ),
            action_id: self.action_id(&draft.draft_id, &approval.approval_id),
            draft_id: draft.draft_id.clone(),
            approval_id: approval.approval_id.clone(),
            idempotency_key,
            account_id: self.config.account_id.clone(),
            conversation_id: draft.conversation_id.clone(),
            capability: action_capability,
            search_key: self.config.search_key_for(draft),
            expected_title: draft.recipient.human_label.clone(),
            body_sha256: draft.rendered_text_sha256.clone(),
            normalized_body_sha256: normalized_send_text_sha256(&draft.rendered_text),
            body: draft.rendered_text.clone(),
            client_build_profile_id: self.config.adapter.client_build_profile_id.clone(),
            calibration_profile_id: profile.profile.body.profile_id.clone(),
            calibration_profile_sha256: profile.canonical_sha256.clone(),
            attachment,
            rollout_stage: self.config.rollout_stage,
            permit_send,
            issued_at_unix_nanoseconds: now_unix_nanoseconds,
            valid_until_unix_nanoseconds: valid_until,
            binding_sha256: String::new(),
        };
        capability.binding_sha256 =
            capability_binding_sha256(&capability).ok_or(SendFailureCode::CapabilityMismatch)?;
        capability.validate(now_unix_nanoseconds)?;
        Ok(capability)
    }

    /// PRECHECK, reserve, dispatch, settle, audit. This is the only path that
    /// can reach the helper, and every denial happens before it.
    pub fn execute(
        &self,
        draft: &ActionDraft,
        approval: &ExternalApprovalEvidence,
        dispatcher: &dyn SendDispatcher,
        attachment_source: Option<&Path>,
        now_unix_nanoseconds: u128,
    ) -> Result<SendAttemptReport, RestoreError> {
        let (profile, matrix) = self.verified_artifacts(now_unix_nanoseconds)?;
        let compatibility = compatibility_decision(
            &matrix,
            &self.config.expected_macos_build,
            &self.config.expected_wechat_build,
        );
        self.execute_with_artifacts(
            draft,
            approval,
            &profile,
            &compatibility,
            dispatcher,
            attachment_source,
            now_unix_nanoseconds,
        )
    }

    /// The orchestration proper, with the signed artifacts already verified.
    /// Kept crate-visible so the unit tests can exercise a release-tier
    /// profile without a release key being pinned into the test binary.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn execute_with_artifacts(
        &self,
        draft: &ActionDraft,
        approval: &ExternalApprovalEvidence,
        profile: &VerifiedCalibrationProfile,
        compatibility: &CompatibilityDecision,
        dispatcher: &dyn SendDispatcher,
        attachment_source: Option<&Path>,
        now_unix_nanoseconds: u128,
    ) -> Result<SendAttemptReport, RestoreError> {
        let helper_status = dispatcher
            .capability_status(Duration::from_millis(
                self.config.helper.status_timeout_milliseconds,
            ))
            .ok();
        let (outbox, _) = self.open_outbox(now_unix_nanoseconds)?;
        let outbox_state = outbox.state(now_unix_nanoseconds)?;
        let precheck = self.precheck(
            draft,
            approval,
            profile,
            compatibility,
            helper_status.as_ref(),
            &outbox_state,
            now_unix_nanoseconds,
        );
        let mut audit_event_count = 0_u64;
        if !precheck.permitted {
            self.append_audit(
                draft,
                &precheck.action_id,
                ConnectorAuditStage::ApprovalRecorded,
                ConnectorAuditOutcome::Denied,
                operation_name(precheck.permit_send),
                0,
                now_unix_nanoseconds,
            )?;
            return Ok(SendAttemptReport {
                format_version: SEND_CONTRACT_VERSION,
                dispatched: false,
                attempted: false,
                visual_confirmation: VisualConfirmation::NotAttempted,
                completion: None,
                awaiting_reconciliation: false,
                lifecycle_state: None,
                stage_reached: Some(SendStage::Precheck),
                failure: precheck.failures.iter().next().copied(),
                operator_action: precheck
                    .failures
                    .iter()
                    .next()
                    .map(|failure| failure.operator_action().to_string()),
                evidence: None,
                capability_id: String::new(),
                idempotency_key: precheck.idempotency_key.clone(),
                audit_event_count: 1,
                recall_deadline_unix_nanoseconds: None,
                completed_at_unix_nanoseconds: now_unix_nanoseconds,
                precheck,
            });
        }

        // Staging happens only after PRECHECK has passed, and before anything
        // is minted or reserved: a refusal here has touched no client state and
        // consumed no capacity.
        let staged = match Self::draft_capability(draft).carries_attachment() {
            false => None,
            true => {
                let source = attachment_source
                    .ok_or_else(|| failure_error(SendFailureCode::AttachmentStagingFailed))?;
                let approved = draft
                    .attachments
                    .first()
                    .ok_or_else(|| failure_error(SendFailureCode::AttachmentInvalid))?;
                match stage_attachment(
                    source,
                    &self.config.staging_root,
                    approved,
                    Self::draft_capability(draft),
                ) {
                    Ok(staged) => Some(staged),
                    Err(error) => {
                        self.append_audit(
                            draft,
                            &precheck.action_id,
                            ConnectorAuditStage::ApprovalRecorded,
                            ConnectorAuditOutcome::Denied,
                            operation_name(precheck.permit_send),
                            0,
                            now_unix_nanoseconds,
                        )?;
                        return Err(error);
                    }
                }
            }
        };
        let capability = self
            .mint_capability(
                draft,
                approval,
                profile,
                staged.as_ref(),
                precheck.permit_send,
                now_unix_nanoseconds,
            )
            .inspect_err(|_| self.discard_staged(staged.as_ref()))
            .map_err(failure_error)?;
        let entry = OutboxEntry {
            action_id: capability.action_id.clone(),
            draft_id: capability.draft_id.clone(),
            approval_id: capability.approval_id.clone(),
            idempotency_key: capability.idempotency_key.clone(),
            capability_id: capability.capability_id.clone(),
            capability_binding_sha256: capability.binding_sha256.clone(),
            account_id: capability.account_id.clone(),
            conversation_id: capability.conversation_id.clone(),
            body_sha256: capability.body_sha256.clone(),
            normalized_body_sha256: capability.normalized_body_sha256.clone(),
            capability: capability.capability,
            attachment_sha256: capability
                .attachment
                .as_ref()
                .map(|attachment| attachment.sha256.clone()),
            display_file_name: capability
                .attachment
                .as_ref()
                .map(|attachment| attachment.display_file_name.clone()),
            staging_directory: capability
                .attachment
                .as_ref()
                .map(|attachment| attachment.staging_directory.clone()),
            bytes_preserved_in_transit: capability.capability.preserves_bytes(),
            rollout_stage: capability.rollout_stage,
            permit_send: capability.permit_send,
            state: OutboxEntryState::Reserved,
            reserved_at_unix_nanoseconds: now_unix_nanoseconds,
            attempted_at_unix_nanoseconds: None,
            deadline_unix_nanoseconds: capability.valid_until_unix_nanoseconds,
        };
        let window_nanoseconds =
            u128::from(self.config.attempt_window_seconds).saturating_mul(1_000_000_000);
        let maximum_attempts = self.config.maximum_attempts_per_window;
        outbox
            .transaction(
                |state| {
                    state
                        .reserve(
                            entry,
                            now_unix_nanoseconds,
                            window_nanoseconds,
                            maximum_attempts,
                        )
                        .map_err(failure_error)
                },
                now_unix_nanoseconds,
            )
            // The reservation is the last synchronous gate: a refusal here is
            // authoritative, and nothing has been dispatched.
            ?;
        self.append_audit(
            draft,
            &capability.action_id,
            ConnectorAuditStage::ApprovalRecorded,
            ConnectorAuditOutcome::Completed,
            operation_name(capability.permit_send),
            draft.rendered_text.len(),
            now_unix_nanoseconds,
        )?;
        audit_event_count += 1;

        if capability.permit_send {
            let key = capability.idempotency_key.clone();
            outbox.transaction(
                |state| {
                    state
                        .mark_attempted(&key, now_unix_nanoseconds)
                        .map_err(failure_error)
                },
                now_unix_nanoseconds,
            )?;
        }
        self.append_audit(
            draft,
            &capability.action_id,
            ConnectorAuditStage::AttemptRecorded,
            ConnectorAuditOutcome::Completed,
            operation_name(capability.permit_send),
            draft.rendered_text.len(),
            now_unix_nanoseconds,
        )?;
        audit_event_count += 1;

        // One logical clock for the whole attempt: the caller's `now` advanced
        // by the measured dispatch duration. Mixing in a second reading of the
        // wall clock would make the outbox's own timestamps disagree with the
        // decision that produced them.
        let dispatch_started = Instant::now();
        let dispatched = dispatcher.execute_send(
            &capability,
            Duration::from_millis(self.config.helper.send_timeout_milliseconds),
        );
        let settled_at = now_unix_nanoseconds.saturating_add(dispatch_started.elapsed().as_nanos());
        let (record, evidence, stage_reached) = settle_dispatch(&capability, dispatched);
        let key = capability.idempotency_key.clone();
        let completion_record = record;
        outbox.transaction(
            |state| {
                state
                    .complete(
                        &key,
                        completion_record,
                        settled_at,
                        self.config.circuit_breaker_failure_threshold,
                        u128::from(self.config.circuit_breaker_cooldown_seconds)
                            .saturating_mul(1_000_000_000),
                    )
                    .map_err(failure_error)
            },
            settled_at,
        )?;
        // The staged copy is kept while an attempt is unreconciled, because the
        // client may still be reading it; a terminal outcome retires it.
        if matches!(record.outcome, SendCompletionOutcome::Completed(_)) {
            self.discard_staged(staged.as_ref());
        }
        self.append_audit(
            draft,
            &capability.action_id,
            ConnectorAuditStage::ReconciliationRecorded,
            match record.outcome {
                SendCompletionOutcome::Completed(kind) if kind.healthy() => {
                    ConnectorAuditOutcome::Completed
                }
                _ => ConnectorAuditOutcome::Denied,
            },
            operation_name(capability.permit_send),
            0,
            settled_at,
        )?;
        audit_event_count += 1;

        let completion = match record.outcome {
            SendCompletionOutcome::Completed(kind) => Some(kind),
            SendCompletionOutcome::AwaitingReconciliation => None,
        };
        Ok(SendAttemptReport {
            format_version: SEND_CONTRACT_VERSION,
            dispatched: true,
            attempted: record.attempted,
            visual_confirmation: record.visual_confirmation,
            completion,
            awaiting_reconciliation: completion.is_none(),
            lifecycle_state: completion.and_then(SendCompletionKind::lifecycle_state),
            stage_reached,
            failure: record.failure,
            operator_action: record
                .failure
                .map(|failure| failure.operator_action().to_string()),
            evidence,
            capability_id: capability.capability_id.clone(),
            idempotency_key: capability.idempotency_key.clone(),
            audit_event_count,
            recall_deadline_unix_nanoseconds: record.attempted.then(|| {
                settled_at.saturating_add(
                    u128::from(self.config.recall_window_seconds).saturating_mul(1_000_000_000),
                )
            }),
            completed_at_unix_nanoseconds: settled_at,
            precheck,
        })
    }

    /// Resolves one parked attempt from authoritative observation. Nothing
    /// here can dispatch; reconciliation only reads and decides.
    pub fn reconcile(
        &self,
        observation: &SendReconciliationObservation,
        draft: Option<&ActionDraft>,
        now_unix_nanoseconds: u128,
    ) -> Result<SendReconciliationReport, RestoreError> {
        let (outbox, _) = self.open_outbox(now_unix_nanoseconds)?;
        let state = outbox.state(now_unix_nanoseconds)?;
        let entry = state
            .pending_reconciliation
            .iter()
            .find(|entry| entry.idempotency_key == observation.idempotency_key)
            .cloned()
            .ok_or_else(|| {
                RestoreError::Integrity(
                    "no parked attempt matches this idempotency key".to_string(),
                )
            })?;
        if observation.format_version != SEND_CONTRACT_VERSION
            || observation.account_id != self.config.account_id
            || observation.conversation_id != entry.conversation_id
        {
            return Err(RestoreError::Integrity(
                "reconciliation observation does not belong to the parked attempt".to_string(),
            ));
        }
        let attempted_at = entry
            .attempted_at_unix_nanoseconds
            .unwrap_or(entry.reserved_at_unix_nanoseconds);
        let grace_nanoseconds =
            u128::from(self.config.reconciliation_grace_seconds).saturating_mul(1_000_000_000);
        let grace_elapsed = now_unix_nanoseconds.saturating_sub(attempted_at) >= grace_nanoseconds;
        // What counts as proof depends on what the client preserved. A text
        // send must match its digest; a file send must match its name; an image
        // send can only be matched by presence, because the client re-encoded
        // it, and that weaker evidence is recorded as such.
        let matched = match observation.match_strength {
            SendMatchStrength::BodyDigest => {
                !entry.capability.carries_attachment() && observation.normalized_body_matched
            }
            SendMatchStrength::AttachmentFileName => {
                entry.capability.carries_attachment()
                    && observation.attachment_reference_found
                    && observation.display_file_name_matched
            }
            SendMatchStrength::AttachmentPresenceOnly => {
                !entry.bytes_preserved_in_transit && observation.attachment_reference_found
            }
            SendMatchStrength::None => false,
        };
        let kind = if observation.outgoing_message_found && matched {
            Some(SendCompletionKind::ObservedSent)
        } else if grace_elapsed {
            Some(SendCompletionKind::ObservedFailed)
        } else {
            None
        };
        if let Some(kind) = kind {
            // A resolved attempt retires its staged copy; nothing needs it now.
            if let Some(directory) = entry.staging_directory.as_ref() {
                discard_staging_directory(Path::new(directory), &self.config.staging_root);
            }
            let key = observation.idempotency_key.clone();
            outbox.transaction(
                |state| {
                    state
                        .resolve_pending(&key, kind, true, now_unix_nanoseconds)
                        .map_err(failure_error)
                },
                now_unix_nanoseconds,
            )?;
            if let Some(draft) = draft {
                self.append_audit(
                    draft,
                    &entry.action_id,
                    ConnectorAuditStage::ReconciliationRecorded,
                    match kind {
                        SendCompletionKind::ObservedSent => ConnectorAuditOutcome::Completed,
                        _ => ConnectorAuditOutcome::Denied,
                    },
                    RECONCILE_OPERATION,
                    0,
                    now_unix_nanoseconds,
                )?;
            }
        }
        let state = outbox.state(now_unix_nanoseconds)?;
        Ok(SendReconciliationReport {
            format_version: SEND_CONTRACT_VERSION,
            idempotency_key: observation.idempotency_key.clone(),
            resolved: kind.is_some(),
            completion: kind,
            lifecycle_state: kind.and_then(SendCompletionKind::lifecycle_state),
            still_awaiting_reconciliation: kind.is_none(),
            grace_elapsed,
            observation: observation.clone(),
            outbox: SendOutboxSummary::from_state(
                &state,
                now_unix_nanoseconds,
                self.config.maximum_attempts_per_window,
            ),
            resolved_at_unix_nanoseconds: now_unix_nanoseconds,
        })
    }

    /// The one command that answers "why is send disabled or failing".
    pub fn doctor(
        &self,
        dispatcher: Option<&dyn SendDispatcher>,
        now_unix_nanoseconds: u128,
    ) -> Result<SendDoctorReport, RestoreError> {
        let mut blocking = Vec::new();
        let configuration_valid = self.config.validate().is_ok();
        if !configuration_valid {
            blocking.push(SendFailureCode::ConfigurationInvalid);
        }
        if self.config.global_kill_switch_engaged {
            blocking.push(SendFailureCode::KillSwitchEngaged);
        }
        let gate_evidence_complete = self.config.gate.acquisition_gate_passed
            && self.config.gate.restoration_gate_passed
            && self.config.gate.mechanism_approved
            && self.config.gate.legal_review_approved;
        if !gate_evidence_complete {
            blocking.push(SendFailureCode::StageNotPermitted);
        }
        let artifacts = self.verified_artifacts(now_unix_nanoseconds).ok();
        let (calibration, compatibility) = match artifacts.as_ref() {
            None => {
                blocking.push(SendFailureCode::ProfileInvalid);
                (None, None)
            }
            Some((profile, matrix)) => {
                let decision = compatibility_decision(
                    matrix,
                    &self.config.expected_macos_build,
                    &self.config.expected_wechat_build,
                );
                let bound = bind_profile_to_compatibility(
                    profile,
                    &decision,
                    self.config.expected_macos_major,
                )
                .is_ok();
                if !decision.state.permits_send() {
                    blocking.push(SendFailureCode::UnknownBuild);
                }
                if !bound {
                    blocking.push(SendFailureCode::ProfileInvalid);
                }
                (
                    Some(CalibrationDiagnostics {
                        calibration_profile_id: profile.profile.body.profile_id.clone(),
                        calibration_profile_sha256: profile.canonical_sha256.clone(),
                        trust_tier: profile.trust_tier,
                        bound_to_compatibility_matrix: bound,
                    }),
                    Some(decision),
                )
            }
        };
        let (helper, helper_failure) = match dispatcher {
            None => (None, Some(SendFailureCode::EngineUnavailable)),
            Some(dispatcher) => match dispatcher.capability_status(Duration::from_millis(
                self.config.helper.status_timeout_milliseconds,
            )) {
                Err(failure) => (None, Some(failure)),
                Ok(status) => {
                    let failure = status.blocking_failure();
                    (Some(status), failure)
                }
            },
        };
        if let Some(failure) = helper_failure {
            blocking.push(failure);
        }
        let (outbox, outbox_recovery) = self.open_outbox(now_unix_nanoseconds)?;
        let state = outbox.state(now_unix_nanoseconds)?;
        if let Some(failure) = state.admission_failure(now_unix_nanoseconds) {
            blocking.push(failure);
        }
        blocking.sort();
        blocking.dedup();
        Ok(SendDoctorReport {
            format_version: SEND_CONTRACT_VERSION,
            account_id: self.config.account_id.clone(),
            rollout_stage: self.config.rollout_stage,
            send_path_open: blocking.is_empty(),
            kill_switch_engaged: self.config.global_kill_switch_engaged,
            configuration_valid,
            gate_evidence_complete,
            operator_actions: blocking
                .iter()
                .map(|failure| failure.operator_action().to_string())
                .collect(),
            blocking_failures: blocking,
            calibration,
            compatibility,
            helper,
            helper_failure,
            outbox: SendOutboxSummary::from_state(
                &state,
                now_unix_nanoseconds,
                self.config.maximum_attempts_per_window,
            ),
            outbox_recovery,
            generated_at_unix_nanoseconds: now_unix_nanoseconds,
        })
    }

    /// Runs one no-send calibration self-test through the helper.
    pub fn calibration_selftest(
        &self,
        dispatcher: &dyn SendDispatcher,
        now_unix_nanoseconds: u128,
    ) -> Result<CalibrationSelfTestReport, RestoreError> {
        let (profile, _) = self.verified_artifacts(now_unix_nanoseconds)?;
        dispatcher
            .run_calibration_selftest(
                &profile.profile,
                Duration::from_millis(self.config.helper.selftest_timeout_milliseconds),
            )
            .map_err(failure_error)
    }

    /// Reports whether a dispatched attempt is still inside the client's
    /// recall window, and the exact steps to recall it.
    pub fn recall_window(
        &self,
        idempotency_key: &str,
        now_unix_nanoseconds: u128,
    ) -> Result<SendRecallWindowReport, RestoreError> {
        let (outbox, _) = self.open_outbox(now_unix_nanoseconds)?;
        let state = outbox.state(now_unix_nanoseconds)?;
        let parked = state
            .pending_reconciliation
            .iter()
            .find(|entry| entry.idempotency_key == idempotency_key)
            .map(|entry| {
                (
                    entry.conversation_id.clone(),
                    entry.attempted_at_unix_nanoseconds,
                )
            });
        let completed = state
            .completions
            .iter()
            .rev()
            .find(|completion| completion.idempotency_key == idempotency_key)
            .map(|completion| {
                (
                    completion.conversation_id.clone(),
                    completion
                        .attempted
                        .then_some(completion.completed_at_unix_nanoseconds),
                )
            });
        let (conversation_id, attempted_at) = parked.or(completed).ok_or_else(|| {
            RestoreError::Integrity("no attempt matches this idempotency key".to_string())
        })?;
        let deadline = attempted_at.map(|at| {
            at.saturating_add(
                u128::from(self.config.recall_window_seconds).saturating_mul(1_000_000_000),
            )
        });
        let remaining_seconds = deadline
            .filter(|deadline| *deadline > now_unix_nanoseconds)
            .map(|deadline| ((deadline - now_unix_nanoseconds) / 1_000_000_000) as u64)
            .unwrap_or_default();
        Ok(SendRecallWindowReport {
            format_version: SEND_CONTRACT_VERSION,
            idempotency_key: idempotency_key.to_string(),
            conversation_id,
            attempted: attempted_at.is_some(),
            recallable: remaining_seconds > 0,
            recall_deadline_unix_nanoseconds: deadline,
            remaining_seconds,
            procedure: vec![
                "Open WeChat and go to the conversation named in this report.".to_string(),
                "Right-click the message the adapter sent.".to_string(),
                "Choose Recall, then confirm.".to_string(),
                "Re-run `send reconcile` so the outbox records the final state.".to_string(),
            ],
            checked_at_unix_nanoseconds: now_unix_nanoseconds,
        })
    }

    /// Retires one staged attachment, if there was one.
    fn discard_staged(&self, staged: Option<&StagedAttachment>) {
        if let Some(staged) = staged {
            discard_staging_directory(&staged.staging_directory, &self.config.staging_root);
        }
    }

    /// Removes every staging directory no live outbox entry still refers to.
    /// Running this after recovery means a crash mid-attempt cannot leave a
    /// copy of the owner's file lying around.
    fn sweep_staging_root(&self, state: &SendOutboxState) {
        let live = state
            .in_flight
            .iter()
            .chain(&state.pending_reconciliation)
            .filter_map(|entry| entry.staging_directory.clone())
            .collect::<BTreeSet<_>>();
        let Ok(entries) = fs::read_dir(&self.config.staging_root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !live.contains(&path.display().to_string()) {
                discard_staging_directory(&path, &self.config.staging_root);
            }
        }
    }

    /// The current outbox projection, after running recovery.
    pub fn outbox_status(
        &self,
        now_unix_nanoseconds: u128,
    ) -> Result<(SendOutboxSummary, SendOutboxRecovery, Vec<OutboxEntry>), RestoreError> {
        let (outbox, recovery) = self.open_outbox(now_unix_nanoseconds)?;
        let state = outbox.state(now_unix_nanoseconds)?;
        Ok((
            SendOutboxSummary::from_state(
                &state,
                now_unix_nanoseconds,
                self.config.maximum_attempts_per_window,
            ),
            recovery,
            state.pending_reconciliation.clone(),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn append_audit(
        &self,
        draft: &ActionDraft,
        action_id: &str,
        stage: ConnectorAuditStage,
        outcome: ConnectorAuditOutcome,
        operation: &str,
        request_body_byte_count: usize,
        observed_at_unix_nanoseconds: u128,
    ) -> Result<(), RestoreError> {
        if self.config.audit_log_path.try_exists()? {
            // Verify the existing chain before extending it.
            audit_connector_log(&self.config.audit_log_path)?;
        }
        let event = ConnectorAuditEvent {
            format_version: 2,
            event_id: digest(
                AUDIT_EVENT_DOMAIN,
                &[
                    action_id,
                    stage_name(stage),
                    operation,
                    observed_at_unix_nanoseconds.to_string().as_str(),
                    match outcome {
                        ConnectorAuditOutcome::Completed => "completed",
                        ConnectorAuditOutcome::Denied => "denied",
                    },
                ],
            ),
            observed_at_unix_nanoseconds,
            account_id: self.config.account_id.clone(),
            requester_id: draft.requester_id.clone(),
            request_id: action_id.to_string(),
            operation: operation.to_string(),
            stage,
            conversation_id: Some(draft.conversation_id.clone()),
            destination: ConnectorDestination::Local,
            outcome,
            returned_item_count: 0,
            released_body_byte_count: 0,
            request_body_byte_count,
            draft_id: Some(draft.draft_id.clone()),
            policy_decision_id: Some(draft.policy_decision_id.clone()),
            previous_event_sha256: None,
            event_sha256: String::new(),
        };
        append_owner_only_connector_event(&self.config.audit_log_path, event)
    }
}

/// Looks for the sent message in the account's own encrypted replica. This is
/// the authoritative half of GATE 3: the helper can only report what a capture
/// showed, while this reads what WeChat actually wrote. It is read-only,
/// bounded, and releases no message text — only a match decision.
pub fn observe_send_in_replica(
    replica_path: &Path,
    key: &ReplicaKey,
    entry: &OutboxEntry,
    lookback_seconds: i64,
    now_unix_nanoseconds: u128,
) -> Result<SendReconciliationObservation, RestoreError> {
    let attempted_at_nanoseconds = entry
        .attempted_at_unix_nanoseconds
        .unwrap_or(entry.reserved_at_unix_nanoseconds);
    let attempted_at_seconds = (attempted_at_nanoseconds / 1_000_000_000) as i64;
    let filter = ReplicaMessageFilter {
        conversation_id: Some(entry.conversation_id.clone()),
        direction: Some(MessageDirection::Outgoing),
        not_before_unix: Some(attempted_at_seconds.saturating_sub(lookback_seconds.max(0))),
        ..ReplicaMessageFilter::default()
    };
    let mut cursor: Option<String> = None;
    let mut scanned = 0_u64;
    let mut source_fingerprint = String::new();
    let mut matched: Option<String> = None;
    let mut match_strength = SendMatchStrength::None;
    let mut attachment_reference_found = false;
    let mut display_file_name_matched = false;
    let mut any_outgoing = false;
    for _ in 0..MAXIMUM_RECONCILIATION_PAGES {
        let page = search_replica_messages(
            replica_path,
            key,
            &filter,
            cursor.as_deref(),
            RECONCILIATION_PAGE_SIZE,
        )?;
        if page.account_id != entry.account_id {
            return Err(RestoreError::Integrity(
                "replica belongs to a different account than the parked attempt".to_string(),
            ));
        }
        source_fingerprint = page.source_fingerprint.clone();
        for message in &page.items {
            scanned = scanned.saturating_add(1);
            any_outgoing = true;
            let summary = match &message.typed_payload {
                TypedPayload::Decoded(value) => {
                    let (_, summary, truncated) =
                        summarize_decoded_payload(value, RECONCILIATION_SUMMARY_BYTES);
                    summary.filter(|_| !truncated)
                }
                TypedPayload::Unknown { .. } => None,
            };
            if entry.capability.carries_attachment() {
                // An attachment send is matched through the artifact the client
                // actually recorded, never through message text.
                if message.artifact_references.is_empty() {
                    continue;
                }
                attachment_reference_found = true;
                let named = entry.display_file_name.as_ref().is_some_and(|name| {
                    summary.as_ref().is_some_and(|summary| {
                        summary.to_lowercase().contains(&name.to_lowercase())
                    })
                });
                if named {
                    display_file_name_matched = true;
                    match_strength = SendMatchStrength::AttachmentFileName;
                    matched = Some(message.canonical_id.clone());
                    break;
                }
                // The client re-encodes an image, so its name does not survive.
                // Presence of an outgoing artifact inside the attempt window is
                // then the strongest available evidence, and it is labelled so.
                if !entry.bytes_preserved_in_transit
                    && matches!(match_strength, SendMatchStrength::None)
                {
                    match_strength = SendMatchStrength::AttachmentPresenceOnly;
                    matched = Some(message.canonical_id.clone());
                }
                continue;
            }
            let Some(summary) = summary else {
                continue;
            };
            if normalized_send_text_sha256(&summary) == entry.normalized_body_sha256 {
                matched = Some(message.canonical_id.clone());
                match_strength = SendMatchStrength::BodyDigest;
                break;
            }
        }
        if matches!(
            match_strength,
            SendMatchStrength::BodyDigest | SendMatchStrength::AttachmentFileName
        ) {
            break;
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(SendReconciliationObservation {
        format_version: SEND_CONTRACT_VERSION,
        idempotency_key: entry.idempotency_key.clone(),
        account_id: entry.account_id.clone(),
        conversation_id: entry.conversation_id.clone(),
        source: SendObservationSource::EncryptedReplica,
        source_fingerprint,
        outgoing_message_found: any_outgoing,
        normalized_body_matched: match_strength == SendMatchStrength::BodyDigest,
        attachment_reference_found,
        display_file_name_matched,
        match_strength,
        canonical_id: matched,
        scanned_message_count: scanned,
        observed_at_unix_nanoseconds: now_unix_nanoseconds,
    })
}

/// Maps a dispatch result onto a durable completion. An attempted send never
/// becomes `observedSent` here: it is parked for replica reconciliation.
fn settle_dispatch(
    capability: &ActionCapabilityEnvelope,
    dispatched: Result<HelperSendOutcome, SendFailureCode>,
) -> (
    SendCompletionRecord,
    Option<HelperGateEvidence>,
    Option<SendStage>,
) {
    match dispatched {
        Err(failure) => {
            // A stall or an unreachable helper after a send-permitting
            // dispatch is inconclusive by construction: Return may have
            // landed, so the entry is parked rather than closed.
            let outcome = if capability.permit_send {
                SendCompletionOutcome::AwaitingReconciliation
            } else {
                SendCompletionOutcome::Completed(SendCompletionKind::ObservedFailed)
            };
            (
                SendCompletionRecord {
                    outcome,
                    attempted: false,
                    visual_confirmation: VisualConfirmation::NotAttempted,
                    failure: Some(failure),
                    reconciled_by_replica: false,
                },
                None,
                None,
            )
        }
        Ok(outcome) => {
            if let Err(failure) = outcome.validate_against(capability) {
                let settled = if capability.permit_send {
                    SendCompletionOutcome::AwaitingReconciliation
                } else {
                    SendCompletionOutcome::Completed(SendCompletionKind::ObservedFailed)
                };
                return (
                    SendCompletionRecord {
                        outcome: settled,
                        attempted: false,
                        visual_confirmation: VisualConfirmation::NotAttempted,
                        failure: Some(failure),
                        reconciled_by_replica: false,
                    },
                    Some(outcome.evidence),
                    Some(outcome.stage_reached),
                );
            }
            let settled = if outcome.attempted {
                SendCompletionOutcome::AwaitingReconciliation
            } else if outcome.failure.is_some() {
                SendCompletionOutcome::Completed(SendCompletionKind::ObservedFailed)
            } else {
                SendCompletionOutcome::Completed(SendCompletionKind::DryRunCompleted)
            };
            (
                SendCompletionRecord {
                    outcome: settled,
                    attempted: outcome.attempted,
                    visual_confirmation: outcome.visual_confirmation,
                    failure: outcome.failure,
                    reconciled_by_replica: false,
                },
                Some(outcome.evidence.clone()),
                Some(outcome.stage_reached),
            )
        }
    }
}

fn guard_failures(decision: &ActionGuardDecision) -> BTreeSet<SendFailureCode> {
    decision
        .denials
        .iter()
        .map(|denial| match denial {
            ActionGuardDenial::KillSwitchEngaged => SendFailureCode::KillSwitchEngaged,
            ActionGuardDenial::ClientBuildMismatch => SendFailureCode::UnknownBuild,
            ActionGuardDenial::IdempotencyKeyAlreadyReserved
            | ActionGuardDenial::IdempotencyKeyMalformed => SendFailureCode::IdempotencyConflict,
            ActionGuardDenial::RateLimitExceeded | ActionGuardDenial::RateLimitInvalid => {
                SendFailureCode::RateLimited
            }
            ActionGuardDenial::ApprovalMalformed
            | ActionGuardDenial::ApprovalBindingMismatch
            | ActionGuardDenial::ApprovalNotYetValid
            | ActionGuardDenial::ApprovalExpired
            | ActionGuardDenial::ApprovalAlreadyConsumed => SendFailureCode::ApprovalInvalid,
            ActionGuardDenial::AccountNotAllowed
            | ActionGuardDenial::ConversationNotAllowed
            | ActionGuardDenial::CapabilityNotAllowed
            | ActionGuardDenial::AcquisitionGateNotPassed
            | ActionGuardDenial::RestorationGateNotPassed
            | ActionGuardDenial::MechanismNotApproved
            | ActionGuardDenial::LegalReviewNotApproved => SendFailureCode::StageNotPermitted,
            ActionGuardDenial::InvalidContract | ActionGuardDenial::AdapterMismatch => {
                SendFailureCode::ConfigurationInvalid
            }
        })
        .collect()
}

/// Attachments start closed even when text sending is open.
fn dry_run_stage() -> SendRolloutStage {
    SendRolloutStage::DryRun
}

/// One attachment per window unless the owner deliberately widens it.
fn one_attempt() -> u64 {
    1
}

/// Absent evidence is no evidence.
fn no_match() -> SendMatchStrength {
    SendMatchStrength::None
}

fn operation_name(permit_send: bool) -> &'static str {
    if permit_send {
        SEND_OPERATION
    } else {
        DRY_RUN_OPERATION
    }
}

fn stage_name(stage: ConnectorAuditStage) -> &'static str {
    match stage {
        ConnectorAuditStage::Request => "request",
        ConnectorAuditStage::DraftRequested => "draftRequested",
        ConnectorAuditStage::DraftReviewed => "draftReviewed",
        ConnectorAuditStage::ApprovalRecorded => "approvalRecorded",
        ConnectorAuditStage::AttemptRecorded => "attemptRecorded",
        ConnectorAuditStage::ReconciliationRecorded => "reconciliationRecorded",
    }
}

fn digest(domain: &str, fields: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    for field in fields {
        hasher.update(field.as_bytes());
        hasher.update([0]);
    }
    hex::encode(hasher.finalize())
}

fn failure_error(failure: SendFailureCode) -> RestoreError {
    RestoreError::Integrity(format!(
        "send adapter refused the request: {} ({})",
        serde_json::to_string(&failure).unwrap_or_else(|_| "\"unknown\"".to_string()),
        failure.operator_action()
    ))
}

/// The current wall clock in Unix nanoseconds.
pub fn unix_nanoseconds() -> Result<u128, RestoreError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(|_| RestoreError::Integrity("system clock predates Unix epoch".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use crate::connector::{ConnectorAuditReport, ResolvedConversation};
    use crate::model::{ConversationKind, EntityDecodeState};
    use crate::send_contract::{HelperGateEvidence, SendStage, VisualConfirmation};
    use crate::send_profile::{
        sign_calibration_profile, verify_calibration_profile, CalibrationAnchors,
        CalibrationAttachments, CalibrationOcrRegions, CalibrationProfileBody, CalibrationSelfTest,
        CompatibilityState, SendTrustRoot, WindowRelativePoint, WindowRelativeRect,
    };
    use crate::tools::ToolSourceDatabaseFreshness;

    const ACCOUNT: &str = "test-account";
    const CONVERSATION: &str = "filehelper";
    const BODY: &str = "adapter self-check";
    const PROFILE_ID: &str = "wechat-4.1.13.269579-macos-26";
    const CLIENT_BUILD: &str = "wechat-macos-4.1.13-269579";
    const WECHAT_BUILD: &str = "4.1.13.269579";
    const MACOS_BUILD: &str = "25G83";

    fn sha(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    /// A dispatcher whose every answer is scripted, so the control plane's
    /// reaction to each adversarial helper response is exactly reproducible.
    struct ScriptedDispatcher {
        status: Option<HelperCapabilityStatus>,
        outcome: RefCell<Option<Result<HelperSendOutcome, SendFailureCode>>>,
        execute_calls: RefCell<u32>,
    }

    impl ScriptedDispatcher {
        fn new(
            status: Option<HelperCapabilityStatus>,
            outcome: Result<HelperSendOutcome, SendFailureCode>,
        ) -> Self {
            Self {
                status,
                outcome: RefCell::new(Some(outcome)),
                execute_calls: RefCell::new(0),
            }
        }
    }

    impl SendDispatcher for ScriptedDispatcher {
        fn capability_status(
            &self,
            _timeout: Duration,
        ) -> Result<HelperCapabilityStatus, SendFailureCode> {
            self.status
                .clone()
                .ok_or(SendFailureCode::EngineUnavailable)
        }

        fn run_calibration_selftest(
            &self,
            _profile: &SignedCalibrationProfile,
            _timeout: Duration,
        ) -> Result<CalibrationSelfTestReport, SendFailureCode> {
            Err(SendFailureCode::EngineUnavailable)
        }

        fn execute_send(
            &self,
            _capability: &ActionCapabilityEnvelope,
            _timeout: Duration,
        ) -> Result<HelperSendOutcome, SendFailureCode> {
            *self.execute_calls.borrow_mut() += 1;
            self.outcome
                .borrow_mut()
                .take()
                .unwrap_or(Err(SendFailureCode::EngineUnavailable))
        }
    }

    fn ready_status() -> HelperCapabilityStatus {
        HelperCapabilityStatus {
            format_version: SEND_CONTRACT_VERSION,
            helper_version: "1.0.0".to_string(),
            engine_version: "1.0.0".to_string(),
            accessibility_granted: true,
            screen_recording_granted: true,
            wechat_running: true,
            wechat_logged_in: true,
            wechat_bundle_identifier: "com.tencent.xinWeChat".to_string(),
            wechat_marketing_version: "4.1.13".to_string(),
            wechat_build: WECHAT_BUILD.to_string(),
            macos_build: MACOS_BUILD.to_string(),
            macos_major: 26,
            main_window_found: true,
            active_calibration_profile_id: PROFILE_ID.to_string(),
            engine_healthy: true,
            bounded_manifest_scope: vec!["com.tencent.xinWeChat".to_string()],
            observed_at_unix_nanoseconds: 1,
        }
    }

    fn profile_body() -> CalibrationProfileBody {
        let point = WindowRelativePoint {
            x_parts_per_million: 235_000,
            y_parts_per_million: 36_000,
        };
        let rect = WindowRelativeRect {
            x_parts_per_million: 400_000,
            y_parts_per_million: 200_000,
            width_parts_per_million: 200_000,
            height_parts_per_million: 100_000,
        };
        CalibrationProfileBody {
            schema: 1,
            profile_id: PROFILE_ID.to_string(),
            wechat_bundle_identifier: "com.tencent.xinWeChat".to_string(),
            wechat_marketing_version: "4.1.13".to_string(),
            wechat_build: WECHAT_BUILD.to_string(),
            client_build_profile_id: CLIENT_BUILD.to_string(),
            macos_major: 26,
            anchors: CalibrationAnchors {
                search_box: point,
                first_result_row: point,
                compose_box: point,
            },
            ocr_regions: CalibrationOcrRegions {
                title: rect,
                compose: rect,
                newest_outgoing: rect,
            },
            selftest: CalibrationSelfTest {
                focus_indicator: "search_caret".to_string(),
                minimum_title_confidence_parts_per_million: 900_000,
            },
            attachments: Some(CalibrationAttachments {
                attach_control: point,
                confirm_send_button: point,
                compose_attachment: rect,
                confirm_sheet: rect,
                presents_confirmation_sheet: true,
                compose_accepts_pasted_file: true,
            }),
            issued_at_unix_seconds: 0,
            expires_at_unix_seconds: 4_000_000_000,
        }
    }

    fn verified_profile(tier: SendTrustTier) -> VerifiedCalibrationProfile {
        let signed = sign_calibration_profile(&profile_body(), &[7; 32]).unwrap();
        let root = SendTrustRoot {
            release_public_keys: Vec::new(),
            development_public_keys: vec![crate::send_profile::signing_key_public_hex(&[7; 32])],
        };
        let mut verified = verify_calibration_profile(&signed, &root, 1_000).unwrap();
        verified.trust_tier = tier;
        verified
    }

    fn supported_decision() -> CompatibilityDecision {
        CompatibilityDecision {
            macos_build: MACOS_BUILD.to_string(),
            wechat_build: WECHAT_BUILD.to_string(),
            state: CompatibilityState::Supported,
            known_combination: true,
            field_kill_switch_engaged: false,
            expected_calibration_profile_id: PROFILE_ID.to_string(),
            client_build_profile_id: CLIENT_BUILD.to_string(),
            note: String::new(),
        }
    }

    fn draft(now: u128) -> ActionDraft {
        ActionDraft {
            format_version: 1,
            draft_id: sha('b'),
            state: crate::connector::DraftState::DraftOnly,
            account_id: ACCOUNT.to_string(),
            conversation_id: CONVERSATION.to_string(),
            recipient: ResolvedConversation {
                conversation_id: CONVERSATION.to_string(),
                kind: ConversationKind::Direct,
                human_label: "File Transfer".to_string(),
                participant_count: 1,
                participants: Vec::new(),
                owner_participant_id: None,
                entity_decode_state: EntityDecodeState::Complete,
                source_database_freshness: ToolSourceDatabaseFreshness::Fresh,
                limitation_codes: Vec::new(),
            },
            reply_target: None,
            rendered_text: BODY.to_string(),
            rendered_text_sha256: hex::encode(Sha256::digest(BODY.as_bytes())),
            attachments: Vec::new(),
            attachment_intent: None,
            connector_version: "1".to_string(),
            api_version: "1".to_string(),
            source_fingerprint: sha('c'),
            policy_decision_id: sha('d'),
            requester_id: "local-owner".to_string(),
            created_at_unix_nanoseconds: now,
            expires_at_unix_nanoseconds: now + 3_600_000_000_000,
        }
    }

    struct Fixture {
        _root: TempDir,
        adapter: SendAdapter,
        audit_path: PathBuf,
    }

    fn fixture(stage: SendRolloutStage, kill_switch: bool) -> Fixture {
        let root = TempDir::new().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let outbox = root.path().join("outbox");
        let audit_path = root.path().join("audit.ndjson");
        let dispatcher_path = root.path().join("dispatcher");
        fs::write(&dispatcher_path, b"#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&dispatcher_path, fs::Permissions::from_mode(0o700)).unwrap();
        let conversations = match stage {
            SendRolloutStage::DryRun | SendRolloutStage::SelfSend => {
                BTreeSet::from([CONVERSATION.to_string()])
            }
            SendRolloutStage::AllowListed => {
                BTreeSet::from([CONVERSATION.to_string(), "peer".to_string()])
            }
        };
        let config = SendAdapterConfig {
            format_version: SEND_ADAPTER_CONFIG_VERSION,
            account_id: ACCOUNT.to_string(),
            rollout_stage: stage,
            global_kill_switch_engaged: kill_switch,
            gate: ActionGateEvidence {
                gate_decision_id: sha('a'),
                acquisition_gate_passed: true,
                restoration_gate_passed: true,
                mechanism_approved: true,
                legal_review_approved: true,
            },
            adapter: ActionAdapterBinding {
                adapter_id: SEND_ADAPTER_ID.to_string(),
                adapter_version: SEND_ADAPTER_VERSION.to_string(),
                client_build_profile_id: CLIENT_BUILD.to_string(),
            },
            allow_list: ActionAllowList {
                account_ids: BTreeSet::from([ACCOUNT.to_string()]),
                conversation_ids: conversations,
                capabilities: BTreeSet::from([ActionCapability::TextSend]),
            },
            self_send_conversation_id: CONVERSATION.to_string(),
            attachment_rollout_stage: stage,
            maximum_attachment_attempts_per_window: 1,
            staging_root: root.path().join("staging"),
            search_key_overrides: BTreeMap::new(),
            attempt_window_seconds: 3_600,
            maximum_attempts_per_window: 3,
            circuit_breaker_failure_threshold: 3,
            circuit_breaker_cooldown_seconds: 900,
            capability_validity_seconds: 120,
            reconciliation_grace_seconds: 900,
            recall_window_seconds: 120,
            expected_macos_build: MACOS_BUILD.to_string(),
            expected_macos_major: 26,
            expected_wechat_build: WECHAT_BUILD.to_string(),
            calibration_profile_path: root.path().join("profile.json"),
            compatibility_matrix_path: root.path().join("matrix.json"),
            development_trust_root_path: None,
            outbox_directory: outbox,
            audit_log_path: audit_path.clone(),
            draft_directory: root.path().join("drafts"),
            helper: SendHelperConfig {
                dispatcher_executable: dispatcher_path,
                dispatcher_arguments: Vec::new(),
                mach_service_name: "test.helper".to_string(),
                status_timeout_milliseconds: 1_000,
                selftest_timeout_milliseconds: 1_000,
                send_timeout_milliseconds: 1_000,
            },
        };
        config.validate().unwrap();
        Fixture {
            adapter: SendAdapter {
                config,
                trust_root: SendTrustRoot::default(),
            },
            audit_path,
            _root: root,
        }
    }

    fn approval_for(
        adapter: &SendAdapter,
        draft: &ActionDraft,
        now: u128,
        seed: char,
    ) -> ExternalApprovalEvidence {
        ExternalApprovalEvidence {
            approval_id: sha(seed),
            immutable_binding_sha256: adapter.expected_approval_binding(draft).unwrap(),
            approver_id: "local-owner".to_string(),
            approved_at_unix_nanoseconds: now.saturating_sub(1),
            expires_at_unix_nanoseconds: now + 600_000_000_000,
        }
    }

    fn outcome(
        capability_id: &str,
        binding: &str,
        attempted: bool,
        failure: Option<SendFailureCode>,
        stage: SendStage,
    ) -> HelperSendOutcome {
        HelperSendOutcome {
            format_version: SEND_CONTRACT_VERSION,
            capability_id: capability_id.to_string(),
            capability_binding_sha256: binding.to_string(),
            helper_version: "1.0.0".to_string(),
            engine_version: "1.0.0".to_string(),
            calibration_profile_id: PROFILE_ID.to_string(),
            stage_reached: stage,
            attempted,
            visual_confirmation: if attempted {
                VisualConfirmation::Confirmed
            } else {
                VisualConfirmation::NotAttempted
            },
            failure,
            evidence: HelperGateEvidence {
                title_confidence_parts_per_million: 1_000_000,
                title_matched: failure != Some(SendFailureCode::RecipientVerifyFailed),
                compose_matched: failure != Some(SendFailureCode::ContentVerifyFailed),
                attachment_name_matched: false,
                attachment_staged: false,
                confirmation_sheet_confirmed: false,
                compose_cleared: attempted,
                newest_outgoing_matched: attempted,
                ambiguous_search_result: false,
                human_activity_observed: false,
                window_frame_digest: sha('e'),
                capture_count: 4,
                elapsed_milliseconds: 900,
            },
            observed_at_unix_nanoseconds: 2,
        }
    }

    /// Runs `execute` with an outcome computed from the minted capability, so
    /// a scripted helper can answer as the real one would.
    fn run(
        fixture: &Fixture,
        profile: &VerifiedCalibrationProfile,
        draft: &ActionDraft,
        approval: &ExternalApprovalEvidence,
        now: u128,
        make_outcome: impl Fn(&ActionCapabilityEnvelope) -> Result<HelperSendOutcome, SendFailureCode>,
        status: Option<HelperCapabilityStatus>,
    ) -> (SendAttemptReport, u32) {
        let permit_send = fixture
            .adapter
            .config
            .stage_permits_send_to(&draft.conversation_id)
            && fixture.adapter.config.rollout_stage.permits_return()
            && profile.trust_tier == SendTrustTier::Release;
        let capability = fixture
            .adapter
            .mint_capability(draft, approval, profile, None, permit_send, now)
            .unwrap();
        let dispatcher = ScriptedDispatcher::new(status, make_outcome(&capability));
        let report = fixture
            .adapter
            .execute_with_artifacts(
                draft,
                approval,
                profile,
                &supported_decision(),
                &dispatcher,
                None,
                now,
            )
            .unwrap();
        let calls = *dispatcher.execute_calls.borrow();
        (report, calls)
    }

    fn audit(fixture: &Fixture) -> ConnectorAuditReport {
        audit_connector_log(&fixture.audit_path).unwrap()
    }

    #[test]
    fn a_dry_run_reaches_both_gates_stops_before_return_and_leaves_a_verifiable_chain() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::DryRun, false);
        let profile = verified_profile(SendTrustTier::Development);
        let draft = draft(now);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let (report, calls) = run(
            &fixture,
            &profile,
            &draft,
            &approval,
            now,
            |capability| {
                Ok(outcome(
                    &capability.capability_id,
                    &capability.binding_sha256,
                    false,
                    None,
                    SendStage::ContentVerify,
                ))
            },
            Some(ready_status()),
        );
        assert_eq!(calls, 1);
        assert!(report.precheck.permitted);
        assert!(!report.precheck.permit_send);
        assert!(!report.attempted);
        assert_eq!(report.completion, Some(SendCompletionKind::DryRunCompleted));
        assert!(report.lifecycle_state.is_none());
        assert!(!report.awaiting_reconciliation);
        let audit = audit(&fixture);
        assert!(audit.chain_verified && audit.fully_chained);
        assert_eq!(audit.approval_event_count, 1);
        assert_eq!(audit.attempt_event_count, 1);
        assert_eq!(audit.reconciliation_event_count, 1);
        let journal = fs::read_to_string(&fixture.audit_path).unwrap();
        assert!(!journal.contains(BODY));
    }

    #[test]
    fn the_same_approval_can_never_be_dispatched_twice() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::DryRun, false);
        let profile = verified_profile(SendTrustTier::Development);
        let draft = draft(now);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let script = |capability: &ActionCapabilityEnvelope| {
            Ok(outcome(
                &capability.capability_id,
                &capability.binding_sha256,
                false,
                None,
                SendStage::ContentVerify,
            ))
        };
        run(
            &fixture,
            &profile,
            &draft,
            &approval,
            now,
            script,
            Some(ready_status()),
        );
        let (second, calls) = run(
            &fixture,
            &profile,
            &draft,
            &approval,
            now + 1_000_000_000,
            script,
            Some(ready_status()),
        );
        assert_eq!(calls, 0);
        assert!(!second.dispatched);
        assert!(second
            .precheck
            .failures
            .contains(&SendFailureCode::IdempotencyConflict));
        assert!(second
            .precheck
            .failures
            .contains(&SendFailureCode::ApprovalInvalid));
    }

    #[test]
    fn the_kill_switch_denies_before_the_helper_is_ever_called() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::DryRun, true);
        let profile = verified_profile(SendTrustTier::Development);
        let draft = draft(now);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let (report, calls) = run(
            &fixture,
            &profile,
            &draft,
            &approval,
            now,
            |capability| {
                Ok(outcome(
                    &capability.capability_id,
                    &capability.binding_sha256,
                    false,
                    None,
                    SendStage::ContentVerify,
                ))
            },
            Some(ready_status()),
        );
        assert_eq!(calls, 0);
        assert!(!report.dispatched);
        assert!(report
            .precheck
            .failures
            .contains(&SendFailureCode::KillSwitchEngaged));
        assert_eq!(audit(&fixture).denied_event_count, 1);
    }

    #[test]
    fn a_helper_that_claims_a_send_a_dry_run_forbade_is_treated_as_a_mismatch() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::DryRun, false);
        let profile = verified_profile(SendTrustTier::Development);
        let draft = draft(now);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let (report, _) = run(
            &fixture,
            &profile,
            &draft,
            &approval,
            now,
            |capability| {
                Ok(outcome(
                    &capability.capability_id,
                    &capability.binding_sha256,
                    true,
                    None,
                    SendStage::SendVerify,
                ))
            },
            Some(ready_status()),
        );
        assert!(!report.attempted);
        assert_eq!(report.failure, Some(SendFailureCode::CapabilityMismatch));
        assert_eq!(report.completion, Some(SendCompletionKind::ObservedFailed));
    }

    #[test]
    fn a_recipient_gate_failure_closes_the_attempt_without_a_send() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::DryRun, false);
        let profile = verified_profile(SendTrustTier::Development);
        let draft = draft(now);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let (report, _) = run(
            &fixture,
            &profile,
            &draft,
            &approval,
            now,
            |capability| {
                Ok(outcome(
                    &capability.capability_id,
                    &capability.binding_sha256,
                    false,
                    Some(SendFailureCode::RecipientVerifyFailed),
                    SendStage::RecipientVerify,
                ))
            },
            Some(ready_status()),
        );
        assert!(!report.attempted);
        assert_eq!(report.failure, Some(SendFailureCode::RecipientVerifyFailed));
        assert_eq!(report.completion, Some(SendCompletionKind::ObservedFailed));
        assert_eq!(report.stage_reached, Some(SendStage::RecipientVerify));
    }

    #[test]
    fn a_missing_grant_or_unknown_build_denies_before_dispatch() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::DryRun, false);
        let profile = verified_profile(SendTrustTier::Development);
        let draft = draft(now);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let mut status = ready_status();
        status.accessibility_granted = false;
        status.wechat_build = "4.1.14.1".to_string();
        let (report, calls) = run(
            &fixture,
            &profile,
            &draft,
            &approval,
            now,
            |capability| {
                Ok(outcome(
                    &capability.capability_id,
                    &capability.binding_sha256,
                    false,
                    None,
                    SendStage::ContentVerify,
                ))
            },
            Some(status),
        );
        assert_eq!(calls, 0);
        assert!(report
            .precheck
            .failures
            .contains(&SendFailureCode::GrantsMissing));
        assert!(report
            .precheck
            .failures
            .contains(&SendFailureCode::UnknownBuild));
    }

    #[test]
    fn an_unreachable_helper_denies_the_precheck_outright() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::DryRun, false);
        let profile = verified_profile(SendTrustTier::Development);
        let draft = draft(now);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let (report, calls) = run(
            &fixture,
            &profile,
            &draft,
            &approval,
            now,
            |_| Err(SendFailureCode::EngineStall),
            None,
        );
        assert_eq!(calls, 0);
        assert!(report
            .precheck
            .failures
            .contains(&SendFailureCode::EngineUnavailable));
    }

    #[test]
    fn a_development_signed_profile_can_never_unlock_a_send_permitting_stage() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::SelfSend, false);
        let profile = verified_profile(SendTrustTier::Development);
        let draft = draft(now);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let (report, calls) = run(
            &fixture,
            &profile,
            &draft,
            &approval,
            now,
            |capability| {
                Ok(outcome(
                    &capability.capability_id,
                    &capability.binding_sha256,
                    false,
                    None,
                    SendStage::ContentVerify,
                ))
            },
            Some(ready_status()),
        );
        assert_eq!(calls, 0);
        assert!(!report.precheck.permit_send);
        assert!(report
            .precheck
            .failures
            .contains(&SendFailureCode::ProfileInvalid));
    }

    #[test]
    fn a_confirmed_self_send_is_parked_until_the_replica_confirms_it() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::SelfSend, false);
        let profile = verified_profile(SendTrustTier::Release);
        let draft = draft(now);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let (report, calls) = run(
            &fixture,
            &profile,
            &draft,
            &approval,
            now,
            |capability| {
                assert!(capability.permit_send);
                Ok(outcome(
                    &capability.capability_id,
                    &capability.binding_sha256,
                    true,
                    None,
                    SendStage::SendVerify,
                ))
            },
            Some(ready_status()),
        );
        assert_eq!(calls, 1);
        assert!(report.attempted);
        assert_eq!(report.visual_confirmation, VisualConfirmation::Confirmed);
        // The helper's own capture never creates observedSent.
        assert!(report.completion.is_none());
        assert!(report.awaiting_reconciliation);
        assert!(report.lifecycle_state.is_none());

        let observation = SendReconciliationObservation {
            format_version: SEND_CONTRACT_VERSION,
            idempotency_key: report.idempotency_key.clone(),
            account_id: ACCOUNT.to_string(),
            conversation_id: CONVERSATION.to_string(),
            source: SendObservationSource::EncryptedReplica,
            source_fingerprint: sha('f'),
            outgoing_message_found: true,
            normalized_body_matched: true,
            attachment_reference_found: false,
            display_file_name_matched: false,
            match_strength: SendMatchStrength::BodyDigest,
            canonical_id: Some(sha('9')),
            scanned_message_count: 3,
            observed_at_unix_nanoseconds: now + 1_000_000_000,
        };
        let reconciled = fixture
            .adapter
            .reconcile(&observation, Some(&draft), now + 1_000_000_000)
            .unwrap();
        assert!(reconciled.resolved);
        assert_eq!(
            reconciled.lifecycle_state,
            Some(ActionLifecycleState::ObservedSent)
        );
        assert_eq!(reconciled.outbox.pending_reconciliation_count, 0);
    }

    #[test]
    fn a_dispatched_attempt_reports_its_recall_window_and_the_manual_procedure() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::SelfSend, false);
        let profile = verified_profile(SendTrustTier::Release);
        let draft = draft(now);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let (report, _) = run(
            &fixture,
            &profile,
            &draft,
            &approval,
            now,
            |capability| {
                Ok(outcome(
                    &capability.capability_id,
                    &capability.binding_sha256,
                    true,
                    None,
                    SendStage::SendVerify,
                ))
            },
            Some(ready_status()),
        );
        assert!(report.recall_deadline_unix_nanoseconds.is_some());
        let window = fixture
            .adapter
            .recall_window(
                &report.idempotency_key,
                report.completed_at_unix_nanoseconds + 1,
            )
            .unwrap();
        assert!(window.attempted);
        assert!(window.recallable);
        assert!(window.remaining_seconds > 0 && window.remaining_seconds <= 120);
        assert_eq!(window.procedure.len(), 4);
        let expired = fixture
            .adapter
            .recall_window(
                &report.idempotency_key,
                report.completed_at_unix_nanoseconds + 200_000_000_000,
            )
            .unwrap();
        assert!(!expired.recallable);
        assert_eq!(expired.remaining_seconds, 0);
    }

    #[test]
    fn a_dry_run_never_opens_a_recall_window() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::DryRun, false);
        let profile = verified_profile(SendTrustTier::Development);
        let draft = draft(now);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let (report, _) = run(
            &fixture,
            &profile,
            &draft,
            &approval,
            now,
            |capability| {
                Ok(outcome(
                    &capability.capability_id,
                    &capability.binding_sha256,
                    false,
                    None,
                    SendStage::ContentVerify,
                ))
            },
            Some(ready_status()),
        );
        assert!(report.recall_deadline_unix_nanoseconds.is_none());
        let window = fixture
            .adapter
            .recall_window(&report.idempotency_key, now + 1)
            .unwrap();
        assert!(!window.attempted);
        assert!(!window.recallable);
    }

    #[test]
    fn a_signed_matrix_kill_switch_denies_before_the_helper_is_ever_called() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::DryRun, false);
        let profile = verified_profile(SendTrustTier::Development);
        let draft = draft(now);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let mut decision = supported_decision();
        decision.field_kill_switch_engaged = true;
        let dispatcher =
            ScriptedDispatcher::new(Some(ready_status()), Err(SendFailureCode::EngineStall));
        let report = fixture
            .adapter
            .execute_with_artifacts(
                &draft,
                &approval,
                &profile,
                &decision,
                &dispatcher,
                None,
                now,
            )
            .unwrap();
        assert_eq!(*dispatcher.execute_calls.borrow(), 0);
        assert!(report
            .precheck
            .failures
            .contains(&SendFailureCode::KillSwitchEngaged));
        assert!(report
            .precheck
            .failures
            .contains(&SendFailureCode::ProfileInvalid));
    }

    #[test]
    fn an_engine_stall_after_a_send_permitting_dispatch_parks_and_never_resends() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::SelfSend, false);
        let profile = verified_profile(SendTrustTier::Release);
        let draft = draft(now);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let (report, calls) = run(
            &fixture,
            &profile,
            &draft,
            &approval,
            now,
            |_| Err(SendFailureCode::EngineStall),
            Some(ready_status()),
        );
        assert_eq!(calls, 1);
        assert!(report.awaiting_reconciliation);
        assert_eq!(report.failure, Some(SendFailureCode::EngineStall));
        let (summary, _, pending) = fixture.adapter.outbox_status(now + 1).unwrap();
        assert_eq!(summary.pending_reconciliation_count, 1);
        assert_eq!(pending[0].state, OutboxEntryState::AwaitingReconciliation);

        // A second, freshly approved draft cannot dispatch while an attempt is
        // unreconciled: recovery reconciles, it never resends.
        let second_approval = approval_for(&fixture.adapter, &draft, now, '2');
        let (blocked, calls) = run(
            &fixture,
            &profile,
            &draft,
            &second_approval,
            now + 1_000_000_000,
            |capability| {
                Ok(outcome(
                    &capability.capability_id,
                    &capability.binding_sha256,
                    true,
                    None,
                    SendStage::SendVerify,
                ))
            },
            Some(ready_status()),
        );
        assert_eq!(calls, 0);
        assert!(blocked
            .precheck
            .failures
            .contains(&SendFailureCode::ReconciliationPending));
    }

    #[test]
    fn an_unproven_attempt_stays_parked_until_the_grace_window_elapses() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::SelfSend, false);
        let profile = verified_profile(SendTrustTier::Release);
        let draft = draft(now);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let (report, _) = run(
            &fixture,
            &profile,
            &draft,
            &approval,
            now,
            |capability| {
                Ok(outcome(
                    &capability.capability_id,
                    &capability.binding_sha256,
                    true,
                    None,
                    SendStage::SendVerify,
                ))
            },
            Some(ready_status()),
        );
        let mut observation = SendReconciliationObservation {
            format_version: SEND_CONTRACT_VERSION,
            idempotency_key: report.idempotency_key.clone(),
            account_id: ACCOUNT.to_string(),
            conversation_id: CONVERSATION.to_string(),
            source: SendObservationSource::EncryptedReplica,
            source_fingerprint: sha('f'),
            outgoing_message_found: false,
            normalized_body_matched: false,
            attachment_reference_found: false,
            display_file_name_matched: false,
            match_strength: SendMatchStrength::None,
            canonical_id: None,
            scanned_message_count: 0,
            observed_at_unix_nanoseconds: now + 1,
        };
        let early = fixture
            .adapter
            .reconcile(&observation, Some(&draft), now + 1)
            .unwrap();
        assert!(!early.resolved);
        assert!(early.still_awaiting_reconciliation);
        assert!(!early.grace_elapsed);

        let later = now + 1_000_000_000_000;
        observation.observed_at_unix_nanoseconds = later;
        let settled = fixture
            .adapter
            .reconcile(&observation, Some(&draft), later)
            .unwrap();
        assert!(settled.resolved);
        assert!(settled.grace_elapsed);
        assert_eq!(
            settled.lifecycle_state,
            Some(ActionLifecycleState::ObservedFailed)
        );
    }

    #[test]
    fn a_stage_that_cannot_reach_the_conversation_is_refused() {
        let now = 10_000_000_000_000_u128;
        let mut fixture = fixture(SendRolloutStage::AllowListed, false);
        fixture.adapter.config.self_send_conversation_id = CONVERSATION.to_string();
        let profile = verified_profile(SendTrustTier::Release);
        let mut draft = draft(now);
        draft.conversation_id = "not-allow-listed".to_string();
        draft.recipient.conversation_id = "not-allow-listed".to_string();
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let (report, calls) = run(
            &fixture,
            &profile,
            &draft,
            &approval,
            now,
            |capability| {
                Ok(outcome(
                    &capability.capability_id,
                    &capability.binding_sha256,
                    false,
                    None,
                    SendStage::ContentVerify,
                ))
            },
            Some(ready_status()),
        );
        assert_eq!(calls, 0);
        assert!(report
            .precheck
            .failures
            .contains(&SendFailureCode::StageNotPermitted));
    }

    #[test]
    fn a_tampered_draft_body_is_refused_before_anything_is_minted() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::DryRun, false);
        let profile = verified_profile(SendTrustTier::Development);
        let mut draft = draft(now);
        draft.rendered_text = "a different body".to_string();
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        // Minting refuses the same draft independently of PRECHECK: the body
        // digest is checked on both sides of the boundary.
        assert!(fixture
            .adapter
            .mint_capability(&draft, &approval, &profile, None, false, now)
            .is_err());
        let dispatcher =
            ScriptedDispatcher::new(Some(ready_status()), Err(SendFailureCode::EngineStall));
        let report = fixture
            .adapter
            .execute_with_artifacts(
                &draft,
                &approval,
                &profile,
                &supported_decision(),
                &dispatcher,
                None,
                now,
            )
            .unwrap();
        assert_eq!(*dispatcher.execute_calls.borrow(), 0);
        assert!(!report.dispatched);
        assert!(report
            .precheck
            .failures
            .contains(&SendFailureCode::DraftInvalid));
    }

    /// Builds an attachment draft plus the file it approves, and allow-lists
    /// the capability, which a configuration must always do explicitly.
    fn attachment_fixture(
        fixture: &mut Fixture,
        now: u128,
        capability: ActionCapability,
        name: &str,
    ) -> (ActionDraft, PathBuf, Vec<u8>) {
        fixture
            .adapter
            .config
            .allow_list
            .capabilities
            .insert(capability);
        let contents = b"an approved attachment payload".repeat(8);
        let source = fixture._root.path().join(name);
        fs::write(&source, &contents).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
        let mut draft = draft(now);
        draft.rendered_text = String::new();
        draft.rendered_text_sha256 = hex::encode(Sha256::digest(b""));
        draft.attachment_intent = Some(capability);
        draft.attachments = vec![crate::connector::DraftAttachment {
            artifact_id: "artifact".to_string(),
            kind: crate::model::ArtifactKind::Image,
            role: crate::model::ArtifactRole::Original,
            digest_kind: "sourceSha256".to_string(),
            sha256: hex::encode(Sha256::digest(&contents)),
            byte_count: Some(contents.len() as u64),
            display_file_name: name.to_string(),
        }];
        (draft, source, contents)
    }

    #[test]
    fn an_attachment_dry_run_stages_the_file_and_retires_the_copy() {
        let now = 10_000_000_000_000_u128;
        let mut fixture = fixture(SendRolloutStage::DryRun, false);
        let profile = verified_profile(SendTrustTier::Development);
        let (draft, source, contents) =
            attachment_fixture(&mut fixture, now, ActionCapability::ImageSend, "photo.png");
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let dispatcher = ScriptedDispatcher::new(
            Some(ready_status()),
            Ok(HelperSendOutcome {
                format_version: SEND_CONTRACT_VERSION,
                capability_id: sha('0'),
                capability_binding_sha256: sha('0'),
                helper_version: "1.0.0".to_string(),
                engine_version: "1.0.0".to_string(),
                calibration_profile_id: PROFILE_ID.to_string(),
                stage_reached: SendStage::ContentVerify,
                attempted: false,
                visual_confirmation: VisualConfirmation::NotAttempted,
                failure: None,
                evidence: HelperGateEvidence {
                    title_confidence_parts_per_million: 1_000_000,
                    title_matched: true,
                    compose_matched: true,
                    attachment_name_matched: true,
                    attachment_staged: true,
                    confirmation_sheet_confirmed: false,
                    compose_cleared: true,
                    newest_outgoing_matched: false,
                    ambiguous_search_result: false,
                    human_activity_observed: false,
                    window_frame_digest: sha('e'),
                    capture_count: 4,
                    elapsed_milliseconds: 900,
                },
                observed_at_unix_nanoseconds: 2,
            }),
        );
        // The scripted outcome cannot know the capability identity in advance,
        // so this run proves the mismatch guard fires; what matters here is
        // that staging happened and was retired either way.
        let report = fixture
            .adapter
            .execute_with_artifacts(
                &draft,
                &approval,
                &profile,
                &supported_decision(),
                &dispatcher,
                Some(source.as_path()),
                now,
            )
            .unwrap();
        assert!(report.precheck.permitted, "{:?}", report.precheck.failures);
        assert!(!report.attempted);
        assert_eq!(*dispatcher.execute_calls.borrow(), 1);
        // Nothing is left of the staged copy once the attempt is terminal.
        let staging = fixture.adapter.config.staging_root.clone();
        let leftovers = fs::read_dir(&staging)
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(leftovers, 0, "a terminal attempt left a staged copy behind");
        // The owner's original is untouched.
        assert_eq!(fs::read(&source).unwrap(), contents);
    }

    #[test]
    fn an_attachment_whose_file_changed_since_approval_is_refused_before_dispatch() {
        let now = 10_000_000_000_000_u128;
        let mut fixture = fixture(SendRolloutStage::DryRun, false);
        let profile = verified_profile(SendTrustTier::Development);
        let (draft, source, _) =
            attachment_fixture(&mut fixture, now, ActionCapability::FileSend, "report.pdf");
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        fs::write(&source, b"a different file entirely").unwrap();
        let dispatcher =
            ScriptedDispatcher::new(Some(ready_status()), Err(SendFailureCode::EngineStall));
        let error = fixture
            .adapter
            .execute_with_artifacts(
                &draft,
                &approval,
                &profile,
                &supported_decision(),
                &dispatcher,
                Some(source.as_path()),
                now,
            )
            .unwrap_err();
        assert!(error.to_string().contains("attachmentDigestMismatch"));
        assert_eq!(*dispatcher.execute_calls.borrow(), 0);
        let staging = fixture.adapter.config.staging_root.clone();
        assert_eq!(
            fs::read_dir(&staging)
                .map(|entries| entries.count())
                .unwrap_or(0),
            0
        );
    }

    #[test]
    fn an_attachment_capability_absent_from_the_allow_list_is_refused() {
        let now = 10_000_000_000_000_u128;
        let mut fixture = fixture(SendRolloutStage::DryRun, false);
        let profile = verified_profile(SendTrustTier::Development);
        let (draft, source, _) =
            attachment_fixture(&mut fixture, now, ActionCapability::FileSend, "report.pdf");
        // Undo the allow-listing the fixture did: a configuration that lists
        // only textSend must refuse a file send outright.
        fixture
            .adapter
            .config
            .allow_list
            .capabilities
            .remove(&ActionCapability::FileSend);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let dispatcher =
            ScriptedDispatcher::new(Some(ready_status()), Err(SendFailureCode::EngineStall));
        let report = fixture
            .adapter
            .execute_with_artifacts(
                &draft,
                &approval,
                &profile,
                &supported_decision(),
                &dispatcher,
                Some(source.as_path()),
                now,
            )
            .unwrap();
        assert!(!report.precheck.permitted);
        assert!(report
            .precheck
            .guard_denials
            .contains(&ActionGuardDenial::CapabilityNotAllowed));
        assert_eq!(*dispatcher.execute_calls.borrow(), 0);
    }

    #[test]
    fn an_attachment_is_refused_while_the_attachment_stage_is_closed() {
        let now = 10_000_000_000_000_u128;
        let mut fixture = fixture(SendRolloutStage::SelfSend, false);
        // Text sending is open; attachments are not.
        fixture.adapter.config.attachment_rollout_stage = SendRolloutStage::DryRun;
        let profile = verified_profile(SendTrustTier::Release);
        let (draft, source, _) =
            attachment_fixture(&mut fixture, now, ActionCapability::FileSend, "report.pdf");
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let dispatcher =
            ScriptedDispatcher::new(Some(ready_status()), Err(SendFailureCode::EngineStall));
        let report = fixture
            .adapter
            .execute_with_artifacts(
                &draft,
                &approval,
                &profile,
                &supported_decision(),
                &dispatcher,
                Some(source.as_path()),
                now,
            )
            .unwrap();
        assert!(!report.precheck.permit_send);
        assert_eq!(*dispatcher.execute_calls.borrow(), 0);
    }

    #[test]
    fn a_forwarding_draft_without_a_stated_intent_can_never_send_an_attachment() {
        let now = 10_000_000_000_000_u128;
        let mut fixture = fixture(SendRolloutStage::DryRun, false);
        let profile = verified_profile(SendTrustTier::Development);
        let (mut draft, source, _) =
            attachment_fixture(&mut fixture, now, ActionCapability::FileSend, "report.pdf");
        // Exactly what the connector can produce: attachments, but no intent.
        draft.attachment_intent = None;
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let dispatcher =
            ScriptedDispatcher::new(Some(ready_status()), Err(SendFailureCode::EngineStall));
        let report = fixture
            .adapter
            .execute_with_artifacts(
                &draft,
                &approval,
                &profile,
                &supported_decision(),
                &dispatcher,
                Some(source.as_path()),
                now,
            )
            .unwrap();
        assert!(!report.precheck.permitted);
        assert!(report
            .precheck
            .failures
            .contains(&SendFailureCode::DraftInvalid));
        assert_eq!(*dispatcher.execute_calls.borrow(), 0);
    }

    #[test]
    fn an_image_send_records_that_the_recipient_receives_a_derivative() {
        let now = 10_000_000_000_000_u128;
        let mut fixture = fixture(SendRolloutStage::DryRun, false);
        let profile = verified_profile(SendTrustTier::Development);
        let (draft, source, _) =
            attachment_fixture(&mut fixture, now, ActionCapability::ImageSend, "photo.png");
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let staged = stage_attachment(
            &source,
            &fixture.adapter.config.staging_root,
            &draft.attachments[0],
            ActionCapability::ImageSend,
        )
        .unwrap();
        assert!(!staged.bytes_preserved_in_transit);
        let capability = fixture
            .adapter
            .mint_capability(&draft, &approval, &profile, Some(&staged), false, now)
            .unwrap();
        assert_eq!(capability.capability, ActionCapability::ImageSend);
        assert!(capability.body.is_empty());
        let attachment = capability.attachment.as_ref().unwrap();
        assert_eq!(attachment.display_file_name, "photo.png");
        assert_eq!(attachment.uniform_type_identifier, "public.png");
        assert!(capability.validate(now).is_ok());
        // Swapping the staged file for another one invalidates the binding.
        let mut tampered = capability.clone();
        tampered.attachment.as_mut().unwrap().sha256 = sha('9');
        assert!(tampered.validate(now).is_err());
    }

    #[test]
    fn a_capability_can_never_carry_a_body_and_an_attachment_at_once() {
        let now = 10_000_000_000_000_u128;
        let mut fixture = fixture(SendRolloutStage::DryRun, false);
        let profile = verified_profile(SendTrustTier::Development);
        let (draft, source, _) =
            attachment_fixture(&mut fixture, now, ActionCapability::FileSend, "report.pdf");
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let staged = stage_attachment(
            &source,
            &fixture.adapter.config.staging_root,
            &draft.attachments[0],
            ActionCapability::FileSend,
        )
        .unwrap();
        let mut capability = fixture
            .adapter
            .mint_capability(&draft, &approval, &profile, Some(&staged), false, now)
            .unwrap();
        capability.body = "a caption".to_string();
        capability.body_sha256 = hex::encode(Sha256::digest(capability.body.as_bytes()));
        capability.normalized_body_sha256 = normalized_send_text_sha256(&capability.body);
        capability.binding_sha256 = capability_binding_sha256(&capability).unwrap();
        assert_eq!(
            capability.validate(now).unwrap_err(),
            SendFailureCode::CapabilityMismatch
        );
    }

    /// Drives an attachment attempt to the parked state so reconciliation can
    /// be exercised against it.
    fn parked_attachment_attempt(
        fixture: &mut Fixture,
        capability: ActionCapability,
        name: &str,
        now: u128,
    ) -> (ActionDraft, SendAttemptReport) {
        let profile = verified_profile(SendTrustTier::Release);
        let (draft, source, _) = attachment_fixture(fixture, now, capability, name);
        fixture.adapter.config.attachment_rollout_stage = SendRolloutStage::SelfSend;
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let staged = stage_attachment(
            &source,
            &fixture.adapter.config.staging_root,
            &draft.attachments[0],
            capability,
        )
        .unwrap();
        let minted = fixture
            .adapter
            .mint_capability(&draft, &approval, &profile, Some(&staged), true, now)
            .unwrap();
        let dispatcher = ScriptedDispatcher::new(
            Some(ready_status()),
            Ok(outcome(
                &minted.capability_id,
                &minted.binding_sha256,
                true,
                None,
                SendStage::SendVerify,
            )),
        );
        let report = fixture
            .adapter
            .execute_with_artifacts(
                &draft,
                &approval,
                &profile,
                &supported_decision(),
                &dispatcher,
                Some(source.as_path()),
                now,
            )
            .unwrap();
        (draft, report)
    }

    #[test]
    fn a_file_send_is_only_observed_sent_when_its_name_is_found_in_the_replica() {
        let now = 10_000_000_000_000_u128;
        let mut fixture = fixture(SendRolloutStage::SelfSend, false);
        let (draft, report) =
            parked_attachment_attempt(&mut fixture, ActionCapability::FileSend, "report.pdf", now);
        assert!(report.awaiting_reconciliation, "{:?}", report.failure);

        let base = SendReconciliationObservation {
            format_version: SEND_CONTRACT_VERSION,
            idempotency_key: report.idempotency_key.clone(),
            account_id: ACCOUNT.to_string(),
            conversation_id: CONVERSATION.to_string(),
            source: SendObservationSource::EncryptedReplica,
            source_fingerprint: sha('f'),
            outgoing_message_found: true,
            normalized_body_matched: false,
            attachment_reference_found: true,
            display_file_name_matched: false,
            match_strength: SendMatchStrength::AttachmentPresenceOnly,
            canonical_id: Some(sha('9')),
            scanned_message_count: 2,
            observed_at_unix_nanoseconds: now + 1,
        };
        // Presence alone is not enough for a file send: its name survives, so
        // the absence of a name match means this is not our message.
        let weak = fixture
            .adapter
            .reconcile(&base, Some(&draft), now + 1)
            .unwrap();
        assert!(!weak.resolved);

        let named = SendReconciliationObservation {
            display_file_name_matched: true,
            match_strength: SendMatchStrength::AttachmentFileName,
            ..base
        };
        let resolved = fixture
            .adapter
            .reconcile(&named, Some(&draft), now + 2)
            .unwrap();
        assert!(resolved.resolved);
        assert_eq!(
            resolved.lifecycle_state,
            Some(ActionLifecycleState::ObservedSent)
        );
        // The staged copy is retired once the attempt is settled.
        assert_eq!(
            fs::read_dir(&fixture.adapter.config.staging_root)
                .map(|entries| entries.count())
                .unwrap_or(0),
            0
        );
    }

    #[test]
    fn an_image_send_may_be_observed_sent_on_presence_alone_and_says_so() {
        let now = 10_000_000_000_000_u128;
        let mut fixture = fixture(SendRolloutStage::SelfSend, false);
        let (draft, report) =
            parked_attachment_attempt(&mut fixture, ActionCapability::ImageSend, "photo.png", now);
        assert!(report.awaiting_reconciliation, "{:?}", report.failure);
        let observation = SendReconciliationObservation {
            format_version: SEND_CONTRACT_VERSION,
            idempotency_key: report.idempotency_key.clone(),
            account_id: ACCOUNT.to_string(),
            conversation_id: CONVERSATION.to_string(),
            source: SendObservationSource::EncryptedReplica,
            source_fingerprint: sha('f'),
            outgoing_message_found: true,
            normalized_body_matched: false,
            attachment_reference_found: true,
            display_file_name_matched: false,
            // The client re-encoded the image, so the name did not survive.
            match_strength: SendMatchStrength::AttachmentPresenceOnly,
            canonical_id: Some(sha('9')),
            scanned_message_count: 1,
            observed_at_unix_nanoseconds: now + 1,
        };
        let resolved = fixture
            .adapter
            .reconcile(&observation, Some(&draft), now + 1)
            .unwrap();
        assert!(resolved.resolved);
        assert_eq!(
            resolved.lifecycle_state,
            Some(ActionLifecycleState::ObservedSent)
        );
        // The weaker evidence is preserved verbatim in the report.
        assert_eq!(
            resolved.observation.match_strength,
            SendMatchStrength::AttachmentPresenceOnly
        );
    }

    #[test]
    fn a_text_send_can_never_be_settled_by_attachment_evidence() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::SelfSend, false);
        let profile = verified_profile(SendTrustTier::Release);
        let draft = draft(now);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let (report, _) = run(
            &fixture,
            &profile,
            &draft,
            &approval,
            now,
            |capability| {
                Ok(outcome(
                    &capability.capability_id,
                    &capability.binding_sha256,
                    true,
                    None,
                    SendStage::SendVerify,
                ))
            },
            Some(ready_status()),
        );
        let observation = SendReconciliationObservation {
            format_version: SEND_CONTRACT_VERSION,
            idempotency_key: report.idempotency_key.clone(),
            account_id: ACCOUNT.to_string(),
            conversation_id: CONVERSATION.to_string(),
            source: SendObservationSource::EncryptedReplica,
            source_fingerprint: sha('f'),
            outgoing_message_found: true,
            normalized_body_matched: false,
            attachment_reference_found: true,
            display_file_name_matched: true,
            match_strength: SendMatchStrength::AttachmentFileName,
            canonical_id: Some(sha('9')),
            scanned_message_count: 1,
            observed_at_unix_nanoseconds: now + 1,
        };
        let report = fixture
            .adapter
            .reconcile(&observation, Some(&draft), now + 1)
            .unwrap();
        assert!(
            !report.resolved,
            "an attachment must not settle a text send"
        );
    }

    #[test]
    fn the_minted_capability_binds_the_recipient_the_control_plane_resolved() {
        let now = 10_000_000_000_000_u128;
        let fixture = fixture(SendRolloutStage::DryRun, false);
        let profile = verified_profile(SendTrustTier::Development);
        let draft = draft(now);
        let approval = approval_for(&fixture.adapter, &draft, now, '1');
        let capability = fixture
            .adapter
            .mint_capability(&draft, &approval, &profile, None, false, now)
            .unwrap();
        assert_eq!(capability.expected_title, "File Transfer");
        assert_eq!(capability.search_key, "File Transfer");
        assert!(!capability.permit_send);
        assert_eq!(capability.calibration_profile_id, PROFILE_ID);
        assert!(capability.validate(now).is_ok());
        assert!(capability.validate(now + 121_000_000_000).is_err());
    }
}
