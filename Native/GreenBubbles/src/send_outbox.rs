//! The adapter-owned atomic reservation store: a durable, single-flight outbox
//! that makes a double send impossible across crashes and restarts.
//!
//! The safety contract requires that the idempotency key be *reserved before*
//! Return and consumed only after reconciliation. This module is where that
//! happens. The control plane persists a reservation, persists the transition
//! to `attempted` before dispatching a send-permitting capability, and only
//! then hands the capability to the helper. Consequently a crash can leave the
//! store in exactly two states, and both are recoverable without resending:
//!
//! * `reserved` — persisted before dispatch, so Return provably never
//!   happened; recovery resolves it as `observedFailed`.
//! * `attempted` — dispatch may have pressed Return; recovery moves it to
//!   `awaitingReconciliation`, where it blocks further sends until the replica
//!   answers. Recovery never re-dispatches.
//!
//! Every mutation runs inside one exclusive `flock` transaction and is made
//! durable with a write-to-temporary, `fsync`, `rename`, `fsync`-directory
//! sequence, so a torn write can never be observed.

use std::collections::{BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::action::ActionCapability;
use crate::archive::ensure_private_directory;
use crate::send_contract::{
    is_sha256, SendCompletionKind, SendCompletionOutcome, SendFailureCode, SendRolloutStage,
    VisualConfirmation,
};
use crate::RestoreError;

/// Format version of the persisted outbox document.
pub const SEND_OUTBOX_FORMAT_VERSION: u32 = 1;
/// Hard ceiling on remembered idempotency keys; keys are never reusable.
pub const MAXIMUM_RESERVED_IDEMPOTENCY_KEYS: usize = 100_000;
/// Hard ceiling on remembered completions.
pub const MAXIMUM_COMPLETION_HISTORY: usize = 512;
/// Hard ceiling on unresolved attempts. Reaching it disables new sends.
pub const MAXIMUM_PENDING_RECONCILIATION: usize = 16;
/// Hard ceiling on the persisted document.
pub const MAXIMUM_OUTBOX_BYTES: u64 = 8 * 1024 * 1024;

const STATE_FILE_NAME: &str = "outbox.json";

/// Text sends preserve their bytes trivially; the flag only ever goes false for
/// an image send, so absence in an older document means true.
fn default_true() -> bool {
    true
}
const LOCK_FILE_NAME: &str = "outbox.lock";

/// Where one reservation stands. The state is persisted before the action it
/// describes, never after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutboxEntryState {
    /// Persisted before dispatch. Return provably has not happened.
    Reserved,
    /// Persisted before dispatching a send-permitting capability.
    Attempted,
    /// Recovered or inconclusive; blocks new sends until reconciled.
    AwaitingReconciliation,
}

/// One reservation. It carries no message body: only the body digest, so the
/// durable store never holds message content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OutboxEntry {
    pub action_id: String,
    pub draft_id: String,
    pub approval_id: String,
    pub idempotency_key: String,
    pub capability_id: String,
    pub capability_binding_sha256: String,
    pub account_id: String,
    pub conversation_id: String,
    pub body_sha256: String,
    pub normalized_body_sha256: String,
    pub capability: ActionCapability,
    /// Set for an attachment send. The outbox records the digest and the name,
    /// never the bytes, and remembers the staging directory so a terminal
    /// state can delete it even after a restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staging_directory: Option<String>,
    /// False for an image send, whose transmitted bytes are a client-produced
    /// derivative of the approved file.
    #[serde(default = "default_true")]
    pub bytes_preserved_in_transit: bool,
    pub rollout_stage: SendRolloutStage,
    pub permit_send: bool,
    pub state: OutboxEntryState,
    pub reserved_at_unix_nanoseconds: u128,
    pub attempted_at_unix_nanoseconds: Option<u128>,
    pub deadline_unix_nanoseconds: u128,
}

impl OutboxEntry {
    fn structurally_valid(&self) -> bool {
        is_sha256(&self.action_id)
            && is_sha256(&self.draft_id)
            && is_sha256(&self.approval_id)
            && is_sha256(&self.idempotency_key)
            && is_sha256(&self.capability_id)
            && is_sha256(&self.capability_binding_sha256)
            && is_sha256(&self.body_sha256)
            && is_sha256(&self.normalized_body_sha256)
            && self
                .attachment_sha256
                .as_ref()
                .is_none_or(|value| is_sha256(value))
            && (self.capability.carries_attachment() == self.attachment_sha256.is_some())
            && (self.attachment_sha256.is_some() == self.staging_directory.is_some())
            && (self.attachment_sha256.is_some() == self.display_file_name.is_some())
            && !self.account_id.is_empty()
            && !self.conversation_id.is_empty()
            && self.reserved_at_unix_nanoseconds < self.deadline_unix_nanoseconds
            && (!self.permit_send || self.rollout_stage.permits_return())
    }
}

/// A terminal record. `completion` is a send-completion kind rather than a
/// delivery status, so the outbox can never invent a semantic the action
/// safety contract does not model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OutboxCompletion {
    pub action_id: String,
    pub draft_id: String,
    pub approval_id: String,
    pub idempotency_key: String,
    pub conversation_id: String,
    pub body_sha256: String,
    pub normalized_body_sha256: String,
    pub capability: ActionCapability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_file_name: Option<String>,
    #[serde(default = "default_true")]
    pub bytes_preserved_in_transit: bool,
    pub rollout_stage: SendRolloutStage,
    pub attempted: bool,
    pub visual_confirmation: VisualConfirmation,
    pub completion: SendCompletionKind,
    pub failure: Option<SendFailureCode>,
    pub reconciled_by_replica: bool,
    pub completed_at_unix_nanoseconds: u128,
}

/// What the control plane observed about one finished dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SendCompletionRecord {
    pub outcome: SendCompletionOutcome,
    pub attempted: bool,
    pub visual_confirmation: VisualConfirmation,
    pub failure: Option<SendFailureCode>,
    pub reconciled_by_replica: bool,
}

/// The attempt-window capacity the safety contract requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SendRateWindow {
    pub started_at_unix_nanoseconds: u128,
    pub ends_at_unix_nanoseconds: u128,
    pub maximum_attempts: u64,
    pub reserved_attempts: u64,
}

impl SendRateWindow {
    /// Rolls the window forward when it has elapsed, leaving an in-progress
    /// window untouched.
    pub fn rolled(
        self,
        now_unix_nanoseconds: u128,
        window_nanoseconds: u128,
        maximum_attempts: u64,
    ) -> Self {
        if now_unix_nanoseconds >= self.ends_at_unix_nanoseconds
            || now_unix_nanoseconds < self.started_at_unix_nanoseconds
        {
            return Self {
                started_at_unix_nanoseconds: now_unix_nanoseconds,
                ends_at_unix_nanoseconds: now_unix_nanoseconds
                    .saturating_add(window_nanoseconds.max(1)),
                maximum_attempts,
                reserved_attempts: 0,
            };
        }
        Self {
            maximum_attempts,
            ..self
        }
    }
}

/// Trips after N consecutive gate or engine failures and stays open for a
/// cooldown, so a systematically failing environment cannot be hammered.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SendCircuitBreaker {
    pub consecutive_failure_count: u32,
    pub opened_at_unix_nanoseconds: Option<u128>,
    pub open_until_unix_nanoseconds: Option<u128>,
    pub last_failure: Option<SendFailureCode>,
}

impl SendCircuitBreaker {
    /// Whether the breaker currently forbids new attempts.
    pub fn open(&self, now_unix_nanoseconds: u128) -> bool {
        self.open_until_unix_nanoseconds
            .is_some_and(|until| now_unix_nanoseconds < until)
    }

    fn record_failure(
        &mut self,
        now_unix_nanoseconds: u128,
        failure_threshold: u32,
        cooldown_nanoseconds: u128,
        failure: Option<SendFailureCode>,
    ) {
        self.consecutive_failure_count = self.consecutive_failure_count.saturating_add(1);
        self.last_failure = failure;
        if failure_threshold > 0 && self.consecutive_failure_count >= failure_threshold {
            self.opened_at_unix_nanoseconds = Some(now_unix_nanoseconds);
            self.open_until_unix_nanoseconds =
                Some(now_unix_nanoseconds.saturating_add(cooldown_nanoseconds));
        }
    }

    fn record_success(&mut self) {
        self.consecutive_failure_count = 0;
        self.opened_at_unix_nanoseconds = None;
        self.open_until_unix_nanoseconds = None;
        self.last_failure = None;
    }
}

/// The persisted outbox document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SendOutboxState {
    pub format_version: u32,
    pub account_id: String,
    pub rate_window: SendRateWindow,
    pub circuit_breaker: SendCircuitBreaker,
    pub in_flight: Option<OutboxEntry>,
    pub pending_reconciliation: Vec<OutboxEntry>,
    pub reserved_idempotency_keys: BTreeSet<String>,
    pub consumed_approval_ids: BTreeSet<String>,
    pub completions: VecDeque<OutboxCompletion>,
    pub reserved_attempt_total: u64,
    pub completed_attempt_total: u64,
    pub recovered_attempt_total: u64,
    pub updated_at_unix_nanoseconds: u128,
}

impl SendOutboxState {
    fn new(account_id: &str, now_unix_nanoseconds: u128) -> Self {
        Self {
            format_version: SEND_OUTBOX_FORMAT_VERSION,
            account_id: account_id.to_string(),
            // An already-elapsed window, so the first reservation always
            // opens a full-length one under the configured capacity.
            rate_window: SendRateWindow {
                started_at_unix_nanoseconds: now_unix_nanoseconds,
                ends_at_unix_nanoseconds: now_unix_nanoseconds,
                maximum_attempts: 0,
                reserved_attempts: 0,
            },
            circuit_breaker: SendCircuitBreaker::default(),
            in_flight: None,
            pending_reconciliation: Vec::new(),
            reserved_idempotency_keys: BTreeSet::new(),
            consumed_approval_ids: BTreeSet::new(),
            completions: VecDeque::new(),
            reserved_attempt_total: 0,
            completed_attempt_total: 0,
            recovered_attempt_total: 0,
            updated_at_unix_nanoseconds: now_unix_nanoseconds,
        }
    }

    fn validate(&self, account_id: &str) -> Result<(), RestoreError> {
        if self.format_version != SEND_OUTBOX_FORMAT_VERSION {
            return Err(RestoreError::Integrity(
                "send outbox format version is unsupported".to_string(),
            ));
        }
        if self.account_id != account_id {
            return Err(RestoreError::Integrity(
                "send outbox belongs to a different account".to_string(),
            ));
        }
        if self.reserved_idempotency_keys.len() > MAXIMUM_RESERVED_IDEMPOTENCY_KEYS
            || self.completions.len() > MAXIMUM_COMPLETION_HISTORY
            || self.pending_reconciliation.len() > MAXIMUM_PENDING_RECONCILIATION
        {
            return Err(RestoreError::Integrity(
                "send outbox exceeds its bounded capacity".to_string(),
            ));
        }
        for entry in self.in_flight.iter().chain(&self.pending_reconciliation) {
            if !entry.structurally_valid()
                || entry.account_id != account_id
                || !self
                    .reserved_idempotency_keys
                    .contains(&entry.idempotency_key)
            {
                return Err(RestoreError::Integrity(
                    "send outbox entry is malformed or unreserved".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// Refuses a new reservation while anything is in flight, unresolved, out
    /// of window capacity, or behind an open breaker.
    pub fn admission_failure(&self, now_unix_nanoseconds: u128) -> Option<SendFailureCode> {
        if self.in_flight.is_some() {
            return Some(SendFailureCode::OutboxBusy);
        }
        if !self.pending_reconciliation.is_empty() {
            return Some(SendFailureCode::ReconciliationPending);
        }
        if self.circuit_breaker.open(now_unix_nanoseconds) {
            return Some(SendFailureCode::CircuitOpen);
        }
        None
    }

    /// Reserves one attempt. The caller must already have run PRECHECK; this
    /// is the atomic, durable half of that decision.
    pub fn reserve(
        &mut self,
        entry: OutboxEntry,
        now_unix_nanoseconds: u128,
        window_nanoseconds: u128,
        maximum_attempts: u64,
    ) -> Result<(), SendFailureCode> {
        if let Some(failure) = self.admission_failure(now_unix_nanoseconds) {
            return Err(failure);
        }
        if !entry.structurally_valid() || entry.account_id != self.account_id {
            return Err(SendFailureCode::ConfigurationInvalid);
        }
        if self
            .reserved_idempotency_keys
            .contains(&entry.idempotency_key)
        {
            return Err(SendFailureCode::IdempotencyConflict);
        }
        if self.consumed_approval_ids.contains(&entry.approval_id) {
            return Err(SendFailureCode::ApprovalInvalid);
        }
        if self.reserved_idempotency_keys.len() >= MAXIMUM_RESERVED_IDEMPOTENCY_KEYS {
            return Err(SendFailureCode::ConfigurationInvalid);
        }
        let window =
            self.rate_window
                .rolled(now_unix_nanoseconds, window_nanoseconds, maximum_attempts);
        if window.maximum_attempts == 0 || window.reserved_attempts >= window.maximum_attempts {
            return Err(SendFailureCode::RateLimited);
        }
        self.rate_window = SendRateWindow {
            reserved_attempts: window.reserved_attempts.saturating_add(1),
            ..window
        };
        self.reserved_idempotency_keys
            .insert(entry.idempotency_key.clone());
        self.reserved_attempt_total = self.reserved_attempt_total.saturating_add(1);
        self.in_flight = Some(OutboxEntry {
            state: OutboxEntryState::Reserved,
            ..entry
        });
        self.updated_at_unix_nanoseconds = now_unix_nanoseconds;
        Ok(())
    }

    /// Persists the transition that must precede any dispatch that is allowed
    /// to press Return.
    pub fn mark_attempted(
        &mut self,
        idempotency_key: &str,
        now_unix_nanoseconds: u128,
    ) -> Result<(), SendFailureCode> {
        let entry = self
            .in_flight
            .as_mut()
            .filter(|entry| entry.idempotency_key == idempotency_key)
            .ok_or(SendFailureCode::IdempotencyConflict)?;
        if !entry.permit_send || entry.state != OutboxEntryState::Reserved {
            return Err(SendFailureCode::IdempotencyConflict);
        }
        entry.state = OutboxEntryState::Attempted;
        entry.attempted_at_unix_nanoseconds = Some(now_unix_nanoseconds);
        self.updated_at_unix_nanoseconds = now_unix_nanoseconds;
        Ok(())
    }

    /// Completes the in-flight reservation. An `awaitingReconciliation`
    /// outcome parks the entry instead of finishing it.
    pub fn complete(
        &mut self,
        idempotency_key: &str,
        record: SendCompletionRecord,
        now_unix_nanoseconds: u128,
        failure_threshold: u32,
        cooldown_nanoseconds: u128,
    ) -> Result<(), SendFailureCode> {
        let entry = self
            .in_flight
            .take()
            .filter(|entry| entry.idempotency_key == idempotency_key)
            .ok_or(SendFailureCode::IdempotencyConflict)?;
        self.consumed_approval_ids.insert(entry.approval_id.clone());
        match record.outcome {
            SendCompletionOutcome::AwaitingReconciliation => {
                if self.pending_reconciliation.len() >= MAXIMUM_PENDING_RECONCILIATION {
                    self.in_flight = Some(entry);
                    return Err(SendFailureCode::ReconciliationPending);
                }
                self.pending_reconciliation.push(OutboxEntry {
                    state: OutboxEntryState::AwaitingReconciliation,
                    ..entry
                });
                self.circuit_breaker.record_failure(
                    now_unix_nanoseconds,
                    failure_threshold,
                    cooldown_nanoseconds,
                    record.failure,
                );
            }
            SendCompletionOutcome::Completed(kind) => {
                if kind.healthy() {
                    self.circuit_breaker.record_success();
                } else {
                    self.circuit_breaker.record_failure(
                        now_unix_nanoseconds,
                        failure_threshold,
                        cooldown_nanoseconds,
                        record.failure,
                    );
                }
                self.push_completion(completion(entry, kind, &record, now_unix_nanoseconds));
            }
        }
        self.updated_at_unix_nanoseconds = now_unix_nanoseconds;
        Ok(())
    }

    /// Resolves one parked attempt with authoritative replica evidence. This
    /// is the only path that can create `observedSent` after an unknown.
    pub fn resolve_pending(
        &mut self,
        idempotency_key: &str,
        kind: SendCompletionKind,
        reconciled_by_replica: bool,
        now_unix_nanoseconds: u128,
    ) -> Result<(), SendFailureCode> {
        if kind == SendCompletionKind::DryRunCompleted {
            return Err(SendFailureCode::ConfigurationInvalid);
        }
        let position = self
            .pending_reconciliation
            .iter()
            .position(|entry| entry.idempotency_key == idempotency_key)
            .ok_or(SendFailureCode::IdempotencyConflict)?;
        let entry = self.pending_reconciliation.remove(position);
        if kind.healthy() {
            self.circuit_breaker.record_success();
        }
        let attempted = entry.attempted_at_unix_nanoseconds.is_some();
        let record = SendCompletionRecord {
            outcome: SendCompletionOutcome::Completed(kind),
            attempted,
            visual_confirmation: VisualConfirmation::Unconfirmed,
            failure: None,
            reconciled_by_replica,
        };
        self.push_completion(completion(entry, kind, &record, now_unix_nanoseconds));
        self.updated_at_unix_nanoseconds = now_unix_nanoseconds;
        Ok(())
    }

    /// Reconciles an interrupted process. A reservation that never reached
    /// dispatch is closed as failed; a dispatched attempt is parked. Neither
    /// path ever re-dispatches.
    pub fn recover(&mut self, now_unix_nanoseconds: u128) -> SendOutboxRecovery {
        let Some(entry) = self.in_flight.take() else {
            return SendOutboxRecovery {
                recovered_reservation: false,
                recovered_attempt: false,
                idempotency_key: None,
                pending_reconciliation_count: self.pending_reconciliation.len() as u64,
            };
        };
        self.recovered_attempt_total = self.recovered_attempt_total.saturating_add(1);
        self.consumed_approval_ids.insert(entry.approval_id.clone());
        let idempotency_key = entry.idempotency_key.clone();
        let dispatched = entry.state == OutboxEntryState::Attempted;
        if dispatched {
            self.pending_reconciliation.push(OutboxEntry {
                state: OutboxEntryState::AwaitingReconciliation,
                ..entry
            });
        } else {
            let record = SendCompletionRecord {
                outcome: SendCompletionOutcome::Completed(SendCompletionKind::ObservedFailed),
                attempted: false,
                visual_confirmation: VisualConfirmation::NotAttempted,
                failure: Some(SendFailureCode::EngineStall),
                reconciled_by_replica: false,
            };
            self.push_completion(completion(
                entry,
                SendCompletionKind::ObservedFailed,
                &record,
                now_unix_nanoseconds,
            ));
        }
        self.updated_at_unix_nanoseconds = now_unix_nanoseconds;
        SendOutboxRecovery {
            recovered_reservation: !dispatched,
            recovered_attempt: dispatched,
            idempotency_key: Some(idempotency_key),
            pending_reconciliation_count: self.pending_reconciliation.len() as u64,
        }
    }

    fn push_completion(&mut self, completion: OutboxCompletion) {
        // Bounded history: totals stay exact even after old rows are dropped.
        self.completed_attempt_total = self.completed_attempt_total.saturating_add(1);
        self.completions.push_back(completion);
        while self.completions.len() > MAXIMUM_COMPLETION_HISTORY {
            self.completions.pop_front();
        }
    }
}

/// Builds a terminal record from a reservation and what was observed.
fn completion(
    entry: OutboxEntry,
    kind: SendCompletionKind,
    record: &SendCompletionRecord,
    now_unix_nanoseconds: u128,
) -> OutboxCompletion {
    OutboxCompletion {
        action_id: entry.action_id,
        draft_id: entry.draft_id,
        approval_id: entry.approval_id,
        idempotency_key: entry.idempotency_key,
        conversation_id: entry.conversation_id,
        body_sha256: entry.body_sha256,
        normalized_body_sha256: entry.normalized_body_sha256,
        capability: entry.capability,
        attachment_sha256: entry.attachment_sha256,
        display_file_name: entry.display_file_name,
        bytes_preserved_in_transit: entry.bytes_preserved_in_transit,
        rollout_stage: entry.rollout_stage,
        attempted: record.attempted,
        visual_confirmation: record.visual_confirmation,
        completion: kind,
        failure: record.failure,
        reconciled_by_replica: record.reconciled_by_replica,
        completed_at_unix_nanoseconds: now_unix_nanoseconds,
    }
}

/// What one recovery pass found and did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SendOutboxRecovery {
    pub recovered_reservation: bool,
    pub recovered_attempt: bool,
    pub idempotency_key: Option<String>,
    pub pending_reconciliation_count: u64,
}

/// An owner-only outbox directory. Every public operation runs under one
/// exclusive lock and persists atomically.
#[derive(Debug, Clone)]
pub struct SendOutbox {
    directory: PathBuf,
    account_id: String,
}

impl SendOutbox {
    /// Opens (creating on first use) an owner-only outbox directory and runs
    /// one recovery pass so an interrupted process can never leave a
    /// reservation dangling.
    pub fn open(
        directory: &Path,
        account_id: &str,
        now_unix_nanoseconds: u128,
    ) -> Result<(Self, SendOutboxRecovery), RestoreError> {
        if account_id.is_empty() {
            return Err(RestoreError::Integrity(
                "send outbox requires a non-empty account identity".to_string(),
            ));
        }
        if !directory.try_exists()? {
            fs::create_dir_all(directory)?;
            fs::set_permissions(
                directory,
                std::os::unix::fs::PermissionsExt::from_mode(0o700),
            )?;
        }
        ensure_private_directory(directory)?;
        let outbox = Self {
            directory: directory.to_path_buf(),
            account_id: account_id.to_string(),
        };
        let recovery = outbox.transaction(
            |state| Ok(state.recover(now_unix_nanoseconds)),
            now_unix_nanoseconds,
        )?;
        Ok((outbox, recovery))
    }

    /// Reads the persisted state without mutating it.
    pub fn state(&self, now_unix_nanoseconds: u128) -> Result<SendOutboxState, RestoreError> {
        self.transaction(|state| Ok(state.clone()), now_unix_nanoseconds)
    }

    /// Runs one mutation under an exclusive lock. The new state is persisted
    /// atomically if and only if the closure succeeds.
    pub fn transaction<T>(
        &self,
        operation: impl FnOnce(&mut SendOutboxState) -> Result<T, RestoreError>,
        now_unix_nanoseconds: u128,
    ) -> Result<T, RestoreError> {
        let lock = self.lock_file()?;
        let descriptor = lock.as_raw_fd();
        if unsafe { libc::flock(descriptor, libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let result = (|| -> Result<T, RestoreError> {
            let (mut state, already_materialized) = self.read_state(now_unix_nanoseconds)?;
            let before = state.clone();
            let value = operation(&mut state)?;
            if !already_materialized || state != before {
                state.validate(&self.account_id)?;
                self.persist(&state)?;
            }
            Ok(value)
        })();
        let unlock = unsafe { libc::flock(descriptor, libc::LOCK_UN) };
        let value = result?;
        if unlock != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(value)
    }

    fn lock_file(&self) -> Result<File, RestoreError> {
        let path = self.directory.join(LOCK_FILE_NAME);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.nlink() != 1
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(RestoreError::Integrity(
                "send outbox lock must be an owner-only regular file with one link".to_string(),
            ));
        }
        Ok(file)
    }

    /// Reads the persisted document, reporting whether it already existed so
    /// the first transaction always materializes an account-bound file.
    fn read_state(
        &self,
        now_unix_nanoseconds: u128,
    ) -> Result<(SendOutboxState, bool), RestoreError> {
        let path = self.directory.join(STATE_FILE_NAME);
        if !path.try_exists()? {
            return Ok((
                SendOutboxState::new(&self.account_id, now_unix_nanoseconds),
                false,
            ));
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.nlink() != 1
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.len() > MAXIMUM_OUTBOX_BYTES
        {
            return Err(RestoreError::Integrity(
                "send outbox state must be a bounded owner-only regular file".to_string(),
            ));
        }
        let state: SendOutboxState = serde_json::from_slice(&fs::read(&path)?)?;
        state.validate(&self.account_id)?;
        Ok((state, true))
    }

    fn persist(&self, state: &SendOutboxState) -> Result<(), RestoreError> {
        let temporary = self
            .directory
            .join(format!("{STATE_FILE_NAME}.tmp.{}", std::process::id()));
        if temporary.try_exists()? {
            fs::remove_file(&temporary)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&temporary)?;
        let bytes = serde_json::to_vec_pretty(state)?;
        if bytes.len() as u64 > MAXIMUM_OUTBOX_BYTES {
            let _ = fs::remove_file(&temporary);
            return Err(RestoreError::Integrity(
                "send outbox state exceeds its bounded size".to_string(),
            ));
        }
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, self.directory.join(STATE_FILE_NAME))?;
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn sha(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    fn entry(key: char, permit_send: bool) -> OutboxEntry {
        OutboxEntry {
            action_id: sha('1'),
            draft_id: sha('2'),
            approval_id: sha(key.to_ascii_uppercase()),
            idempotency_key: sha(key),
            capability_id: sha('4'),
            capability_binding_sha256: sha('5'),
            account_id: "account".to_string(),
            conversation_id: "filehelper".to_string(),
            body_sha256: sha('6'),
            normalized_body_sha256: sha('7'),
            capability: ActionCapability::TextSend,
            attachment_sha256: None,
            display_file_name: None,
            staging_directory: None,
            bytes_preserved_in_transit: true,
            rollout_stage: if permit_send {
                SendRolloutStage::SelfSend
            } else {
                SendRolloutStage::DryRun
            },
            permit_send,
            state: OutboxEntryState::Reserved,
            reserved_at_unix_nanoseconds: 1_000,
            attempted_at_unix_nanoseconds: None,
            deadline_unix_nanoseconds: 60_000,
        }
    }

    fn private_directory() -> TempDir {
        let directory = TempDir::new().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    #[test]
    fn a_reservation_is_single_flight_and_the_key_is_never_reusable() {
        let mut state = SendOutboxState::new("account", 1_000);
        state.reserve(entry('a', true), 1_000, 10_000, 2).unwrap();
        assert_eq!(
            state
                .reserve(entry('b', true), 1_000, 10_000, 2)
                .unwrap_err(),
            SendFailureCode::OutboxBusy
        );
        state.mark_attempted(&sha('a'), 1_100).unwrap();
        state
            .complete(
                &sha('a'),
                SendCompletionRecord {
                    outcome: SendCompletionOutcome::Completed(SendCompletionKind::ObservedSent),
                    attempted: true,
                    visual_confirmation: VisualConfirmation::Confirmed,
                    failure: None,
                    reconciled_by_replica: true,
                },
                1_200,
                3,
                5_000,
            )
            .unwrap();
        assert_eq!(
            state
                .reserve(entry('a', true), 1_300, 10_000, 2)
                .unwrap_err(),
            SendFailureCode::IdempotencyConflict
        );
    }

    #[test]
    fn the_attempt_window_and_circuit_breaker_both_fail_closed() {
        let mut state = SendOutboxState::new("account", 1_000);
        state.reserve(entry('a', true), 1_000, 10_000, 1).unwrap();
        state
            .complete(
                &sha('a'),
                SendCompletionRecord {
                    outcome: SendCompletionOutcome::Completed(SendCompletionKind::ObservedFailed),
                    attempted: false,
                    visual_confirmation: VisualConfirmation::NotAttempted,
                    failure: Some(SendFailureCode::RecipientVerifyFailed),
                    reconciled_by_replica: false,
                },
                1_100,
                1,
                5_000,
            )
            .unwrap();
        assert!(state.circuit_breaker.open(1_200));
        assert_eq!(
            state
                .reserve(entry('b', true), 1_200, 10_000, 5)
                .unwrap_err(),
            SendFailureCode::CircuitOpen
        );
        assert_eq!(
            state
                .reserve(entry('b', true), 7_000, 10_000, 1)
                .unwrap_err(),
            SendFailureCode::RateLimited
        );
        assert!(state.reserve(entry('b', true), 12_000, 10_000, 1).is_ok());
    }

    #[test]
    fn recovery_of_a_dispatched_attempt_parks_it_and_never_resends() {
        let mut state = SendOutboxState::new("account", 1_000);
        state.reserve(entry('a', true), 1_000, 10_000, 5).unwrap();
        state.mark_attempted(&sha('a'), 1_100).unwrap();
        let recovery = state.recover(2_000);
        assert!(recovery.recovered_attempt);
        assert!(!recovery.recovered_reservation);
        assert_eq!(recovery.pending_reconciliation_count, 1);
        assert_eq!(
            state
                .reserve(entry('b', true), 2_100, 10_000, 5)
                .unwrap_err(),
            SendFailureCode::ReconciliationPending
        );
        state
            .resolve_pending(&sha('a'), SendCompletionKind::ObservedSent, true, 2_200)
            .unwrap();
        assert!(state.pending_reconciliation.is_empty());
        assert!(state.reserve(entry('b', true), 2_300, 10_000, 5).is_ok());
    }

    #[test]
    fn recovery_of_an_undispatched_reservation_closes_it_as_failed() {
        let mut state = SendOutboxState::new("account", 1_000);
        state.reserve(entry('a', false), 1_000, 10_000, 5).unwrap();
        let recovery = state.recover(2_000);
        assert!(recovery.recovered_reservation);
        assert_eq!(recovery.pending_reconciliation_count, 0);
        let completion = state.completions.back().unwrap();
        assert_eq!(completion.completion, SendCompletionKind::ObservedFailed);
        assert!(completion.completion.lifecycle_state().is_some());
        assert!(!completion.attempted);
    }

    #[test]
    fn a_dry_run_reservation_can_never_be_marked_attempted() {
        let mut state = SendOutboxState::new("account", 1_000);
        state.reserve(entry('a', false), 1_000, 10_000, 5).unwrap();
        assert_eq!(
            state.mark_attempted(&sha('a'), 1_100).unwrap_err(),
            SendFailureCode::IdempotencyConflict
        );
    }

    #[test]
    fn state_survives_a_restart_and_recovers_exactly_once() {
        let directory = private_directory();
        let (outbox, recovery) = SendOutbox::open(directory.path(), "account", 1_000).unwrap();
        assert!(!recovery.recovered_attempt);
        outbox
            .transaction(
                |state| {
                    state.reserve(entry('a', true), 1_000, 10_000, 5).unwrap();
                    state.mark_attempted(&sha('a'), 1_050).unwrap();
                    Ok(())
                },
                1_000,
            )
            .unwrap();
        let (reopened, recovery) = SendOutbox::open(directory.path(), "account", 2_000).unwrap();
        assert!(recovery.recovered_attempt);
        let state = reopened.state(2_100).unwrap();
        assert!(state.in_flight.is_none());
        assert_eq!(state.pending_reconciliation.len(), 1);
        assert!(state.reserved_idempotency_keys.contains(&sha('a')));
        let (_, second) = SendOutbox::open(directory.path(), "account", 2_200).unwrap();
        assert!(!second.recovered_attempt);
        assert_eq!(second.pending_reconciliation_count, 1);
    }

    #[test]
    fn an_outbox_bound_to_another_account_is_refused() {
        let directory = private_directory();
        SendOutbox::open(directory.path(), "account", 1_000).unwrap();
        assert!(SendOutbox::open(directory.path(), "other-account", 1_100).is_err());
    }

    #[test]
    fn the_persisted_document_never_contains_a_message_body() {
        let directory = private_directory();
        let (outbox, _) = SendOutbox::open(directory.path(), "account", 1_000).unwrap();
        outbox
            .transaction(
                |state| {
                    state.reserve(entry('a', true), 1_000, 10_000, 5).unwrap();
                    Ok(())
                },
                1_000,
            )
            .unwrap();
        let document = fs::read_to_string(directory.path().join(STATE_FILE_NAME)).unwrap();
        assert!(document.contains("bodySha256"));
        assert!(!document.contains("\"body\""));
        let mode = fs::metadata(directory.path().join(STATE_FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }
}
