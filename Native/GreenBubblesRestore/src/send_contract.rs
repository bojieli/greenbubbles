//! The vocabulary shared by the Rust control plane and the first-party input
//! helper: rollout stages, the failure taxonomy, the single-use bound action
//! capability, and the helper's status and outcome envelopes.
//!
//! Every type here crosses a process boundary, so each one is validated on
//! receipt rather than trusted. The helper never receives keys, the replica,
//! or any tool policy; it receives exactly one capability bound to one
//! recipient, one body, and one short validity window, and it enforces the
//! on-screen gates against that capability alone.
//!
//! The Swift package mirrors these declarations byte for byte, including
//! `capability_binding_bytes`; `tests/send_contract_vectors.rs` and the Swift
//! test suite pin the same fixture digests so the two encoders cannot drift.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::action::{ActionCapability, ActionLifecycleState};

/// Format version of every envelope in this module.
pub const SEND_CONTRACT_VERSION: u32 = 1;
/// Largest body a single text send may carry.
pub const MAXIMUM_SEND_BODY_BYTES: usize = 4_096;
/// Largest attachment this adapter will stage and send.
pub const MAXIMUM_ATTACHMENT_BYTES: u64 = 100 * 1024 * 1024;
/// Largest display file name a staged attachment may carry.
pub const MAXIMUM_DISPLAY_FILE_NAME_BYTES: usize = 255;
/// Largest search key the addressing step may type.
pub const MAXIMUM_SEARCH_KEY_BYTES: usize = 256;
/// Largest recipient title the recipient gate may compare.
pub const MAXIMUM_EXPECTED_TITLE_BYTES: usize = 256;

const ACTION_CAPABILITY_DOMAIN: &str = "greenbubbles.send.action-capability.v1";

/// The canonical name of a reviewed capability, shared with the signed bytes.
pub(crate) fn action_capability_name(capability: ActionCapability) -> &'static str {
    match capability {
        ActionCapability::TextSend => "textSend",
        ActionCapability::ReplySend => "replySend",
        ActionCapability::ImageSend => "imageSend",
        ActionCapability::FileSend => "fileSend",
    }
}

/// The single text normalization both the on-screen gates and replica
/// reconciliation use: trim the ends and fold every run of Unicode whitespace
/// into one space. Vision's line output and WeChat's re-decoded content both
/// vary only in whitespace for a plain text send, so comparing normalized
/// forms is exact where comparing raw bytes would be brittle.
pub fn normalized_send_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Digest of the normalized form of a body.
pub fn normalized_send_text_sha256(value: &str) -> String {
    hex::encode(Sha256::digest(normalized_send_text(value).as_bytes()))
}

/// A canonical byte encoder shared with the Swift helper. Strings are written
/// as their UTF-8 bytes followed by one NUL; unsigned integers are written as
/// decimal ASCII followed by one NUL. A NUL inside a field invalidates the
/// encoding rather than producing ambiguous bytes.
#[derive(Debug, Default)]
pub(crate) struct CanonicalWriter {
    bytes: Vec<u8>,
    valid: bool,
}

impl CanonicalWriter {
    pub(crate) fn new(domain: &str) -> Self {
        let mut writer = Self {
            bytes: Vec::new(),
            valid: true,
        };
        writer.text(domain);
        writer
    }

    pub(crate) fn text(&mut self, value: &str) -> &mut Self {
        if value.as_bytes().contains(&0) {
            self.valid = false;
            return self;
        }
        self.bytes.extend_from_slice(value.as_bytes());
        self.bytes.push(0);
        self
    }

    pub(crate) fn number(&mut self, value: u128) -> &mut Self {
        self.bytes.extend_from_slice(value.to_string().as_bytes());
        self.bytes.push(0);
        self
    }

    pub(crate) fn flag(&mut self, value: bool) -> &mut Self {
        self.text(if value { "true" } else { "false" })
    }

    pub(crate) fn finish(self) -> Option<Vec<u8>> {
        self.valid.then_some(self.bytes)
    }
}

/// How far the phased rollout has been opened. Only the control plane decides
/// this; the helper receives the decision inside the capability and refuses to
/// press Return unless the capability says it may.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SendRolloutStage {
    /// Rollout A: run the mechanical skill through both verification gates and
    /// stop before Return. Zero send risk.
    DryRun,
    /// Rollout B: send, but only to the account's own File Transfer surface.
    SelfSend,
    /// Rollout C: send to one reviewed, allow-listed peer under volume caps.
    AllowListed,
}

impl SendRolloutStage {
    /// Whether this stage may ever press Return.
    pub fn permits_return(self) -> bool {
        !matches!(self, SendRolloutStage::DryRun)
    }

    /// Canonical name used in signed bytes and audit evidence.
    pub fn canonical_name(self) -> &'static str {
        match self {
            SendRolloutStage::DryRun => "dryRun",
            SendRolloutStage::SelfSend => "selfSend",
            SendRolloutStage::AllowListed => "allowListed",
        }
    }
}

/// The step of the mechanical send skill that a helper run reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SendStage {
    Precheck,
    Calibrate,
    Address,
    RecipientVerify,
    Compose,
    ContentVerify,
    Send,
    SendVerify,
}

/// What the helper's own out-of-band capture proved immediately after Return.
/// This is never authoritative for delivery; `observedSent` is created only by
/// later replica reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VisualConfirmation {
    /// Return was never pressed.
    NotAttempted,
    /// The compose box cleared and the newest outgoing bubble matched.
    Confirmed,
    /// Return was pressed but the post-state could not be proven either way.
    Unconfirmed,
}

/// The user-facing failure taxonomy. Every variant keeps the send path closed
/// and maps to exactly one operator action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SendFailureCode {
    GrantsMissing,
    WechatNotRunning,
    NotLoggedIn,
    UnknownBuild,
    CalibrationDrift,
    RecipientVerifyFailed,
    ContentVerifyFailed,
    AddressingFocusFailed,
    AttachmentInvalid,
    AttachmentStagingFailed,
    AttachmentDigestMismatch,
    AttachmentVerifyFailed,
    AttachPanelNotPresented,
    UnsupportedAttachmentType,
    SendUnconfirmed,
    EngineStall,
    EngineUnavailable,
    HumanCollision,
    ManifestViolation,
    WindowNotFound,
    KillSwitchEngaged,
    StageNotPermitted,
    ConfigurationInvalid,
    ProfileInvalid,
    DraftInvalid,
    ApprovalInvalid,
    CapabilityExpired,
    CapabilityMismatch,
    IdempotencyConflict,
    RateLimited,
    CircuitOpen,
    OutboxBusy,
    ReconciliationPending,
}

impl SendFailureCode {
    /// The one action that resolves this failure, surfaced verbatim by
    /// `send doctor` and by the helper's own diagnostics.
    pub fn operator_action(self) -> &'static str {
        match self {
            SendFailureCode::GrantsMissing => {
                "Grant Accessibility and Screen Recording to GreenBubblesInputHelper, then re-run the capability probe."
            }
            SendFailureCode::WechatNotRunning => "Launch WeChat and leave it running in the background.",
            SendFailureCode::NotLoggedIn => "Log in to WeChat on this Mac.",
            SendFailureCode::UnknownBuild => {
                "This macOS/WeChat build pair is not in the signed compatibility matrix; wait for a validated matrix update."
            }
            SendFailureCode::CalibrationDrift => {
                "No verified calibration profile is active for this client build; run `send selftest`, and install a signed profile for this WeChat build if it fails."
            }
            SendFailureCode::RecipientVerifyFailed => {
                "The opened conversation did not match the approved recipient; re-resolve the recipient and approve a new draft."
            }
            SendFailureCode::ContentVerifyFailed => {
                "The composed text did not match the approved body; approve a new draft."
            }
            SendFailureCode::AddressingFocusFailed => {
                "The search box did not take focus, so the recipient was never addressed; nothing destructive was typed and the run stopped."
            }
            SendFailureCode::AttachmentInvalid => {
                "The draft's attachment is malformed, too large, or names a path outside its staging directory."
            }
            SendFailureCode::AttachmentStagingFailed => {
                "The approved file could not be staged; check that it is an owner-only regular file that still exists."
            }
            SendFailureCode::AttachmentDigestMismatch => {
                "The file on disk no longer matches the digest the draft approved; approve a new draft for the current file."
            }
            SendFailureCode::AttachmentVerifyFailed => {
                "The staged attachment's name was not read back from the compose area; nothing was sent."
            }
            SendFailureCode::AttachPanelNotPresented => {
                "The attach control did not open a file panel; the click was abandoned and the compose area left untouched."
            }
            SendFailureCode::UnsupportedAttachmentType => {
                "This attachment type is not in the reviewed set for the active calibration profile."
            }
            SendFailureCode::SendUnconfirmed => {
                "Return was pressed but the result is unproven; run send reconcile before any further attempt."
            }
            SendFailureCode::EngineStall => {
                "The input helper stalled and was abandoned; run send doctor and re-check the outbox before retrying."
            }
            SendFailureCode::EngineUnavailable => "The input helper is not reachable; run send doctor.",
            SendFailureCode::HumanCollision => {
                "Real user activity was observed on WeChat; the attempt yielded. Retry when the client is idle."
            }
            SendFailureCode::ManifestViolation => {
                "The helper refused an action outside its WeChat-scoped capability manifest; this is a defect, not a configuration issue."
            }
            SendFailureCode::WindowNotFound => "WeChat's main window was not found on screen.",
            SendFailureCode::KillSwitchEngaged => "The send path is disabled by the kill switch.",
            SendFailureCode::StageNotPermitted => {
                "The rollout stage does not permit sending to this conversation."
            }
            SendFailureCode::ConfigurationInvalid => "The send adapter configuration is invalid; run send doctor.",
            SendFailureCode::ProfileInvalid => {
                "The calibration profile or compatibility matrix is missing, unsigned, expired, or bound to another build."
            }
            SendFailureCode::DraftInvalid => "The draft is missing, malformed, stale, or expired.",
            SendFailureCode::ApprovalInvalid => "The approval evidence is missing, malformed, expired, or already consumed.",
            SendFailureCode::CapabilityExpired => "The minted capability expired before it reached the helper.",
            SendFailureCode::CapabilityMismatch => "The helper's outcome does not match the capability it was given.",
            SendFailureCode::IdempotencyConflict => "This idempotency key was already reserved; recovery reconciles, it never resends.",
            SendFailureCode::RateLimited => "The attempt window has no remaining capacity.",
            SendFailureCode::CircuitOpen => "The circuit breaker is open after consecutive failures.",
            SendFailureCode::OutboxBusy => "Another attempt is already in flight; the outbox is single-flight by design.",
            SendFailureCode::ReconciliationPending => {
                "A previous attempt is still awaiting reconciliation; resolve it before another send."
            }
        }
    }
}

/// How one reservation finished. Kept separate from the action lifecycle so a
/// deliberate dry run is never recorded as a failed send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SendCompletionKind {
    /// The mechanical skill ran through both gates and stopped before Return,
    /// exactly as the dry-run stage requires.
    DryRunCompleted,
    /// Replica reconciliation proved the message exists.
    ObservedSent,
    /// The attempt provably did not send.
    ObservedFailed,
}

impl SendCompletionKind {
    /// The action-lifecycle state this completion creates, if any. A dry run
    /// never becomes `attempted`, so it creates none.
    pub fn lifecycle_state(self) -> Option<ActionLifecycleState> {
        match self {
            SendCompletionKind::DryRunCompleted => None,
            SendCompletionKind::ObservedSent => Some(ActionLifecycleState::ObservedSent),
            SendCompletionKind::ObservedFailed => Some(ActionLifecycleState::ObservedFailed),
        }
    }

    /// Whether the circuit breaker should treat this completion as healthy.
    pub fn healthy(self) -> bool {
        matches!(
            self,
            SendCompletionKind::DryRunCompleted | SendCompletionKind::ObservedSent
        )
    }
}

/// Either a finished reservation or one parked for deferred reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "completion")]
pub enum SendCompletionOutcome {
    Completed(SendCompletionKind),
    /// Return may have been pressed and the result is unproven. The entry is
    /// parked; recovery reconciles against the replica and never resends.
    AwaitingReconciliation,
}

/// One staged local attachment. The control plane has already copied the
/// approved file into a single-use staging directory and re-hashed *that copy*,
/// so the digest here describes the exact bytes the helper will hand over and
/// nothing can be swapped underneath it afterwards.
///
/// The helper never reads the file. It writes a *reference* to the pasteboard
/// (or types the path into an open panel), so the bytes travel from the
/// filesystem to WeChat without passing through the process that holds the
/// input and capture grants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionAttachment {
    /// The single-use directory the control plane created for this action. It
    /// is the only path the helper may touch.
    pub staging_directory: String,
    /// The staged copy, always `<staging_directory>/<display_file_name>`.
    pub staged_path: String,
    pub display_file_name: String,
    pub byte_count: u64,
    /// Digest of the staged copy, re-verified from that copy before minting.
    pub sha256: String,
    pub uniform_type_identifier: String,
}

impl ActionAttachment {
    /// Structural validation performed independently on both sides. The path
    /// containment check is what keeps a compromised control plane from
    /// pointing the helper at an arbitrary file.
    pub fn validate(&self) -> Result<(), SendFailureCode> {
        let name_valid = !self.display_file_name.is_empty()
            && self.display_file_name.len() <= MAXIMUM_DISPLAY_FILE_NAME_BYTES
            && !self.display_file_name.contains('/')
            && !self.display_file_name.contains('\0')
            && !matches!(self.display_file_name.as_str(), "." | "..");
        let expected_path = format!("{}/{}", self.staging_directory, self.display_file_name);
        if !name_valid
            || !is_sha256(&self.sha256)
            || self.byte_count == 0
            || self.byte_count > MAXIMUM_ATTACHMENT_BYTES
            || self.uniform_type_identifier.is_empty()
            || !self.staging_directory.starts_with('/')
            || self.staging_directory.contains("/..")
            || self.staged_path != expected_path
        {
            return Err(SendFailureCode::AttachmentInvalid);
        }
        Ok(())
    }
}

/// The single-use, bound action capability handed to the helper. It carries no
/// key, no replica handle, and no policy: the control plane has already
/// resolved the recipient, so the helper can enforce the recipient gate with
/// nothing but this document and its own capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionCapabilityEnvelope {
    pub format_version: u32,
    pub capability_id: String,
    pub action_id: String,
    pub draft_id: String,
    pub approval_id: String,
    pub idempotency_key: String,
    pub account_id: String,
    pub conversation_id: String,
    /// Which reviewed capability this action exercises. Text carries a body;
    /// image and file carry an attachment. Never both.
    pub capability: ActionCapability,
    /// What to type into the search box to address the conversation.
    pub search_key: String,
    /// GATE 1: the opened conversation title must equal this.
    pub expected_title: String,
    /// GATE 2 and GATE 3: the exact text to paste and to confirm. Empty for an
    /// attachment send.
    pub body: String,
    pub body_sha256: String,
    /// Digest of the body after `normalized_send_text`; both the on-screen
    /// gates and replica reconciliation compare against this, never the raw
    /// bytes, because rendering and re-decoding both fold whitespace.
    pub normalized_body_sha256: String,
    pub client_build_profile_id: String,
    pub calibration_profile_id: String,
    pub calibration_profile_sha256: String,
    /// Present exactly when `capability` carries an attachment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<ActionAttachment>,
    pub rollout_stage: SendRolloutStage,
    /// False in every dry run: the helper must stop before Return.
    pub permit_send: bool,
    pub issued_at_unix_nanoseconds: u128,
    pub valid_until_unix_nanoseconds: u128,
    pub binding_sha256: String,
}

/// The exact bytes summarized by `binding_sha256`.
pub fn capability_binding_bytes(capability: &ActionCapabilityEnvelope) -> Option<Vec<u8>> {
    let mut writer = CanonicalWriter::new(ACTION_CAPABILITY_DOMAIN);
    writer
        .number(u128::from(capability.format_version))
        .text(&capability.capability_id)
        .text(&capability.action_id)
        .text(&capability.draft_id)
        .text(&capability.approval_id)
        .text(&capability.idempotency_key)
        .text(&capability.account_id)
        .text(&capability.conversation_id)
        .text(action_capability_name(capability.capability))
        .text(&capability.search_key)
        .text(&capability.expected_title)
        .text(&capability.body_sha256)
        .text(&capability.normalized_body_sha256)
        .text(&capability.client_build_profile_id)
        .text(&capability.calibration_profile_id)
        .text(&capability.calibration_profile_sha256)
        .flag(capability.attachment.is_some());
    if let Some(attachment) = &capability.attachment {
        writer
            .text(&attachment.staging_directory)
            .text(&attachment.staged_path)
            .text(&attachment.display_file_name)
            .number(u128::from(attachment.byte_count))
            .text(&attachment.sha256)
            .text(&attachment.uniform_type_identifier);
    }
    writer
        .text(capability.rollout_stage.canonical_name())
        .flag(capability.permit_send)
        .number(capability.issued_at_unix_nanoseconds)
        .number(capability.valid_until_unix_nanoseconds);
    writer.finish()
}

/// Recomputes a capability's binding digest.
pub fn capability_binding_sha256(capability: &ActionCapabilityEnvelope) -> Option<String> {
    capability_binding_bytes(capability).map(|bytes| hex::encode(Sha256::digest(bytes)))
}

impl ActionCapabilityEnvelope {
    /// Structural and temporal validation performed independently by both the
    /// control plane (before dispatch) and the helper (on receipt).
    pub fn validate(&self, now_unix_nanoseconds: u128) -> Result<(), SendFailureCode> {
        if self.format_version != SEND_CONTRACT_VERSION
            || !is_sha256(&self.capability_id)
            || !is_sha256(&self.action_id)
            || !is_sha256(&self.draft_id)
            || !is_sha256(&self.approval_id)
            || !is_sha256(&self.idempotency_key)
            || !is_sha256(&self.body_sha256)
            || !is_sha256(&self.normalized_body_sha256)
            || !is_sha256(&self.calibration_profile_sha256)
            || self.account_id.is_empty()
            || self.conversation_id.is_empty()
            || self.client_build_profile_id.is_empty()
            || self.calibration_profile_id.is_empty()
            || self.search_key.is_empty()
            || self.search_key.len() > MAXIMUM_SEARCH_KEY_BYTES
            || self.expected_title.is_empty()
            || self.expected_title.len() > MAXIMUM_EXPECTED_TITLE_BYTES
            || self.body.len() > MAXIMUM_SEND_BODY_BYTES
            || self.issued_at_unix_nanoseconds >= self.valid_until_unix_nanoseconds
            || hex::encode(Sha256::digest(self.body.as_bytes())) != self.body_sha256
            || hex::encode(Sha256::digest(normalized_send_text(&self.body).as_bytes()))
                != self.normalized_body_sha256
            || capability_binding_sha256(self).as_deref() != Some(self.binding_sha256.as_str())
            || (self.permit_send && !self.rollout_stage.permits_return())
        {
            return Err(SendFailureCode::CapabilityMismatch);
        }
        // A capability carries a body or an attachment, never both and never
        // neither. Captions are a separate capability, so an attachment send
        // has no text at all and a text send has no file.
        match (self.capability.carries_attachment(), &self.attachment) {
            (false, None) if !self.body.is_empty() => {}
            (true, Some(attachment)) if self.body.is_empty() => attachment.validate()?,
            _ => return Err(SendFailureCode::CapabilityMismatch),
        }
        if now_unix_nanoseconds < self.issued_at_unix_nanoseconds
            || now_unix_nanoseconds >= self.valid_until_unix_nanoseconds
        {
            return Err(SendFailureCode::CapabilityExpired);
        }
        Ok(())
    }
}

/// Body-free evidence from the helper's own captures. It carries match
/// decisions and confidences, never recognized text, so the audit journal
/// stays free of message content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HelperGateEvidence {
    pub title_confidence_parts_per_million: u32,
    pub title_matched: bool,
    /// GATE 0: the search key was read back out of the search field, proving
    /// the click took focus there and not somewhere the user was working.
    pub search_key_echoed: bool,
    pub compose_matched: bool,
    /// GATE 2a: the staged attachment's display name was read back in the
    /// compose region. False for a text send.
    pub attachment_name_matched: bool,
    /// Whether the compose region showed a staged attachment at all.
    pub attachment_staged: bool,
    /// Whether a send-confirmation sheet was observed and confirmed.
    pub confirmation_sheet_confirmed: bool,
    pub compose_cleared: bool,
    pub newest_outgoing_matched: bool,
    pub ambiguous_search_result: bool,
    pub human_activity_observed: bool,
    pub window_frame_digest: String,
    pub capture_count: u32,
    pub elapsed_milliseconds: u64,
}

/// What one `execute_send` run reports back. `attempted` means Return was
/// pressed; nothing here may ever be read as proof of delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HelperSendOutcome {
    pub format_version: u32,
    pub capability_id: String,
    pub capability_binding_sha256: String,
    pub helper_version: String,
    pub engine_version: String,
    pub calibration_profile_id: String,
    pub stage_reached: SendStage,
    pub attempted: bool,
    pub visual_confirmation: VisualConfirmation,
    pub failure: Option<SendFailureCode>,
    pub evidence: HelperGateEvidence,
    pub observed_at_unix_nanoseconds: u128,
}

impl HelperSendOutcome {
    /// Checks that an outcome actually belongs to the capability that was
    /// dispatched and is internally consistent. A helper that claims a send a
    /// dry-run capability forbade is treated as a mismatch, not as a send.
    pub fn validate_against(
        &self,
        capability: &ActionCapabilityEnvelope,
    ) -> Result<(), SendFailureCode> {
        if self.format_version != SEND_CONTRACT_VERSION
            || self.capability_id != capability.capability_id
            || self.capability_binding_sha256 != capability.binding_sha256
            || self.calibration_profile_id != capability.calibration_profile_id
            || self.helper_version.is_empty()
            || self.engine_version.is_empty()
            || self.evidence.title_confidence_parts_per_million > 1_000_000
        {
            return Err(SendFailureCode::CapabilityMismatch);
        }
        let consistent = matches!(
            (self.attempted, self.visual_confirmation),
            (false, VisualConfirmation::NotAttempted)
                | (
                    true,
                    VisualConfirmation::Confirmed | VisualConfirmation::Unconfirmed
                )
        );
        if !consistent || (self.attempted && !capability.permit_send) {
            return Err(SendFailureCode::CapabilityMismatch);
        }
        if !self.attempted
            && self.failure.is_none()
            && self.stage_reached != SendStage::ContentVerify
        {
            // A run that neither pressed Return nor failed must have stopped
            // exactly where the dry run is defined to stop.
            return Err(SendFailureCode::CapabilityMismatch);
        }
        Ok(())
    }
}

/// The helper's read-only preflight answer. It is the only way the control
/// plane learns about TCC grants and live client state, because the control
/// plane deliberately holds no input or capture grants of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HelperCapabilityStatus {
    pub format_version: u32,
    pub helper_version: String,
    pub engine_version: String,
    pub accessibility_granted: bool,
    pub screen_recording_granted: bool,
    pub wechat_running: bool,
    pub wechat_logged_in: bool,
    pub wechat_bundle_identifier: String,
    pub wechat_marketing_version: String,
    pub wechat_build: String,
    pub macos_build: String,
    pub macos_major: u32,
    pub main_window_found: bool,
    pub active_calibration_profile_id: String,
    pub engine_healthy: bool,
    pub bounded_manifest_scope: Vec<String>,
    pub observed_at_unix_nanoseconds: u128,
}

impl HelperCapabilityStatus {
    /// The first blocking reason the live environment gives, in the order the
    /// operator should fix them. `None` means the environment is ready.
    pub fn blocking_failure(&self) -> Option<SendFailureCode> {
        if self.format_version != SEND_CONTRACT_VERSION
            || self.helper_version.is_empty()
            || self.engine_version.is_empty()
        {
            return Some(SendFailureCode::ConfigurationInvalid);
        }
        if !self.accessibility_granted || !self.screen_recording_granted {
            return Some(SendFailureCode::GrantsMissing);
        }
        if !self.engine_healthy {
            return Some(SendFailureCode::EngineUnavailable);
        }
        if !self.wechat_running {
            return Some(SendFailureCode::WechatNotRunning);
        }
        if !self.wechat_logged_in {
            return Some(SendFailureCode::NotLoggedIn);
        }
        if !self.main_window_found {
            return Some(SendFailureCode::WindowNotFound);
        }
        if self.bounded_manifest_scope.is_empty()
            || self
                .bounded_manifest_scope
                .iter()
                .any(|entry| entry.is_empty())
        {
            return Some(SendFailureCode::ManifestViolation);
        }
        None
    }
}

/// The result of one calibration self-test: locate and focus the search box,
/// confirm by capture, and never send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CalibrationSelfTestReport {
    pub format_version: u32,
    pub calibration_profile_id: String,
    pub passed: bool,
    pub search_box_focused: bool,
    pub title_confidence_parts_per_million: u32,
    pub window_frame_digest: String,
    pub drift_report: Vec<String>,
    pub failure: Option<SendFailureCode>,
    pub observed_at_unix_nanoseconds: u128,
}

pub(crate) fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sha(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    pub(crate) fn capability(permit_send: bool) -> ActionCapabilityEnvelope {
        let body = "hello from the adapter".to_string();
        let mut capability = ActionCapabilityEnvelope {
            format_version: SEND_CONTRACT_VERSION,
            capability_id: sha('1'),
            action_id: sha('2'),
            draft_id: sha('3'),
            approval_id: sha('4'),
            idempotency_key: sha('5'),
            account_id: "account".to_string(),
            conversation_id: "filehelper".to_string(),
            capability: ActionCapability::TextSend,
            search_key: "File Transfer".to_string(),
            expected_title: "File Transfer".to_string(),
            body_sha256: hex::encode(Sha256::digest(body.as_bytes())),
            normalized_body_sha256: normalized_send_text_sha256(&body),
            body,
            client_build_profile_id: "wechat-macos-4.1.13-269579".to_string(),
            calibration_profile_id: "wechat-4.1.13.269579-macos-26".to_string(),
            calibration_profile_sha256: sha('6'),
            attachment: None,
            rollout_stage: if permit_send {
                SendRolloutStage::SelfSend
            } else {
                SendRolloutStage::DryRun
            },
            permit_send,
            issued_at_unix_nanoseconds: 1_000,
            valid_until_unix_nanoseconds: 9_000,
            binding_sha256: String::new(),
        };
        capability.binding_sha256 = capability_binding_sha256(&capability).unwrap();
        capability
    }

    fn outcome(capability: &ActionCapabilityEnvelope, attempted: bool) -> HelperSendOutcome {
        HelperSendOutcome {
            format_version: SEND_CONTRACT_VERSION,
            capability_id: capability.capability_id.clone(),
            capability_binding_sha256: capability.binding_sha256.clone(),
            helper_version: "1.0.0".to_string(),
            engine_version: "1.0.0".to_string(),
            calibration_profile_id: capability.calibration_profile_id.clone(),
            stage_reached: if attempted {
                SendStage::SendVerify
            } else {
                SendStage::ContentVerify
            },
            attempted,
            visual_confirmation: if attempted {
                VisualConfirmation::Confirmed
            } else {
                VisualConfirmation::NotAttempted
            },
            failure: None,
            evidence: HelperGateEvidence {
                title_confidence_parts_per_million: 1_000_000,
                title_matched: true,
                search_key_echoed: true,
                compose_matched: true,
                attachment_name_matched: false,
                attachment_staged: false,
                confirmation_sheet_confirmed: false,
                compose_cleared: attempted,
                newest_outgoing_matched: attempted,
                ambiguous_search_result: false,
                human_activity_observed: false,
                window_frame_digest: sha('7'),
                capture_count: 4,
                elapsed_milliseconds: 1_200,
            },
            observed_at_unix_nanoseconds: 2_000,
        }
    }

    #[test]
    fn a_well_formed_capability_validates_inside_its_window_only() {
        let capability = capability(false);
        assert!(capability.validate(2_000).is_ok());
        assert_eq!(
            capability.validate(999).unwrap_err(),
            SendFailureCode::CapabilityExpired
        );
        assert_eq!(
            capability.validate(9_000).unwrap_err(),
            SendFailureCode::CapabilityExpired
        );
    }

    #[test]
    fn every_bound_capability_field_changes_the_binding_digest() {
        let capability = capability(true);
        let original = capability.binding_sha256.clone();
        type Mutation = Box<dyn Fn(&mut ActionCapabilityEnvelope)>;
        let mutations: Vec<Mutation> = vec![
            Box::new(|value| value.capability_id = sha('a')),
            Box::new(|value| value.action_id = sha('a')),
            Box::new(|value| value.draft_id = sha('a')),
            Box::new(|value| value.approval_id = sha('a')),
            Box::new(|value| value.idempotency_key = sha('a')),
            Box::new(|value| value.account_id.push('x')),
            Box::new(|value| value.conversation_id.push('x')),
            Box::new(|value| value.capability = ActionCapability::FileSend),
            Box::new(|value| value.search_key.push('x')),
            Box::new(|value| value.expected_title.push('x')),
            Box::new(|value| value.body_sha256 = sha('a')),
            Box::new(|value| value.normalized_body_sha256 = sha('a')),
            Box::new(|value| value.client_build_profile_id.push('x')),
            Box::new(|value| value.calibration_profile_id.push('x')),
            Box::new(|value| value.calibration_profile_sha256 = sha('a')),
            Box::new(|value| value.rollout_stage = SendRolloutStage::AllowListed),
            Box::new(|value| value.permit_send = false),
            Box::new(|value| value.issued_at_unix_nanoseconds += 1),
            Box::new(|value| value.valid_until_unix_nanoseconds += 1),
        ];
        for mutation in mutations {
            let mut variant = capability.clone();
            mutation(&mut variant);
            assert_ne!(capability_binding_sha256(&variant).unwrap(), original);
            assert_eq!(
                variant.validate(2_000).unwrap_err(),
                SendFailureCode::CapabilityMismatch
            );
        }
    }

    #[test]
    fn a_dry_run_capability_can_never_be_marked_send_permitting() {
        let mut capability = capability(false);
        capability.permit_send = true;
        capability.binding_sha256 = capability_binding_sha256(&capability).unwrap();
        assert_eq!(
            capability.validate(2_000).unwrap_err(),
            SendFailureCode::CapabilityMismatch
        );
    }

    #[test]
    fn an_outcome_claiming_a_send_a_dry_run_forbade_is_refused() {
        let capability = capability(false);
        let claimed = outcome(&capability, true);
        assert_eq!(
            claimed.validate_against(&capability).unwrap_err(),
            SendFailureCode::CapabilityMismatch
        );
        assert!(outcome(&capability, false)
            .validate_against(&capability)
            .is_ok());
    }

    #[test]
    fn an_outcome_for_a_different_capability_is_refused() {
        let capability = capability(true);
        let mut foreign = outcome(&capability, true);
        foreign.capability_binding_sha256 = sha('b');
        assert_eq!(
            foreign.validate_against(&capability).unwrap_err(),
            SendFailureCode::CapabilityMismatch
        );
    }

    #[test]
    fn helper_status_reports_the_first_blocking_reason_in_repair_order() {
        let ready = HelperCapabilityStatus {
            format_version: SEND_CONTRACT_VERSION,
            helper_version: "1.0.0".to_string(),
            engine_version: "1.0.0".to_string(),
            accessibility_granted: true,
            screen_recording_granted: true,
            wechat_running: true,
            wechat_logged_in: true,
            wechat_bundle_identifier: "com.tencent.xinWeChat".to_string(),
            wechat_marketing_version: "4.1.13".to_string(),
            wechat_build: "4.1.13.269579".to_string(),
            macos_build: "25G83".to_string(),
            macos_major: 26,
            main_window_found: true,
            active_calibration_profile_id: "wechat-4.1.13.269579-macos-26".to_string(),
            engine_healthy: true,
            bounded_manifest_scope: vec!["com.tencent.xinWeChat".to_string()],
            observed_at_unix_nanoseconds: 1,
        };
        assert!(ready.blocking_failure().is_none());
        for (mutate, expected) in [
            (
                Box::new(|status: &mut HelperCapabilityStatus| status.accessibility_granted = false)
                    as Box<dyn Fn(&mut HelperCapabilityStatus)>,
                SendFailureCode::GrantsMissing,
            ),
            (
                Box::new(|status: &mut HelperCapabilityStatus| status.engine_healthy = false),
                SendFailureCode::EngineUnavailable,
            ),
            (
                Box::new(|status: &mut HelperCapabilityStatus| status.wechat_running = false),
                SendFailureCode::WechatNotRunning,
            ),
            (
                Box::new(|status: &mut HelperCapabilityStatus| status.wechat_logged_in = false),
                SendFailureCode::NotLoggedIn,
            ),
            (
                Box::new(|status: &mut HelperCapabilityStatus| status.main_window_found = false),
                SendFailureCode::WindowNotFound,
            ),
            (
                Box::new(|status: &mut HelperCapabilityStatus| {
                    status.bounded_manifest_scope.clear()
                }),
                SendFailureCode::ManifestViolation,
            ),
        ] {
            let mut status = ready.clone();
            mutate(&mut status);
            assert_eq!(status.blocking_failure(), Some(expected));
        }
    }

    #[test]
    fn every_failure_code_names_one_operator_action() {
        for code in [
            SendFailureCode::GrantsMissing,
            SendFailureCode::WechatNotRunning,
            SendFailureCode::NotLoggedIn,
            SendFailureCode::UnknownBuild,
            SendFailureCode::CalibrationDrift,
            SendFailureCode::RecipientVerifyFailed,
            SendFailureCode::ContentVerifyFailed,
            SendFailureCode::SendUnconfirmed,
            SendFailureCode::EngineStall,
            SendFailureCode::EngineUnavailable,
            SendFailureCode::HumanCollision,
            SendFailureCode::ManifestViolation,
            SendFailureCode::WindowNotFound,
            SendFailureCode::KillSwitchEngaged,
            SendFailureCode::StageNotPermitted,
            SendFailureCode::ConfigurationInvalid,
            SendFailureCode::ProfileInvalid,
            SendFailureCode::DraftInvalid,
            SendFailureCode::ApprovalInvalid,
            SendFailureCode::CapabilityExpired,
            SendFailureCode::CapabilityMismatch,
            SendFailureCode::IdempotencyConflict,
            SendFailureCode::RateLimited,
            SendFailureCode::CircuitOpen,
            SendFailureCode::OutboxBusy,
            SendFailureCode::ReconciliationPending,
        ] {
            assert!(code.operator_action().len() > 16, "{code:?}");
        }
    }
}
