use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::archive::{ensure_private_directory, ensure_private_regular_file};
use crate::audit::{verify_recorded_artifact_files, RecordedArtifactFileVerifier};
use crate::replica::{
    get_replica_artifact, get_replica_changes, get_replica_conversation,
    get_replica_conversation_batch, get_replica_message, get_replica_participant,
    get_replica_participant_batch, replica_conversation_references_artifact_in_range,
    replica_coverage, replica_restoration_report, replica_status, search_replica_cached_moments,
    search_replica_messages, stream_replica_artifact_snapshot, ReplicaArtifactSnapshotItem,
    ReplicaCachedMomentFilter, ReplicaCachedSurfaceAvailability, ReplicaCoverageView,
    ReplicaMessageFilter, ReplicaStatus,
};
use crate::tools::{
    entity_source_database_freshness, load_tool_policy, minimize_cached_moment, minimize_message,
    released_body_bytes, released_cached_moment_body_bytes, CachedMomentsToolScope,
    ConversationToolScope, MinimizedCachedMoment, MinimizedMessage, ToolAuthorizationPolicy,
    ToolCapability, ToolDataDestination, ToolMessageField, ToolSourceDatabaseFreshness,
    MAX_SEARCH_QUERY_BYTES,
};
use crate::{
    ArtifactAvailability, ArtifactDecodeState, ArtifactKind, ArtifactRole, CanonicalArtifact,
    CanonicalConversation, CanonicalParticipant, ConversationKind, EntityDecodeState, ReplicaKey,
    RestoreError,
};

pub const CONNECTOR_API_VERSION: &str = "greenbubbles.connector.v1";
pub const CONNECTOR_VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_DRAFT_EXPIRY_SECONDS: u64 = 24 * 60 * 60;
const MAX_DRAFT_EXPIRY_SECONDS: u64 = 7 * 24 * 60 * 60;
const MAX_REQUESTER_ID_BYTES: usize = 256;
const MAX_CACHED_MOMENT_REQUESTS_PER_MINUTE: usize = 60;
const MAX_CONNECTOR_AUDIT_BYTES: u64 = 1_073_741_824;
const MAX_CONNECTOR_AUDIT_RECORD_BYTES: usize = 64 * 1_024;
const MAX_CONNECTOR_DRAFT_BYTES: u64 = 1_048_576;
const MAX_CONNECTOR_DRAFT_COUNT: usize = 100_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorRequest {
    pub api_version: String,
    pub request_id: String,
    pub requester_id: String,
    #[serde(default)]
    pub destination: ConnectorDestination,
    pub operation: ConnectorOperation,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectorDestination {
    #[default]
    Local,
    RemoteModel,
}

impl From<ConnectorDestination> for ToolDataDestination {
    fn from(value: ConnectorDestination) -> Self {
        match value {
            ConnectorDestination::Local => Self::LocalModel,
            ConnectorDestination::RemoteModel => Self::RemoteModel,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ConnectorOperation {
    Capabilities,
    Status,
    Coverage,
    GetChanges {
        cursor: Option<String>,
        limit: Option<usize>,
    },
    GetCachedMoments {
        author_id: Option<String>,
        not_before_unix: Option<i64>,
        not_after_unix: Option<i64>,
        content_type: Option<i64>,
        cursor: Option<String>,
        limit: Option<usize>,
    },
    ListConversations {
        cursor: Option<String>,
        limit: Option<usize>,
    },
    SearchMessages {
        query: String,
        conversation_id: Option<String>,
        cursor: Option<String>,
        limit: Option<usize>,
    },
    GetMessages {
        conversation_id: String,
        cursor: Option<String>,
        limit: Option<usize>,
    },
    GetMessage {
        canonical_id: String,
    },
    GetArtifact {
        conversation_id: String,
        artifact_id: String,
    },
    ResolveContact {
        participant_id: String,
    },
    ResolveConversation {
        conversation_id: String,
    },
    CreateMessageDraft {
        conversation_id: String,
        rendered_text: String,
        #[serde(default)]
        attachment_ids: Vec<String>,
        expires_in_seconds: Option<u64>,
    },
    CreateReplyDraft {
        conversation_id: String,
        reply_target_canonical_id: String,
        rendered_text: String,
        #[serde(default)]
        attachment_ids: Vec<String>,
        expires_in_seconds: Option<u64>,
    },
    CreateAttachmentDraft {
        conversation_id: String,
        attachment_ids: Vec<String>,
        rendered_text: Option<String>,
        expires_in_seconds: Option<u64>,
    },
    PreviewAction {
        draft_id: String,
    },
    Bootstrap,
    Synchronize,
    Refresh,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorResponse {
    pub api_version: String,
    pub request_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ConnectorResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ConnectorErrorBody>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum ConnectorResult {
    Capabilities(ConnectorCapabilities),
    Status(ConnectorStatus),
    Coverage(ReplicaCoverageView),
    Changes(ScopedChangePage),
    CachedMoments(ConnectorCachedMomentPage),
    DirectStatus(DirectConnectorStatus),
    Conversations(ConnectorConversationList),
    Messages(ConnectorMessagePage),
    Message(Option<MinimizedMessage>),
    Artifact(ConnectorArtifactView),
    Contact(ResolvedContact),
    Conversation(ResolvedConversation),
    Draft(DraftReceipt),
    Preview(ActionPreview),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorErrorBody {
    pub code: ConnectorErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectorErrorCode {
    InvalidRequest,
    Unauthorized,
    NotFound,
    Unavailable,
    Conflict,
    IntegrityFailure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityState {
    pub available: bool,
    pub enabled: bool,
    pub reason_code: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorCapabilities {
    pub format_version: u32,
    pub api_version: String,
    pub connector_version: String,
    pub account_id: Option<String>,
    pub self_participant_id: Option<String>,
    pub passive_read: CapabilityState,
    pub cached_moments_read: CapabilityState,
    pub authenticated_active_read: CapabilityState,
    pub draft: CapabilityState,
    pub text_send: CapabilityState,
    pub reply_send: CapabilityState,
    pub file_send: CapabilityState,
    pub operations: BTreeMap<String, CapabilityState>,
    pub enabled_conversation_count: usize,
    pub local_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorStatus {
    pub format_version: u32,
    pub api_version: String,
    pub connector_version: String,
    pub replica: ReplicaStatus,
    pub policy_created_from_source_fingerprint: String,
    pub enabled_conversation_ids: Vec<String>,
    pub locally_enabled_operation_count: usize,
    pub remotely_enabled_conversation_count: usize,
    pub cached_moments_enabled: bool,
    pub cached_moments_remote_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorConversationView {
    pub conversation_id: String,
    pub kind: ConversationKind,
    pub participant_count: usize,
    pub entity_decode_state: EntityDecodeState,
    pub source_database_freshness: ToolSourceDatabaseFreshness,
    pub human_label: String,
    pub capabilities: BTreeSet<ToolCapability>,
    pub message_fields: BTreeSet<ToolMessageField>,
    pub not_before_unix: Option<i64>,
    pub not_after_unix: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorConversationList {
    pub account_id: String,
    pub conversations: Vec<ConnectorConversationView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub omitted_conversation_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectConnectorStatus {
    pub format_version: u32,
    pub api_version: String,
    pub connector_version: String,
    pub source_mode: crate::live_query::QuerySourceMode,
    pub source_identity: String,
    pub policy_created_from_source_fingerprint: String,
    pub enabled_conversation_count: usize,
    pub locally_enabled_operation_count: usize,
    pub remotely_enabled_conversation_count: usize,
    pub ordinary_reads_use_direct_sqlite: bool,
}

pub trait ConnectorRequestHandler {
    fn handle_connector_request(&self, request: ConnectorRequest) -> ConnectorResponse;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorMessagePage {
    pub account_id: String,
    pub source_fingerprint: String,
    pub messages: Vec<MinimizedMessage>,
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub omitted_message_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectorArtifactFileOrigin {
    DownloadedSource,
    DatabaseMaterializedSource,
    DecodedDerivative,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorArtifactFile {
    pub origin: ConnectorArtifactFileOrigin,
    pub absolute_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_relative_path: Option<String>,
    pub byte_count: u64,
    pub sha256: String,
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorArtifactView {
    pub artifact_id: String,
    pub kind: ArtifactKind,
    pub role: ArtifactRole,
    pub availability: ArtifactAvailability,
    pub decode_state: ArtifactDecodeState,
    pub source: Option<ConnectorArtifactFile>,
    pub decoded: Option<ConnectorArtifactFile>,
    pub verification_detail: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
}

pub(crate) struct ConnectorArtifactExportSummary {
    pub checkpoint_revision: String,
    pub requested_count: u64,
    pub resolved_count: u64,
    pub error_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorCachedMomentPage {
    pub account_id: String,
    pub source_fingerprint: String,
    pub availability: ReplicaCachedSurfaceAvailability,
    pub cache_completeness: Option<crate::CachedSurfaceCompleteness>,
    pub observed_at: Option<String>,
    pub moments: Vec<MinimizedCachedMoment>,
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub omitted_moment_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopedChangePage {
    pub account_id: String,
    pub items: Vec<crate::replica::ReplicaChange>,
    pub next_cursor: Option<String>,
    pub scope_note: String,
    #[serde(default)]
    pub omitted_change_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedContact {
    pub participant_id: String,
    pub display_name: String,
    pub local_profile_available: bool,
    pub source_database_freshness: ToolSourceDatabaseFreshness,
    pub enabled_conversation_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RecipientParticipantEvidence {
    pub participant_id: String,
    pub display_name: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResolvedConversation {
    pub conversation_id: String,
    pub kind: ConversationKind,
    pub human_label: String,
    pub participant_count: usize,
    pub participants: Vec<RecipientParticipantEvidence>,
    pub owner_participant_id: Option<String>,
    pub entity_decode_state: EntityDecodeState,
    pub source_database_freshness: ToolSourceDatabaseFreshness,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DraftReplyTarget {
    pub canonical_id: String,
    pub canonical_record_sha256: String,
    pub sender_id: Option<String>,
    pub created_at_unix: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DraftAttachment {
    pub artifact_id: String,
    pub kind: ArtifactKind,
    pub role: ArtifactRole,
    pub digest_kind: String,
    pub sha256: String,
    pub byte_count: Option<u64>,
    pub display_file_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionDraft {
    pub format_version: u32,
    pub draft_id: String,
    pub state: DraftState,
    pub account_id: String,
    pub conversation_id: String,
    pub recipient: ResolvedConversation,
    pub reply_target: Option<DraftReplyTarget>,
    pub rendered_text: String,
    pub rendered_text_sha256: String,
    pub attachments: Vec<DraftAttachment>,
    pub connector_version: String,
    pub api_version: String,
    pub source_fingerprint: String,
    pub policy_decision_id: String,
    pub requester_id: String,
    pub created_at_unix_nanoseconds: u128,
    pub expires_at_unix_nanoseconds: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DraftState {
    DraftOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftReceipt {
    pub draft_id: String,
    pub state: DraftState,
    pub conversation_id: String,
    pub human_recipient: String,
    pub reply_target_canonical_id: Option<String>,
    pub rendered_text_sha256: String,
    pub rendered_text_byte_count: usize,
    pub attachment_count: usize,
    pub policy_decision_id: String,
    pub expires_at_unix_nanoseconds: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPreview {
    pub draft: ActionDraft,
    pub expired: bool,
    pub executable: bool,
    pub execution_unavailable_reason: String,
    pub warning: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectorAuditOutcome {
    Completed,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectorAuditStage {
    Request,
    DraftRequested,
    DraftReviewed,
    ApprovalRecorded,
    AttemptRecorded,
    ReconciliationRecorded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ConnectorAuditEvent {
    pub format_version: u32,
    pub event_id: String,
    pub observed_at_unix_nanoseconds: u128,
    pub account_id: String,
    pub requester_id: String,
    pub request_id: String,
    pub operation: String,
    pub stage: ConnectorAuditStage,
    pub conversation_id: Option<String>,
    pub destination: ConnectorDestination,
    pub outcome: ConnectorAuditOutcome,
    pub returned_item_count: usize,
    pub released_body_byte_count: usize,
    pub request_body_byte_count: usize,
    pub draft_id: Option<String>,
    pub policy_decision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_event_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub event_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorAuditReport {
    pub format_version: u32,
    pub privacy_safe_summary: bool,
    pub chain_verified: bool,
    pub fully_chained: bool,
    pub event_count: u64,
    pub legacy_unchained_event_count: u64,
    pub chained_event_count: u64,
    pub completed_event_count: u64,
    pub denied_event_count: u64,
    pub draft_requested_event_count: u64,
    pub draft_reviewed_event_count: u64,
    pub approval_event_count: u64,
    pub attempt_event_count: u64,
    pub reconciliation_event_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ConnectorStateAuditReport {
    pub format_version: u32,
    pub privacy_safe_summary: bool,
    pub audit_log: ConnectorAuditReport,
    pub draft_file_count: u64,
    pub structurally_valid_draft_count: u64,
    pub currently_previewable_draft_count: u64,
    pub stale_draft_count: u64,
    pub expired_draft_count: u64,
    pub reviewed_draft_count: u64,
    pub completed_draft_request_event_count: u64,
    pub completed_draft_review_event_count: u64,
    pub all_drafts_linked_to_request_events: bool,
    pub all_completed_review_events_linked_to_drafts: bool,
    pub gated_action_stage_event_count: u64,
}

pub struct ConnectorService<'a> {
    replica_path: PathBuf,
    key: &'a ReplicaKey,
    policy: ToolAuthorizationPolicy,
    policy_sha256: String,
    audit_path: PathBuf,
    draft_directory: PathBuf,
    checkpoint_gate: Mutex<()>,
    cache_checkpoint: Mutex<ConnectorCheckpointBinding>,
    cached_moment_request_times: Mutex<VecDeque<u128>>,
    conversation_cache: Mutex<BTreeMap<String, Option<CanonicalConversation>>>,
    participant_cache: Mutex<BTreeMap<String, Option<CanonicalParticipant>>>,
    resolved_conversation_cache: Mutex<BTreeMap<String, ResolvedConversation>>,
    preserved_stale_source_set_ids: Mutex<BTreeSet<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConnectorCheckpointBinding {
    source_fingerprint: String,
    checkpoint_revision: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ConnectorConversationCursor {
    version: u32,
    source_identity: String,
    policy_sha256: String,
    destination: ConnectorDestination,
    after_conversation_id: String,
}

impl<'a> ConnectorService<'a> {
    pub fn open(
        replica_path: &Path,
        key: &'a ReplicaKey,
        policy_path: &Path,
        audit_path: &Path,
        draft_directory: &Path,
    ) -> Result<Self, RestoreError> {
        let policy = load_tool_policy(policy_path)?;
        let policy_sha256 = hex::encode(Sha256::digest(fs::read(policy_path)?));
        let status = replica_status(replica_path, key)?;
        let cache_checkpoint = connector_checkpoint_binding(&status)?;
        let account_id = status.account_id.ok_or_else(|| {
            RestoreError::Integrity("connector replica is not initialized".to_string())
        })?;
        if account_id != policy.account_id {
            return Err(RestoreError::Integrity(
                "connector policy belongs to a different replica account".to_string(),
            ));
        }
        let preserved_stale_source_set_ids = replica_restoration_report(replica_path, key)?
            .ok_or_else(|| {
                RestoreError::Integrity("connector replica has no restoration report".to_string())
            })?
            .database_coverage
            .map(|coverage| {
                coverage
                    .preserved_stale_source_set_ids
                    .into_iter()
                    .collect()
            })
            .unwrap_or_default();
        let audit_parent = audit_path
            .parent()
            .ok_or_else(|| RestoreError::UnsafePath("audit path has no parent".to_string()))?;
        ensure_private_directory(audit_parent)?;
        if audit_path.try_exists()? {
            ensure_private_regular_file(audit_path)?;
            audit_connector_log_for_account(audit_path, Some(&account_id))?;
        }
        ensure_private_directory(draft_directory)?;
        Ok(Self {
            replica_path: replica_path.to_path_buf(),
            key,
            policy,
            policy_sha256,
            audit_path: audit_path.to_path_buf(),
            draft_directory: draft_directory.to_path_buf(),
            checkpoint_gate: Mutex::new(()),
            cache_checkpoint: Mutex::new(cache_checkpoint),
            cached_moment_request_times: Mutex::new(VecDeque::new()),
            conversation_cache: Mutex::new(BTreeMap::new()),
            participant_cache: Mutex::new(BTreeMap::new()),
            resolved_conversation_cache: Mutex::new(BTreeMap::new()),
            preserved_stale_source_set_ids: Mutex::new(preserved_stale_source_set_ids),
        })
    }

    pub fn audit_state(&self) -> Result<ConnectorStateAuditReport, RestoreError> {
        let (audit_log, events) =
            verified_connector_log_for_account(&self.audit_path, Some(&self.policy.account_id))?;
        let gated_action_stage_event_count = audit_log
            .approval_event_count
            .saturating_add(audit_log.attempt_event_count)
            .saturating_add(audit_log.reconciliation_event_count);
        if gated_action_stage_event_count != 0 {
            return Err(RestoreError::Integrity(
                "connector journal contains a gated action stage".to_string(),
            ));
        }
        let status = replica_status(&self.replica_path, self.key)?;
        let current_source_fingerprint = status.current_source_fingerprint.ok_or_else(|| {
            RestoreError::Integrity("connector replica has no current checkpoint".to_string())
        })?;
        let now = unix_nanoseconds()?;
        let mut drafts = BTreeMap::new();
        let mut draft_snapshot = BTreeMap::new();
        let mut current_count = 0_u64;
        let mut stale_count = 0_u64;
        let mut expired_count = 0_u64;
        for entry in fs::read_dir(&self.draft_directory)? {
            let entry = entry?;
            if drafts.len() >= MAX_CONNECTOR_DRAFT_COUNT {
                return Err(RestoreError::Integrity(
                    "connector draft store exceeds the verification limit".to_string(),
                ));
            }
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                return Err(RestoreError::Integrity(
                    "connector draft store contains an unsupported entry".to_string(),
                ));
            }
            let file_id = path
                .file_stem()
                .and_then(|value| value.to_str())
                .filter(|value| valid_sha256(value))
                .ok_or_else(|| {
                    RestoreError::Integrity(
                        "connector draft filename is not an immutable draft identity".to_string(),
                    )
                })?;
            let bytes = read_connector_draft_file(&path)?;
            let draft: ActionDraft = serde_json::from_slice(&bytes)?;
            validate_draft_structure(&draft)?;
            if draft.draft_id != file_id {
                return Err(RestoreError::Integrity(
                    "connector draft filename does not match its contents".to_string(),
                ));
            }
            if self.draft_current_binding_matches(&draft, &current_source_fingerprint) {
                if now < draft.expires_at_unix_nanoseconds {
                    current_count += 1;
                }
            } else {
                stale_count += 1;
            }
            if now >= draft.expires_at_unix_nanoseconds {
                expired_count += 1;
            }
            if drafts.insert(draft.draft_id.clone(), draft).is_some() {
                return Err(RestoreError::Integrity(
                    "connector draft store repeats an immutable identity".to_string(),
                ));
            }
            draft_snapshot.insert(file_id.to_string(), hex::encode(Sha256::digest(&bytes)));
        }

        let mut completed_requests = BTreeMap::<String, u64>::new();
        let mut reviewed_drafts = BTreeSet::new();
        let mut completed_review_count = 0_u64;
        for event in &events {
            validate_current_connector_stage_evidence(event)?;
            match (event.stage, event.outcome) {
                (ConnectorAuditStage::DraftRequested, ConnectorAuditOutcome::Completed) => {
                    let draft = linked_audit_draft(event, &drafts, "createDraft")?;
                    if event.requester_id != draft.requester_id {
                        return Err(RestoreError::Integrity(
                            "draft request audit requester does not match the draft".to_string(),
                        ));
                    }
                    *completed_requests
                        .entry(draft.draft_id.clone())
                        .or_default() += 1;
                }
                (ConnectorAuditStage::DraftReviewed, ConnectorAuditOutcome::Completed) => {
                    let draft = linked_audit_draft(event, &drafts, "previewAction")?;
                    reviewed_drafts.insert(draft.draft_id.clone());
                    completed_review_count += 1;
                }
                _ => {}
            }
        }
        if completed_requests.len() != drafts.len()
            || completed_requests.values().any(|count| *count != 1)
        {
            return Err(RestoreError::Integrity(
                "connector drafts and completed request events are not one-to-one".to_string(),
            ));
        }
        let (_, final_events) =
            verified_connector_log_for_account(&self.audit_path, Some(&self.policy.account_id))?;
        if audit_event_snapshot(&events)? != audit_event_snapshot(&final_events)?
            || draft_snapshot != connector_draft_store_snapshot(&self.draft_directory)?
        {
            return Err(RestoreError::Integrity(
                "connector audit or draft state changed during verification".to_string(),
            ));
        }

        Ok(ConnectorStateAuditReport {
            format_version: 1,
            privacy_safe_summary: true,
            audit_log,
            draft_file_count: drafts.len() as u64,
            structurally_valid_draft_count: drafts.len() as u64,
            currently_previewable_draft_count: current_count,
            stale_draft_count: stale_count,
            expired_draft_count: expired_count,
            reviewed_draft_count: reviewed_drafts.len() as u64,
            completed_draft_request_event_count: completed_requests.len() as u64,
            completed_draft_review_event_count: completed_review_count,
            all_drafts_linked_to_request_events: true,
            all_completed_review_events_linked_to_drafts: true,
            gated_action_stage_event_count,
        })
    }

    pub fn handle(&self, request: ConnectorRequest) -> ConnectorResponse {
        let request_id = request.request_id.clone();
        let result = self.dispatch(&request);
        connector_response(request_id, result)
    }

    /// Uses a checkpoint guard owned by the caller across one or more
    /// connector operations. The AI query/export layer calls
    /// `prepare_external_checkpoint_guard` once and verifies the same replica
    /// status before and after the complete operation or generation.
    pub(crate) fn handle_with_external_checkpoint_guard(
        &self,
        request: ConnectorRequest,
    ) -> ConnectorResponse {
        let request_id = request.request_id.clone();
        let result = self.dispatch_internal(&request, false);
        connector_response(request_id, result)
    }

    pub(crate) fn prepare_external_checkpoint_guard(&self) -> Result<(), RestoreError> {
        self.refresh_checkpoint_bound_state()
            .map(|_| ())
            .map_err(|error| RestoreError::Integrity(error.message))
    }

    fn dispatch(&self, request: &ConnectorRequest) -> Result<ConnectorResult, ConnectorErrorBody> {
        self.dispatch_internal(request, true)
    }

    fn dispatch_internal(
        &self,
        request: &ConnectorRequest,
        verify_checkpoint: bool,
    ) -> Result<ConnectorResult, ConnectorErrorBody> {
        self.validate_request(request)?;
        // The connector may outlive a separately managed replica follower.
        // Serialize checkpoint-bound reads, invalidate hydrated metadata when
        // the follower advances, and never release a response assembled
        // across two checkpoints. AI query/export own a wider equivalent
        // guard, so they skip the per-call checks while paging one generation.
        let _checkpoint_gate = if verify_checkpoint {
            Some(self.checkpoint_gate.lock().map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector checkpoint gate is unavailable".to_string(),
                ))
            })?)
        } else {
            None
        };
        let before = if verify_checkpoint {
            Some(self.refresh_checkpoint_bound_state()?)
        } else {
            None
        };
        let result = match &request.operation {
            ConnectorOperation::Capabilities => {
                let value = self.capabilities().map_err(integrity_error)?;
                self.audit_metadata(request, "capabilities")?;
                Ok(ConnectorResult::Capabilities(value))
            }
            ConnectorOperation::Status => {
                let value = self.status().map_err(integrity_error)?;
                self.audit_metadata(request, "status")?;
                Ok(ConnectorResult::Status(value))
            }
            ConnectorOperation::Coverage => {
                let value = replica_coverage(&self.replica_path, self.key)
                    .map_err(integrity_error)?;
                self.audit_metadata(request, "coverage")?;
                Ok(ConnectorResult::Coverage(value))
            }
            ConnectorOperation::GetChanges { cursor, limit } => self
                .changes(request, cursor.as_deref(), limit.unwrap_or(100))
                .map(ConnectorResult::Changes),
            ConnectorOperation::GetCachedMoments {
                author_id,
                not_before_unix,
                not_after_unix,
                content_type,
                cursor,
                limit,
            } => self
                .get_cached_moments(
                    request,
                    author_id.as_deref(),
                    *not_before_unix,
                    *not_after_unix,
                    *content_type,
                    cursor.as_deref(),
                    limit.unwrap_or(20),
                )
                .map(ConnectorResult::CachedMoments),
            ConnectorOperation::ListConversations { cursor, limit } => self
                .list_conversations(request, cursor.as_deref(), limit.unwrap_or(100))
                .map(ConnectorResult::Conversations),
            ConnectorOperation::SearchMessages {
                query,
                conversation_id,
                cursor,
                limit,
            } => self
                .search_messages(
                    request,
                    query,
                    conversation_id.as_deref(),
                    cursor.as_deref(),
                    limit.unwrap_or(20),
                )
                .map(ConnectorResult::Messages),
            ConnectorOperation::GetMessages {
                conversation_id,
                cursor,
                limit,
            } => self
                .get_messages(
                    request,
                    conversation_id,
                    cursor.as_deref(),
                    limit.unwrap_or(20),
                )
                .map(ConnectorResult::Messages),
            ConnectorOperation::GetMessage { canonical_id } => self
                .get_message(request, canonical_id)
                .map(ConnectorResult::Message),
            ConnectorOperation::GetArtifact {
                conversation_id,
                artifact_id,
            } => self
                .get_artifact(request, conversation_id, artifact_id)
                .map(ConnectorResult::Artifact),
            ConnectorOperation::ResolveContact { participant_id } => self
                .resolve_contact(request, participant_id)
                .map(ConnectorResult::Contact),
            ConnectorOperation::ResolveConversation { conversation_id } => self
                .resolve_conversation_authorized(request, conversation_id)
                .map(ConnectorResult::Conversation),
            ConnectorOperation::CreateMessageDraft {
                conversation_id,
                rendered_text,
                attachment_ids,
                expires_in_seconds,
            } => self
                .create_draft(
                    request,
                    conversation_id,
                    None,
                    rendered_text,
                    attachment_ids,
                    *expires_in_seconds,
                )
                .map(ConnectorResult::Draft),
            ConnectorOperation::CreateReplyDraft {
                conversation_id,
                reply_target_canonical_id,
                rendered_text,
                attachment_ids,
                expires_in_seconds,
            } => self
                .create_draft(
                    request,
                    conversation_id,
                    Some(reply_target_canonical_id),
                    rendered_text,
                    attachment_ids,
                    *expires_in_seconds,
                )
                .map(ConnectorResult::Draft),
            ConnectorOperation::CreateAttachmentDraft {
                conversation_id,
                attachment_ids,
                rendered_text,
                expires_in_seconds,
            } => self
                .create_draft(
                    request,
                    conversation_id,
                    None,
                    rendered_text.as_deref().unwrap_or(""),
                    attachment_ids,
                    *expires_in_seconds,
                )
                .map(ConnectorResult::Draft),
            ConnectorOperation::PreviewAction { draft_id } => self
                .preview(request, draft_id)
                .map(ConnectorResult::Preview),
            ConnectorOperation::Bootstrap
            | ConnectorOperation::Synchronize
            | ConnectorOperation::Refresh => Err(unavailable(
                "sourceAcquisitionNotInServingProcess",
                "Source acquisition is intentionally isolated from the replica serving process; use the passive CLI workflow",
            )),
        };
        if result.is_ok() && verify_checkpoint {
            let after = self.current_checkpoint_binding()?;
            if Some(&after) != before.as_ref() {
                // Clear any records observed before the concurrent commit so
                // the retry starts entirely from the new checkpoint.
                self.refresh_checkpoint_bound_state()?;
                return Err(retryable_conflict(
                    "replica checkpoint changed during the request; retry against the current checkpoint",
                ));
            }
        }
        result
    }

    fn validate_request(&self, request: &ConnectorRequest) -> Result<(), ConnectorErrorBody> {
        if request.api_version != CONNECTOR_API_VERSION {
            return Err(invalid("unsupported connector API version"));
        }
        if request.request_id.is_empty() || request.request_id.len() > 256 {
            return Err(invalid("request ID must be between 1 and 256 bytes"));
        }
        if request.requester_id.is_empty() || request.requester_id.len() > MAX_REQUESTER_ID_BYTES {
            return Err(invalid("requester ID must be between 1 and 256 bytes"));
        }
        Ok(())
    }

    fn capabilities(&self) -> Result<ConnectorCapabilities, RestoreError> {
        let status = replica_status(&self.replica_path, self.key)?;
        let initialized = status.account_id.is_some();
        let cached_source_available = if initialized {
            search_replica_cached_moments(
                &self.replica_path,
                self.key,
                &ReplicaCachedMomentFilter::default(),
                None,
                1,
            )?
            .availability
                != ReplicaCachedSurfaceAvailability::Unavailable
        } else {
            false
        };
        let cached_enabled = self.policy.cached_moments_scope.is_some();
        let draft_enabled = self
            .policy
            .conversation_scopes
            .values()
            .any(|scope| scope.capabilities.contains(&ToolCapability::CreateDraft));
        let artifact_read_enabled = initialized
            && self.policy.conversation_scopes.values().any(|scope| {
                scope
                    .capabilities
                    .contains(&ToolCapability::ReadRecentMessages)
                    && scope
                        .message_fields
                        .contains(&ToolMessageField::Attachments)
            });
        let available = |enabled: bool, code: &str, reason: &str| CapabilityState {
            available: true,
            enabled,
            reason_code: code.to_string(),
            reason: reason.to_string(),
        };
        let unavailable_state = |code: &str, reason: &str| CapabilityState {
            available: false,
            enabled: false,
            reason_code: code.to_string(),
            reason: reason.to_string(),
        };
        let read = available(
            initialized,
            if initialized {
                "enabled"
            } else {
                "replicaUninitialized"
            },
            if initialized {
                "Encrypted replica reads are available within explicit policy scopes"
            } else {
                "The encrypted replica has not been bootstrapped"
            },
        );
        let draft = available(
            draft_enabled,
            if draft_enabled {
                "enabled"
            } else {
                "notEnabledByPolicy"
            },
            if draft_enabled {
                "Immutable non-executing drafts and previews are enabled"
            } else {
                "No conversation policy enables draft creation"
            },
        );
        let active_read = unavailable_state(
            "phase05GateNotPassed",
            "Authenticated active reads have no approved adapter",
        );
        let send = unavailable_state(
            "phase05GateNotPassed",
            "Ordinary-contact actions are not implemented until the disposable-account, supportability, and legal gates pass",
        );
        let cached_read = CapabilityState {
            available: cached_source_available,
            enabled: cached_source_available && cached_enabled,
            reason_code: if !cached_source_available {
                "cachedSurfaceUnavailable"
            } else if !cached_enabled {
                "notEnabledByPolicy"
            } else {
                "enabled"
            }
            .to_string(),
            reason: if !cached_source_available {
                "No supported passive local Moments cache is present in the authoritative replica"
            } else if !cached_enabled {
                "Passive cached Moments exist but have no independent policy scope"
            } else {
                "Passive cached Moments reads are available within their independent policy scope"
            }
            .to_string(),
        };
        let artifact_read = CapabilityState {
            available: initialized,
            enabled: artifact_read_enabled,
            reason_code: if !initialized {
                "replicaUninitialized"
            } else if !artifact_read_enabled {
                "notEnabledByPolicy"
            } else {
                "enabled"
            }
            .to_string(),
            reason: if !initialized {
                "The encrypted replica has not been bootstrapped"
            } else if !artifact_read_enabled {
                "No readable conversation scope releases attachment fields"
            } else {
                "Verified artifact metadata and paths are available to local requests only"
            }
            .to_string(),
        };
        let mut operations = BTreeMap::new();
        for name in [
            "capabilities",
            "status",
            "coverage",
            "getChanges",
            "listConversations",
            "searchMessages",
            "getMessages",
            "getMessage",
            "resolveContact",
            "resolveConversation",
        ] {
            operations.insert(name.to_string(), read.clone());
        }
        operations.insert("getArtifact".to_string(), artifact_read);
        operations.insert("getCachedMoments".to_string(), cached_read.clone());
        for name in [
            "createMessageDraft",
            "createReplyDraft",
            "createAttachmentDraft",
            "previewAction",
        ] {
            operations.insert(name.to_string(), draft.clone());
        }
        for name in ["bootstrap", "synchronize", "refresh"] {
            operations.insert(
                name.to_string(),
                unavailable_state(
                    "isolatedPassiveWorkflow",
                    "Available through the isolated passive acquisition CLI, not this serving process",
                ),
            );
        }
        Ok(ConnectorCapabilities {
            format_version: 1,
            api_version: CONNECTOR_API_VERSION.to_string(),
            connector_version: CONNECTOR_VERSION.to_string(),
            account_id: status.account_id,
            self_participant_id: status.self_participant_id,
            passive_read: read,
            cached_moments_read: cached_read,
            authenticated_active_read: active_read,
            draft,
            text_send: send.clone(),
            reply_send: send.clone(),
            file_send: send,
            operations,
            enabled_conversation_count: self.policy.conversation_scopes.len(),
            local_only: true,
        })
    }

    fn status(&self) -> Result<ConnectorStatus, RestoreError> {
        let replica = replica_status(&self.replica_path, self.key)?;
        let enabled_conversation_ids = self
            .policy
            .conversation_scopes
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let locally_enabled_operation_count: usize = self
            .policy
            .conversation_scopes
            .values()
            .map(|scope| scope.capabilities.len())
            .sum();
        let remotely_enabled_conversation_count = self
            .policy
            .conversation_scopes
            .values()
            .filter(|scope| scope.allow_remote_model)
            .count();
        let cached_moments_enabled = self.policy.cached_moments_scope.is_some();
        let cached_moments_remote_enabled = self
            .policy
            .cached_moments_scope
            .as_ref()
            .is_some_and(|scope| scope.allow_remote_model);
        Ok(ConnectorStatus {
            format_version: 1,
            api_version: CONNECTOR_API_VERSION.to_string(),
            connector_version: CONNECTOR_VERSION.to_string(),
            replica,
            policy_created_from_source_fingerprint: self
                .policy
                .created_from_source_fingerprint
                .clone(),
            enabled_conversation_ids,
            locally_enabled_operation_count: locally_enabled_operation_count
                + usize::from(cached_moments_enabled),
            remotely_enabled_conversation_count,
            cached_moments_enabled,
            cached_moments_remote_enabled,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn get_cached_moments(
        &self,
        request: &ConnectorRequest,
        author_id: Option<&str>,
        requested_not_before: Option<i64>,
        requested_not_after: Option<i64>,
        content_type: Option<i64>,
        cursor: Option<&str>,
        requested_limit: usize,
    ) -> Result<ConnectorCachedMomentPage, ConnectorErrorBody> {
        if requested_not_before
            .zip(requested_not_after)
            .is_some_and(|(start, end)| start > end)
        {
            return Err(invalid("cached Moments request has an inverted time range"));
        }
        let scope = self.authorize_cached_moments(request)?;
        let not_before_unix = match (scope.not_before_unix, requested_not_before) {
            (Some(scope), Some(requested)) => Some(scope.max(requested)),
            (scope, requested) => scope.or(requested),
        };
        let not_after_unix = match (scope.not_after_unix, requested_not_after) {
            (Some(scope), Some(requested)) => Some(scope.min(requested)),
            (scope, requested) => scope.or(requested),
        };
        if not_before_unix
            .zip(not_after_unix)
            .is_some_and(|(start, end)| start > end)
        {
            return Err(invalid(
                "cached Moments request does not overlap the authorized time range",
            ));
        }
        self.enforce_cached_moment_rate(request)?;
        let filter = ReplicaCachedMomentFilter {
            author_id: author_id.map(str::to_string),
            not_before_unix,
            not_after_unix,
            content_type,
        };
        let limit = requested_limit.clamp(1, self.policy.maximum_result_count);
        let page =
            search_replica_cached_moments(&self.replica_path, self.key, &filter, cursor, limit)
                .map_err(integrity_error)?;
        let preserved_stale_source_set_ids = self.preserved_stale_sources()?;
        let mut moments = Vec::new();
        let mut omitted_moment_count = page.omitted_item_count;
        let mut limitation_codes = page.limitation_codes.into_iter().collect::<BTreeSet<_>>();
        for moment in page.items {
            match minimize_cached_moment(
                moment,
                self.policy.maximum_message_summary_bytes,
                &scope.fields,
                &preserved_stale_source_set_ids,
            ) {
                Ok(moment) => moments.push(moment),
                Err(_) => {
                    omitted_moment_count = omitted_moment_count.saturating_add(1);
                    limitation_codes.insert("malformedCachedMomentOmitted".to_string());
                }
            }
        }
        let released = released_cached_moment_body_bytes(&moments);
        self.audit(
            request,
            "getCachedMoments",
            None,
            ConnectorAuditOutcome::Completed,
            moments.len(),
            released,
            0,
            None,
            None,
        )?;
        Ok(ConnectorCachedMomentPage {
            account_id: page.account_id,
            source_fingerprint: page.source_fingerprint,
            availability: page.availability,
            cache_completeness: page.cache_completeness,
            observed_at: page.observed_at,
            moments,
            next_cursor: page.next_cursor,
            omitted_moment_count,
            limitation_codes: limitation_codes.into_iter().collect(),
        })
    }

    fn list_conversations(
        &self,
        request: &ConnectorRequest,
        cursor: Option<&str>,
        requested_limit: usize,
    ) -> Result<ConnectorConversationList, ConnectorErrorBody> {
        let destination = ToolDataDestination::from(request.destination);
        let status = replica_status(&self.replica_path, self.key).map_err(integrity_error)?;
        let account_id = status
            .account_id
            .ok_or_else(|| unavailable("replicaUninitialized", "Replica is not initialized"))?;
        let source_identity = status.current_source_fingerprint.ok_or_else(|| {
            unavailable(
                "replicaUninitialized",
                "Replica has no authoritative checkpoint",
            )
        })?;
        let mut conversations = Vec::new();
        let mut limitation_codes = BTreeSet::new();
        let enabled_conversation_ids = self
            .policy
            .conversation_scopes
            .iter()
            .filter(|(_, scope)| {
                scope
                    .capabilities
                    .contains(&ToolCapability::ListConversations)
                    && (destination != ToolDataDestination::RemoteModel || scope.allow_remote_model)
            })
            .map(|(conversation_id, _)| conversation_id.clone())
            .collect::<BTreeSet<_>>();
        let after = decode_connector_conversation_cursor(
            cursor,
            &source_identity,
            &self.policy_sha256,
            request.destination,
        )?;
        let limit = requested_limit.clamp(1, self.policy.maximum_result_count);
        let selected_ids = enabled_conversation_ids
            .iter()
            .filter(|identifier| {
                after
                    .as_deref()
                    .is_none_or(|after| identifier.as_str() > after)
            })
            .take(limit.saturating_add(1))
            .cloned()
            .collect::<Vec<_>>();
        let has_more = selected_ids.len() > limit;
        let selected_ids = selected_ids
            .into_iter()
            .take(limit)
            .collect::<BTreeSet<_>>();
        self.warm_conversation_caches(&selected_ids)?;
        for (conversation_id, scope) in &self.policy.conversation_scopes {
            if !scope
                .capabilities
                .contains(&ToolCapability::ListConversations)
                || (destination == ToolDataDestination::RemoteModel && !scope.allow_remote_model)
                || !selected_ids.contains(conversation_id)
            {
                continue;
            }
            let resolved = self.resolve_conversation_by_id(conversation_id)?;
            limitation_codes.extend(resolved.limitation_codes.iter().cloned());
            conversations.push(ConnectorConversationView {
                conversation_id: resolved.conversation_id,
                kind: resolved.kind,
                participant_count: resolved.participant_count,
                entity_decode_state: resolved.entity_decode_state,
                source_database_freshness: resolved.source_database_freshness,
                human_label: resolved.human_label,
                capabilities: scope.capabilities.clone(),
                message_fields: scope.message_fields.clone(),
                not_before_unix: scope.not_before_unix,
                not_after_unix: scope.not_after_unix,
            });
        }
        conversations.sort_by(|left, right| left.conversation_id.cmp(&right.conversation_id));
        self.audit(
            request,
            "listConversations",
            None,
            ConnectorAuditOutcome::Completed,
            conversations.len(),
            0,
            0,
            None,
            None,
        )?;
        Ok(ConnectorConversationList {
            account_id,
            conversations,
            next_cursor: if has_more {
                selected_ids
                    .last()
                    .map(|after| {
                        encode_connector_conversation_cursor(
                            &source_identity,
                            &self.policy_sha256,
                            request.destination,
                            after,
                        )
                    })
                    .transpose()?
            } else {
                None
            },
            omitted_conversation_count: 0,
            limitation_codes: limitation_codes.into_iter().collect(),
        })
    }

    fn get_messages(
        &self,
        request: &ConnectorRequest,
        conversation_id: &str,
        cursor: Option<&str>,
        requested_limit: usize,
    ) -> Result<ConnectorMessagePage, ConnectorErrorBody> {
        let scope = self.authorize(
            request,
            conversation_id,
            ToolCapability::ReadRecentMessages,
            "getMessages",
        )?;
        let limit = requested_limit.clamp(1, self.policy.maximum_result_count);
        let filter = ReplicaMessageFilter {
            conversation_id: Some(conversation_id.to_string()),
            not_before_unix: scope.not_before_unix,
            not_after_unix: scope.not_after_unix,
            ..Default::default()
        };
        let page = search_replica_messages(&self.replica_path, self.key, &filter, cursor, limit)
            .map_err(integrity_error)?;
        let preserved_stale_source_set_ids = self.preserved_stale_sources()?;
        let messages = page
            .items
            .into_iter()
            .map(|message| {
                minimize_message(
                    message,
                    self.policy.maximum_message_summary_bytes,
                    &scope.message_fields,
                    &preserved_stale_source_set_ids,
                )
            })
            .collect::<Vec<_>>();
        let mut limitation_codes = page.limitation_codes.into_iter().collect::<BTreeSet<_>>();
        extend_message_projection_limitations(&messages, &mut limitation_codes);
        let released = released_body_bytes(&messages);
        self.audit(
            request,
            "getMessages",
            Some(conversation_id),
            ConnectorAuditOutcome::Completed,
            messages.len(),
            released,
            0,
            None,
            None,
        )?;
        Ok(ConnectorMessagePage {
            account_id: page.account_id,
            source_fingerprint: page.source_fingerprint,
            messages,
            next_cursor: page.next_cursor,
            omitted_message_count: page.omitted_item_count,
            limitation_codes: limitation_codes.into_iter().collect(),
        })
    }

    fn search_messages(
        &self,
        request: &ConnectorRequest,
        query: &str,
        conversation_id: Option<&str>,
        cursor: Option<&str>,
        requested_limit: usize,
    ) -> Result<ConnectorMessagePage, ConnectorErrorBody> {
        if query.is_empty() || query.len() > MAX_SEARCH_QUERY_BYTES {
            return Err(invalid(
                "search query length is outside the supported range",
            ));
        }
        if conversation_id.is_none() && cursor.is_some() {
            return Err(invalid(
                "cross-conversation search cursors are unavailable; scope the search to one conversation",
            ));
        }
        let limit = requested_limit.clamp(1, self.policy.maximum_result_count);
        let preserved_stale_source_set_ids = self.preserved_stale_sources()?;
        if let Some(conversation_id) = conversation_id {
            let scope = self.authorize(
                request,
                conversation_id,
                ToolCapability::SearchMessages,
                "searchMessages",
            )?;
            let filter = ReplicaMessageFilter {
                conversation_id: Some(conversation_id.to_string()),
                not_before_unix: scope.not_before_unix,
                not_after_unix: scope.not_after_unix,
                full_text_query: Some(query.to_string()),
                ..Default::default()
            };
            let page =
                search_replica_messages(&self.replica_path, self.key, &filter, cursor, limit)
                    .map_err(integrity_error)?;
            let messages = page
                .items
                .into_iter()
                .map(|message| {
                    minimize_message(
                        message,
                        self.policy.maximum_message_summary_bytes,
                        &scope.message_fields,
                        &preserved_stale_source_set_ids,
                    )
                })
                .collect::<Vec<_>>();
            let mut limitation_codes = page.limitation_codes.into_iter().collect::<BTreeSet<_>>();
            extend_message_projection_limitations(&messages, &mut limitation_codes);
            let released = released_body_bytes(&messages);
            self.audit(
                request,
                "searchMessages",
                Some(conversation_id),
                ConnectorAuditOutcome::Completed,
                messages.len(),
                released,
                query.len(),
                None,
                None,
            )?;
            return Ok(ConnectorMessagePage {
                account_id: page.account_id,
                source_fingerprint: page.source_fingerprint,
                messages,
                next_cursor: page.next_cursor,
                omitted_message_count: page.omitted_item_count,
                limitation_codes: limitation_codes.into_iter().collect(),
            });
        }

        let status = replica_status(&self.replica_path, self.key).map_err(integrity_error)?;
        let account_id = status
            .account_id
            .ok_or_else(|| unavailable("replicaUninitialized", "Replica is not initialized"))?;
        let source_fingerprint = status.current_source_fingerprint.ok_or_else(|| {
            unavailable(
                "replicaUninitialized",
                "Replica has no authoritative checkpoint",
            )
        })?;
        let destination = ToolDataDestination::from(request.destination);
        let mut messages = Vec::new();
        let mut omitted_message_count = 0_u64;
        let mut limitation_codes = BTreeSet::new();
        for (identifier, scope) in &self.policy.conversation_scopes {
            if !scope.capabilities.contains(&ToolCapability::SearchMessages)
                || (destination == ToolDataDestination::RemoteModel && !scope.allow_remote_model)
            {
                continue;
            }
            let filter = ReplicaMessageFilter {
                conversation_id: Some(identifier.clone()),
                not_before_unix: scope.not_before_unix,
                not_after_unix: scope.not_after_unix,
                full_text_query: Some(query.to_string()),
                ..Default::default()
            };
            let page = search_replica_messages(&self.replica_path, self.key, &filter, None, limit)
                .map_err(integrity_error)?;
            omitted_message_count = omitted_message_count.saturating_add(page.omitted_item_count);
            limitation_codes.extend(page.limitation_codes);
            messages.extend(page.items.into_iter().map(|message| {
                minimize_message(
                    message,
                    self.policy.maximum_message_summary_bytes,
                    &scope.message_fields,
                    &preserved_stale_source_set_ids,
                )
            }));
        }
        messages.sort_by(|left, right| {
            (
                left.created_at_unix,
                &left.conversation_id,
                left.conversation_ordinal,
                &left.canonical_id,
            )
                .cmp(&(
                    right.created_at_unix,
                    &right.conversation_id,
                    right.conversation_ordinal,
                    &right.canonical_id,
                ))
        });
        messages.truncate(limit);
        extend_message_projection_limitations(&messages, &mut limitation_codes);
        let released = released_body_bytes(&messages);
        self.audit(
            request,
            "searchMessages",
            None,
            ConnectorAuditOutcome::Completed,
            messages.len(),
            released,
            query.len(),
            None,
            None,
        )?;
        Ok(ConnectorMessagePage {
            account_id,
            source_fingerprint,
            messages,
            next_cursor: None,
            omitted_message_count,
            limitation_codes: limitation_codes.into_iter().collect(),
        })
    }

    fn get_message(
        &self,
        request: &ConnectorRequest,
        canonical_id: &str,
    ) -> Result<Option<MinimizedMessage>, ConnectorErrorBody> {
        let message = get_replica_message(&self.replica_path, self.key, canonical_id)
            .map_err(integrity_error)?;
        let Some(message) = message else {
            return Ok(None);
        };
        let scope = self.authorize(
            request,
            &message.conversation_id,
            ToolCapability::ReadRecentMessages,
            "getMessage",
        )?;
        if !scope.includes_message(&message) {
            return Err(unauthorized("message is outside the authorized time range"));
        }
        let preserved_stale_source_set_ids = self.preserved_stale_sources()?;
        let result = minimize_message(
            message,
            self.policy.maximum_message_summary_bytes,
            &scope.message_fields,
            &preserved_stale_source_set_ids,
        );
        let released = result
            .payload_summary
            .as_ref()
            .map(String::len)
            .unwrap_or_default();
        self.audit(
            request,
            "getMessage",
            Some(&result.conversation_id),
            ConnectorAuditOutcome::Completed,
            1,
            released,
            0,
            None,
            None,
        )?;
        Ok(Some(result))
    }

    fn get_artifact(
        &self,
        request: &ConnectorRequest,
        conversation_id: &str,
        artifact_id: &str,
    ) -> Result<ConnectorArtifactView, ConnectorErrorBody> {
        if request.destination != ConnectorDestination::Local {
            let _ = self.audit(
                request,
                "getArtifact",
                Some(conversation_id),
                ConnectorAuditOutcome::Denied,
                0,
                0,
                0,
                None,
                None,
            );
            return Err(unauthorized(
                "artifact paths are restricted to the local destination",
            ));
        }
        let scope = self.authorize(
            request,
            conversation_id,
            ToolCapability::ReadRecentMessages,
            "getArtifact",
        )?;
        if !scope
            .message_fields
            .contains(&ToolMessageField::Attachments)
        {
            let _ = self.audit(
                request,
                "getArtifact",
                Some(conversation_id),
                ConnectorAuditOutcome::Denied,
                0,
                0,
                0,
                None,
                None,
            );
            return Err(unauthorized(
                "artifact fields are not enabled for this conversation",
            ));
        }
        let referenced = replica_conversation_references_artifact_in_range(
            &self.replica_path,
            self.key,
            conversation_id,
            artifact_id,
            scope.not_before_unix,
            scope.not_after_unix,
        )
        .map_err(integrity_error)?;
        if !referenced {
            return Err(unauthorized(
                "artifact is not referenced within the authorized conversation time range",
            ));
        }
        let Some(artifact) = get_replica_artifact(&self.replica_path, self.key, artifact_id)
            .map_err(integrity_error)?
        else {
            let result = unavailable_artifact(artifact_id);
            self.audit(
                request,
                "getArtifact",
                Some(conversation_id),
                ConnectorAuditOutcome::Completed,
                1,
                0,
                0,
                None,
                None,
            )?;
            return Ok(result);
        };
        let report = replica_restoration_report(&self.replica_path, self.key)
            .map_err(integrity_error)?
            .ok_or_else(|| {
                unavailable(
                    "replicaUninitialized",
                    "Replica has no authoritative restoration report",
                )
            })?;
        let archive_root = Path::new(&report.artifacts_path).parent().ok_or_else(|| {
            integrity_error(RestoreError::UnsafePath(report.artifacts_path.clone()))
        })?;
        verify_recorded_artifact_files(archive_root, &artifact).map_err(integrity_error)?;
        let result = connector_artifact_view(artifact).map_err(integrity_error)?;
        let released = serde_json::to_vec(&result)
            .map_err(|error| integrity_error(error.into()))?
            .len();
        self.audit(
            request,
            "getArtifact",
            Some(conversation_id),
            ConnectorAuditOutcome::Completed,
            1,
            released,
            0,
            None,
            None,
        )?;
        Ok(result)
    }

    pub(crate) fn export_authorized_artifacts(
        &self,
        request: &ConnectorRequest,
        artifact_conversations: &BTreeMap<String, BTreeSet<String>>,
        mut visit: impl FnMut(
            &str,
            &BTreeSet<String>,
            Result<ConnectorArtifactView, ConnectorErrorBody>,
        ) -> Result<(), RestoreError>,
    ) -> Result<ConnectorArtifactExportSummary, RestoreError> {
        self.validate_request(request)
            .map_err(connector_error_as_restore_error)?;
        for conversation_ids in artifact_conversations.values() {
            if conversation_ids.is_empty()
                || conversation_ids.iter().any(|conversation_id| {
                    !self
                        .policy
                        .conversation_scopes
                        .get(conversation_id)
                        .is_some_and(|scope| {
                            scope
                                .capabilities
                                .contains(&ToolCapability::ReadRecentMessages)
                                && scope
                                    .message_fields
                                    .contains(&ToolMessageField::Attachments)
                                && (request.destination == ConnectorDestination::Local
                                    || scope.allow_remote_model)
                        })
                })
            {
                return Err(RestoreError::Integrity(
                    "AI artifact batch is not derived from authorized message references"
                        .to_string(),
                ));
            }
        }

        if request.destination != ConnectorDestination::Local {
            for (artifact_id, conversation_ids) in artifact_conversations {
                visit(
                    artifact_id,
                    conversation_ids,
                    Err(unauthorized(
                        "artifact paths are restricted to the local destination",
                    )),
                )?;
            }
            self.audit(
                request,
                "exportArtifacts",
                None,
                ConnectorAuditOutcome::Denied,
                0,
                0,
                0,
                None,
                None,
            )
            .map_err(connector_error_as_restore_error)?;
            let checkpoint_revision = replica_status(&self.replica_path, self.key)?
                .checkpoint_revision
                .ok_or_else(|| {
                    RestoreError::Integrity(
                        "connector replica has no current checkpoint revision".to_string(),
                    )
                })?;
            return Ok(ConnectorArtifactExportSummary {
                checkpoint_revision,
                requested_count: artifact_conversations.len() as u64,
                resolved_count: 0,
                error_count: artifact_conversations.len() as u64,
            });
        }

        let artifact_ids = artifact_conversations
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut verifier: Option<RecordedArtifactFileVerifier> = None;
        let mut verifier_initialization_failed = false;
        let mut resolved_count = 0_u64;
        let mut error_count = 0_u64;
        let mut released_body_byte_count = 0_usize;
        let snapshot = stream_replica_artifact_snapshot(
            &self.replica_path,
            self.key,
            &artifact_ids,
            |artifact_id, item, report| {
                let result = match item {
                    ReplicaArtifactSnapshotItem::Available(artifact) => {
                        if verifier.is_none() && !verifier_initialization_failed {
                            let opened = Path::new(&report.artifacts_path)
                                .parent()
                                .ok_or_else(|| {
                                    RestoreError::UnsafePath(report.artifacts_path.clone())
                                })
                                .and_then(RecordedArtifactFileVerifier::open);
                            match opened {
                                Ok(opened) => verifier = Some(opened),
                                Err(_) => verifier_initialization_failed = true,
                            }
                        }
                        if let Some(verifier) = verifier.as_mut() {
                            verifier
                                .verify(&artifact)
                                .and_then(|()| connector_artifact_view(*artifact))
                                .map_err(integrity_error)
                        } else {
                            Err(integrity_error(RestoreError::Integrity(
                                "artifact archive root is unavailable for verification".to_string(),
                            )))
                        }
                    }
                    ReplicaArtifactSnapshotItem::Missing => {
                        Err(not_found("artifact was not found"))
                    }
                    ReplicaArtifactSnapshotItem::Invalid => {
                        Err(integrity_error(RestoreError::Integrity(
                            "artifact record failed canonical verification".to_string(),
                        )))
                    }
                };
                match &result {
                    Ok(value) => {
                        resolved_count = resolved_count.saturating_add(1);
                        released_body_byte_count = released_body_byte_count.saturating_add(
                            serde_json::to_vec(value).map_err(RestoreError::from)?.len(),
                        );
                    }
                    Err(_) => error_count = error_count.saturating_add(1),
                }
                let conversation_ids =
                    artifact_conversations.get(artifact_id).ok_or_else(|| {
                        RestoreError::Integrity(
                            "artifact snapshot returned an unrequested identity".to_string(),
                        )
                    })?;
                visit(artifact_id, conversation_ids, result)
            },
        )?;
        if snapshot.requested_count != artifact_conversations.len() as u64
            || snapshot
                .available_count
                .saturating_add(snapshot.missing_count)
                .saturating_add(snapshot.invalid_count)
                != snapshot.requested_count
            || resolved_count.saturating_add(error_count) != snapshot.requested_count
        {
            return Err(RestoreError::Integrity(
                "artifact batch accounting is inconsistent".to_string(),
            ));
        }
        self.audit(
            request,
            "exportArtifacts",
            None,
            ConnectorAuditOutcome::Completed,
            usize::try_from(resolved_count).unwrap_or(usize::MAX),
            released_body_byte_count,
            0,
            None,
            None,
        )
        .map_err(connector_error_as_restore_error)?;
        Ok(ConnectorArtifactExportSummary {
            checkpoint_revision: snapshot.checkpoint_revision,
            requested_count: snapshot.requested_count,
            resolved_count,
            error_count,
        })
    }

    fn changes(
        &self,
        request: &ConnectorRequest,
        cursor: Option<&str>,
        requested_limit: usize,
    ) -> Result<ScopedChangePage, ConnectorErrorBody> {
        let destination = ToolDataDestination::from(request.destination);
        let allowed = self
            .policy
            .conversation_scopes
            .iter()
            .filter(|(_, scope)| {
                (scope
                    .capabilities
                    .contains(&ToolCapability::ReadRecentMessages)
                    || scope.capabilities.contains(&ToolCapability::SearchMessages))
                    && (destination == ToolDataDestination::LocalModel || scope.allow_remote_model)
            })
            .map(|(identifier, _)| identifier.as_str())
            .collect::<BTreeSet<_>>();
        if allowed.is_empty() {
            return Err(unauthorized("no conversation permits reading changes"));
        }
        let limit = requested_limit.clamp(1, self.policy.maximum_result_count);
        let raw = get_replica_changes(&self.replica_path, self.key, cursor, limit)
            .map_err(integrity_error)?;
        let items = raw
            .items
            .into_iter()
            .filter(|change| {
                change
                    .conversation_id
                    .as_deref()
                    .is_some_and(|identifier| allowed.contains(identifier))
            })
            .collect::<Vec<_>>();
        self.audit(
            request,
            "getChanges",
            None,
            ConnectorAuditOutcome::Completed,
            items.len(),
            0,
            0,
            None,
            None,
        )?;
        Ok(ScopedChangePage {
            account_id: raw.account_id,
            items,
            next_cursor: raw.next_cursor,
            scope_note: "Only change records with an explicitly authorized conversation identity are released; participant, artifact, and checkpoint events are omitted"
                .to_string(),
            omitted_change_count: raw.omitted_item_count,
            limitation_codes: raw.limitation_codes,
        })
    }

    fn resolve_contact(
        &self,
        request: &ConnectorRequest,
        participant_id: &str,
    ) -> Result<ResolvedContact, ConnectorErrorBody> {
        let destination = ToolDataDestination::from(request.destination);
        let participant = self.cached_participant(participant_id)?;
        let mut enabled = participant
            .as_ref()
            .into_iter()
            .flat_map(|participant| participant.conversation_ids.iter())
            .filter(|identifier| {
                self.policy
                    .conversation_scopes
                    .get(*identifier)
                    .is_some_and(|scope| {
                        (scope
                            .capabilities
                            .contains(&ToolCapability::ListConversations)
                            || scope.capabilities.contains(&ToolCapability::CreateDraft))
                            && (destination == ToolDataDestination::LocalModel
                                || scope.allow_remote_model)
                    })
            })
            .cloned()
            .collect::<BTreeSet<_>>();

        // A healthy participant carries its account-bound memberships, which
        // are tiny compared with an all-conversation policy. Only a missing
        // profile needs the broader conversation scan to prove that the
        // requested opaque identity is still authorized before synthesizing
        // a placeholder.
        if participant.is_none() {
            let allowed_conversation_ids = self
                .policy
                .conversation_scopes
                .iter()
                .filter(|(_, scope)| {
                    (scope
                        .capabilities
                        .contains(&ToolCapability::ListConversations)
                        || scope.capabilities.contains(&ToolCapability::CreateDraft))
                        && (destination == ToolDataDestination::LocalModel
                            || scope.allow_remote_model)
                })
                .map(|(identifier, _)| identifier.clone())
                .collect::<BTreeSet<_>>();
            self.warm_conversation_caches(&allowed_conversation_ids)?;
            let conversations = self.conversation_cache.lock().map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector conversation cache is unavailable".to_string(),
                ))
            })?;
            enabled.extend(
                allowed_conversation_ids
                    .iter()
                    .filter(|identifier| {
                        conversations
                            .get(*identifier)
                            .and_then(Option::as_ref)
                            .is_some_and(|conversation| {
                                conversation
                                    .participant_ids
                                    .iter()
                                    .any(|candidate| candidate == participant_id)
                            })
                    })
                    .cloned(),
            );
        }
        if enabled.is_empty() {
            return Err(unauthorized(
                "participant has no enabled conversation for this destination",
            ));
        }
        let preserved_stale_source_set_ids = self.preserved_stale_sources()?;
        let result = if let Some(participant) = participant {
            let display_name = participant_display_name(&participant);
            ResolvedContact {
                participant_id: participant.participant_id,
                display_name,
                local_profile_available: participant.local_profile_state
                    == crate::LocalProfileState::Hydrated,
                source_database_freshness: entity_source_database_freshness(
                    participant
                        .source_records
                        .iter()
                        .map(|record| record.source_set_id.clone()),
                    &preserved_stale_source_set_ids,
                ),
                enabled_conversation_ids: enabled.into_iter().collect(),
                limitation_codes: Vec::new(),
            }
        } else {
            ResolvedContact {
                participant_id: participant_id.to_string(),
                display_name: short_identifier(participant_id),
                local_profile_available: false,
                source_database_freshness: ToolSourceDatabaseFreshness::Derived,
                enabled_conversation_ids: enabled.into_iter().collect(),
                limitation_codes: vec!["unavailableParticipantProfileSynthesized".to_string()],
            }
        };
        self.audit(
            request,
            "resolveContact",
            None,
            ConnectorAuditOutcome::Completed,
            1,
            0,
            0,
            None,
            None,
        )?;
        Ok(result)
    }

    fn resolve_conversation_authorized(
        &self,
        request: &ConnectorRequest,
        conversation_id: &str,
    ) -> Result<ResolvedConversation, ConnectorErrorBody> {
        let destination = ToolDataDestination::from(request.destination);
        let allowed = self
            .policy
            .conversation_scopes
            .get(conversation_id)
            .is_some_and(|scope| {
                (scope
                    .capabilities
                    .contains(&ToolCapability::ListConversations)
                    || scope.capabilities.contains(&ToolCapability::CreateDraft))
                    && (destination == ToolDataDestination::LocalModel || scope.allow_remote_model)
            });
        if !allowed {
            return Err(unauthorized(
                "conversation resolution is outside the authorized scope",
            ));
        }
        let result = self.resolve_conversation_by_id(conversation_id)?;
        self.audit(
            request,
            "resolveConversation",
            Some(conversation_id),
            ConnectorAuditOutcome::Completed,
            1,
            0,
            0,
            None,
            None,
        )?;
        Ok(result)
    }

    fn current_checkpoint_binding(&self) -> Result<ConnectorCheckpointBinding, ConnectorErrorBody> {
        let status = replica_status(&self.replica_path, self.key).map_err(integrity_error)?;
        if status.account_id.as_deref() != Some(self.policy.account_id.as_str()) {
            return Err(integrity_error(RestoreError::Integrity(
                "connector replica account changed after opening".to_string(),
            )));
        }
        connector_checkpoint_binding(&status).map_err(integrity_error)
    }

    fn refresh_checkpoint_bound_state(
        &self,
    ) -> Result<ConnectorCheckpointBinding, ConnectorErrorBody> {
        let current = self.current_checkpoint_binding()?;
        let mut cached = self.cache_checkpoint.lock().map_err(|_| {
            integrity_error(RestoreError::Integrity(
                "connector checkpoint cache is unavailable".to_string(),
            ))
        })?;
        if *cached == current {
            return Ok(current);
        }

        let preserved_stale_source_set_ids =
            replica_restoration_report(&self.replica_path, self.key)
                .map_err(integrity_error)?
                .ok_or_else(|| {
                    integrity_error(RestoreError::Integrity(
                        "connector replica has no restoration report".to_string(),
                    ))
                })?
                .database_coverage
                .map(|coverage| {
                    coverage
                        .preserved_stale_source_set_ids
                        .into_iter()
                        .collect()
                })
                .unwrap_or_default();
        self.conversation_cache
            .lock()
            .map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector conversation cache is unavailable".to_string(),
                ))
            })?
            .clear();
        self.participant_cache
            .lock()
            .map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector participant cache is unavailable".to_string(),
                ))
            })?
            .clear();
        self.resolved_conversation_cache
            .lock()
            .map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector resolved-conversation cache is unavailable".to_string(),
                ))
            })?
            .clear();
        *self.preserved_stale_source_set_ids.lock().map_err(|_| {
            integrity_error(RestoreError::Integrity(
                "connector coverage cache is unavailable".to_string(),
            ))
        })? = preserved_stale_source_set_ids;
        *cached = current.clone();
        Ok(current)
    }

    fn preserved_stale_sources(&self) -> Result<BTreeSet<String>, ConnectorErrorBody> {
        self.preserved_stale_source_set_ids
            .lock()
            .map(|sources| sources.clone())
            .map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector coverage cache is unavailable".to_string(),
                ))
            })
    }

    fn warm_conversation_caches(
        &self,
        conversation_ids: &BTreeSet<String>,
    ) -> Result<(), ConnectorErrorBody> {
        let missing_conversation_ids = {
            let cache = self.conversation_cache.lock().map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector conversation cache is unavailable".to_string(),
                ))
            })?;
            conversation_ids
                .iter()
                .filter(|identifier| !cache.contains_key(*identifier))
                .cloned()
                .collect::<BTreeSet<_>>()
        };
        if !missing_conversation_ids.is_empty() {
            let mut loaded = get_replica_conversation_batch(
                &self.replica_path,
                self.key,
                &missing_conversation_ids,
            )
            .map_err(integrity_error)?;
            let mut cache = self.conversation_cache.lock().map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector conversation cache is unavailable".to_string(),
                ))
            })?;
            for identifier in missing_conversation_ids {
                cache.insert(identifier.clone(), loaded.remove(&identifier));
            }
        }

        let participant_ids = {
            let cache = self.conversation_cache.lock().map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector conversation cache is unavailable".to_string(),
                ))
            })?;
            conversation_ids
                .iter()
                .filter_map(|identifier| cache.get(identifier).and_then(Option::as_ref))
                .flat_map(|conversation| conversation.participant_ids.iter().cloned())
                .collect::<BTreeSet<_>>()
        };
        let missing_participant_ids = {
            let cache = self.participant_cache.lock().map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector participant cache is unavailable".to_string(),
                ))
            })?;
            participant_ids
                .iter()
                .filter(|identifier| !cache.contains_key(*identifier))
                .cloned()
                .collect::<BTreeSet<_>>()
        };
        if !missing_participant_ids.is_empty() {
            let mut loaded = get_replica_participant_batch(
                &self.replica_path,
                self.key,
                &missing_participant_ids,
            )
            .map_err(integrity_error)?;
            let mut cache = self.participant_cache.lock().map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector participant cache is unavailable".to_string(),
                ))
            })?;
            for identifier in missing_participant_ids {
                cache.insert(identifier.clone(), loaded.remove(&identifier));
            }
        }
        Ok(())
    }

    fn cached_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<CanonicalConversation>, ConnectorErrorBody> {
        if let Some(cached) = self
            .conversation_cache
            .lock()
            .map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector conversation cache is unavailable".to_string(),
                ))
            })?
            .get(conversation_id)
            .cloned()
        {
            return Ok(cached);
        }
        let loaded = get_replica_conversation(&self.replica_path, self.key, conversation_id)
            .map_err(integrity_error)?;
        self.conversation_cache
            .lock()
            .map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector conversation cache is unavailable".to_string(),
                ))
            })?
            .insert(conversation_id.to_string(), loaded.clone());
        Ok(loaded)
    }

    fn cached_participant(
        &self,
        participant_id: &str,
    ) -> Result<Option<CanonicalParticipant>, ConnectorErrorBody> {
        if let Some(cached) = self
            .participant_cache
            .lock()
            .map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector participant cache is unavailable".to_string(),
                ))
            })?
            .get(participant_id)
            .cloned()
        {
            return Ok(cached);
        }
        let loaded = get_replica_participant(&self.replica_path, self.key, participant_id)
            .map_err(integrity_error)?;
        self.participant_cache
            .lock()
            .map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector participant cache is unavailable".to_string(),
                ))
            })?
            .insert(participant_id.to_string(), loaded.clone());
        Ok(loaded)
    }

    fn resolve_conversation_by_id(
        &self,
        conversation_id: &str,
    ) -> Result<ResolvedConversation, ConnectorErrorBody> {
        if let Some(cached) = self
            .resolved_conversation_cache
            .lock()
            .map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector resolved-conversation cache is unavailable".to_string(),
                ))
            })?
            .get(conversation_id)
            .cloned()
        {
            return Ok(cached);
        }
        let resolved = match self.cached_conversation(conversation_id)? {
            Some(conversation) => self.resolve_conversation(&conversation)?,
            None => derived_conversation(conversation_id),
        };
        self.resolved_conversation_cache
            .lock()
            .map_err(|_| {
                integrity_error(RestoreError::Integrity(
                    "connector resolved-conversation cache is unavailable".to_string(),
                ))
            })?
            .insert(conversation_id.to_string(), resolved.clone());
        Ok(resolved)
    }

    fn resolve_conversation(
        &self,
        conversation: &CanonicalConversation,
    ) -> Result<ResolvedConversation, ConnectorErrorBody> {
        let membership_roles = conversation
            .memberships
            .iter()
            .map(|membership| {
                (
                    membership.participant_id.as_str(),
                    (
                        format!("{:?}", membership.role),
                        membership.display_name_base64.as_deref(),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut participants = Vec::new();
        let mut limitation_codes = BTreeSet::new();
        for participant_id in &conversation.participant_ids {
            let participant = self.cached_participant(participant_id)?;
            if participant.is_none() {
                limitation_codes.insert("unavailableParticipantProfile".to_string());
            }
            let (role, scoped_name) = membership_roles
                .get(participant_id.as_str())
                .cloned()
                .unwrap_or_else(|| ("Participant".to_string(), None));
            let display_name = scoped_name
                .and_then(decode_base64_text)
                .or_else(|| participant.as_ref().map(participant_display_name))
                .unwrap_or_else(|| short_identifier(participant_id));
            participants.push(RecipientParticipantEvidence {
                participant_id: participant_id.clone(),
                display_name,
                role,
            });
        }
        participants.sort_by(|left, right| {
            (&left.display_name, &left.participant_id)
                .cmp(&(&right.display_name, &right.participant_id))
        });
        let names = participants
            .iter()
            .map(|participant| participant.display_name.as_str())
            .take(4)
            .collect::<Vec<_>>();
        let human_label = if names.is_empty() {
            format!(
                "{:?} {}",
                conversation.kind,
                short_identifier(&conversation.conversation_id)
            )
        } else if participants.len() > names.len() {
            format!("{} +{}", names.join(", "), participants.len() - names.len())
        } else {
            names.join(", ")
        };
        let preserved_stale_source_set_ids = self.preserved_stale_sources()?;
        Ok(ResolvedConversation {
            conversation_id: conversation.conversation_id.clone(),
            kind: conversation.kind,
            human_label,
            participant_count: conversation.participant_ids.len(),
            participants,
            owner_participant_id: conversation.owner_participant_id.clone(),
            entity_decode_state: conversation.entity_decode_state,
            source_database_freshness: entity_source_database_freshness(
                conversation
                    .source_records
                    .iter()
                    .map(|record| record.source_set_id.clone()),
                &preserved_stale_source_set_ids,
            ),
            limitation_codes: limitation_codes.into_iter().collect(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn create_draft(
        &self,
        request: &ConnectorRequest,
        conversation_id: &str,
        reply_target_id: Option<&String>,
        rendered_text: &str,
        attachment_ids: &[String],
        expiry_seconds: Option<u64>,
    ) -> Result<DraftReceipt, ConnectorErrorBody> {
        if request.destination != ConnectorDestination::Local {
            return Err(unauthorized(
                "draft creation is restricted to the local destination",
            ));
        }
        let scope = self.authorize(
            request,
            conversation_id,
            ToolCapability::CreateDraft,
            "createDraft",
        )?;
        if rendered_text.len() > self.policy.maximum_draft_bytes
            || (rendered_text.is_empty() && attachment_ids.is_empty())
        {
            return Err(invalid(
                "draft requires text or an attachment and exceeds no configured size limit",
            ));
        }
        if attachment_ids.len() > 20 {
            return Err(invalid("a draft can contain at most 20 attachments"));
        }
        let expiry_seconds = expiry_seconds.unwrap_or(DEFAULT_DRAFT_EXPIRY_SECONDS);
        if expiry_seconds == 0 || expiry_seconds > MAX_DRAFT_EXPIRY_SECONDS {
            return Err(invalid("draft expiry is outside the supported range"));
        }
        let conversation = get_replica_conversation(&self.replica_path, self.key, conversation_id)
            .map_err(integrity_error)?
            .ok_or_else(|| not_found("conversation was not found"))?;
        let recipient = self.resolve_conversation(&conversation)?;
        let reply_target = reply_target_id
            .map(|identifier| self.resolve_reply_target(conversation_id, identifier, scope))
            .transpose()?;
        let mut attachment_ids = attachment_ids.to_vec();
        attachment_ids.sort();
        if attachment_ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(invalid("draft attachment IDs must be unique"));
        }
        let attachments = attachment_ids
            .iter()
            .map(|identifier| self.resolve_attachment(conversation_id, identifier, scope))
            .collect::<Result<Vec<_>, _>>()?;
        let status = replica_status(&self.replica_path, self.key).map_err(integrity_error)?;
        let source_fingerprint = status.current_source_fingerprint.ok_or_else(|| {
            unavailable(
                "replicaUninitialized",
                "Replica has no authoritative checkpoint",
            )
        })?;
        let created = unix_nanoseconds().map_err(integrity_error)?;
        let expires = created
            .checked_add(u128::from(expiry_seconds) * 1_000_000_000)
            .ok_or_else(|| invalid("draft expiry exceeds the supported range"))?;
        let policy_decision_id = self.policy_decision_id(
            request,
            conversation_id,
            reply_target_id.map(String::as_str),
            &attachment_ids,
            &source_fingerprint,
        );
        let text_sha256 = hex::encode(Sha256::digest(rendered_text.as_bytes()));
        let draft_id = draft_identity(
            &self.policy.account_id,
            conversation_id,
            &recipient,
            reply_target.as_ref(),
            &text_sha256,
            &attachments,
            &source_fingerprint,
            &policy_decision_id,
            &request.requester_id,
            CONNECTOR_VERSION,
            CONNECTOR_API_VERSION,
            created,
            expires,
        );
        let draft = ActionDraft {
            format_version: 1,
            draft_id: draft_id.clone(),
            state: DraftState::DraftOnly,
            account_id: self.policy.account_id.clone(),
            conversation_id: conversation_id.to_string(),
            recipient,
            reply_target,
            rendered_text: rendered_text.to_string(),
            rendered_text_sha256: text_sha256.clone(),
            attachments,
            connector_version: CONNECTOR_VERSION.to_string(),
            api_version: CONNECTOR_API_VERSION.to_string(),
            source_fingerprint,
            policy_decision_id: policy_decision_id.clone(),
            requester_id: request.requester_id.clone(),
            created_at_unix_nanoseconds: created,
            expires_at_unix_nanoseconds: expires,
        };
        let path = self.draft_directory.join(format!("{draft_id}.json"));
        write_owner_only_json(&path, &draft).map_err(integrity_error)?;
        if let Err(error) = self.audit(
            request,
            "createDraft",
            Some(conversation_id),
            ConnectorAuditOutcome::Completed,
            1,
            0,
            rendered_text.len(),
            Some(&draft_id),
            Some(&policy_decision_id),
        ) {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        Ok(DraftReceipt {
            draft_id,
            state: DraftState::DraftOnly,
            conversation_id: conversation_id.to_string(),
            human_recipient: draft.recipient.human_label,
            reply_target_canonical_id: draft.reply_target.map(|target| target.canonical_id),
            rendered_text_sha256: text_sha256,
            rendered_text_byte_count: rendered_text.len(),
            attachment_count: draft.attachments.len(),
            policy_decision_id,
            expires_at_unix_nanoseconds: expires,
        })
    }

    fn resolve_reply_target(
        &self,
        conversation_id: &str,
        canonical_id: &str,
        scope: &ConversationToolScope,
    ) -> Result<DraftReplyTarget, ConnectorErrorBody> {
        let message = get_replica_message(&self.replica_path, self.key, canonical_id)
            .map_err(integrity_error)?
            .ok_or_else(|| not_found("reply target message was not found"))?;
        if message.conversation_id != conversation_id {
            return Err(invalid("reply target belongs to a different conversation"));
        }
        if !scope.includes_message(&message) {
            return Err(unauthorized(
                "reply target is outside the authorized message time range",
            ));
        }
        let canonical_record_sha256 = hex::encode(Sha256::digest(
            serde_json::to_vec(&message).map_err(|error| integrity_error(error.into()))?,
        ));
        Ok(DraftReplyTarget {
            canonical_id: message.canonical_id,
            canonical_record_sha256,
            sender_id: message.sender_id,
            created_at_unix: message.created_at_unix,
        })
    }

    fn resolve_attachment(
        &self,
        conversation_id: &str,
        artifact_id: &str,
        scope: &ConversationToolScope,
    ) -> Result<DraftAttachment, ConnectorErrorBody> {
        if !replica_conversation_references_artifact_in_range(
            &self.replica_path,
            self.key,
            conversation_id,
            artifact_id,
            scope.not_before_unix,
            scope.not_after_unix,
        )
        .map_err(integrity_error)?
        {
            return Err(unauthorized(
                "attachment is not referenced by the drafted conversation",
            ));
        }
        let artifact = get_replica_artifact(&self.replica_path, self.key, artifact_id)
            .map_err(integrity_error)?
            .ok_or_else(|| not_found("draft attachment was not found"))?;
        let (digest_kind, sha256, byte_count) =
            if let Some(digest) = artifact.decoded_sha256.clone() {
                ("decodedSha256", digest, artifact.decoded_byte_count)
            } else if let Some(digest) = artifact.source_sha256.clone() {
                ("sourceSha256", digest, artifact.source_byte_count)
            } else {
                return Err(unavailable(
                    "attachmentDigestUnavailable",
                    "Attachment cannot be bound to a draft without a verified SHA-256 digest",
                ));
            };
        if !valid_sha256(&sha256) {
            return Err(unavailable(
                "attachmentDigestInvalid",
                "Attachment digest is malformed and cannot be bound to an immutable draft",
            ));
        }
        let byte_count = byte_count.ok_or_else(|| {
            unavailable(
                "attachmentSizeUnavailable",
                "Attachment cannot be bound to a draft without a verified byte count",
            )
        })?;
        let display_file_name = artifact
            .account_relative_path
            .as_deref()
            .and_then(|path| Path::new(path).file_name())
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("attachment")
            .to_string();
        Ok(DraftAttachment {
            artifact_id: artifact.artifact_id,
            kind: artifact.kind,
            role: artifact.role,
            digest_kind: digest_kind.to_string(),
            sha256,
            byte_count: Some(byte_count),
            display_file_name,
        })
    }

    fn preview(
        &self,
        request: &ConnectorRequest,
        draft_id: &str,
    ) -> Result<ActionPreview, ConnectorErrorBody> {
        if request.destination != ConnectorDestination::Local {
            return Err(unauthorized("draft previews are local-only"));
        }
        if !valid_sha256(draft_id) {
            return Err(invalid("draft ID is malformed"));
        }
        let path = self.draft_directory.join(format!("{draft_id}.json"));
        ensure_private_regular_file(&path).map_err(|_| not_found("draft was not found"))?;
        let draft: ActionDraft = serde_json::from_slice(
            &fs::read(&path).map_err(|error| integrity_error(error.into()))?,
        )
        .map_err(|error| integrity_error(error.into()))?;
        self.validate_draft(&draft)?;
        self.authorize(
            request,
            &draft.conversation_id,
            ToolCapability::CreateDraft,
            "previewAction",
        )?;
        let expired =
            unix_nanoseconds().map_err(integrity_error)? > draft.expires_at_unix_nanoseconds;
        self.audit(
            request,
            "previewAction",
            Some(&draft.conversation_id),
            ConnectorAuditOutcome::Completed,
            1,
            draft.rendered_text.len(),
            0,
            Some(&draft.draft_id),
            Some(&draft.policy_decision_id),
        )?;
        Ok(ActionPreview {
            draft,
            expired,
            executable: false,
            execution_unavailable_reason:
                "No write adapter exists and the Phase 0.5 action gate has not passed".to_string(),
            warning: "Preview only: this record cannot mutate WeChat or contact any recipient"
                .to_string(),
        })
    }

    fn validate_draft(&self, draft: &ActionDraft) -> Result<(), ConnectorErrorBody> {
        validate_draft_structure(draft)
            .map_err(|_| conflict("draft binding evidence is invalid or stale"))?;
        if draft.account_id != self.policy.account_id
            || draft.api_version != CONNECTOR_API_VERSION
            || draft.connector_version != CONNECTOR_VERSION
            || draft.rendered_text.len() > self.policy.maximum_draft_bytes
        {
            return Err(conflict("draft binding evidence is invalid or stale"));
        }
        let status = replica_status(&self.replica_path, self.key).map_err(integrity_error)?;
        if !status
            .current_source_fingerprint
            .as_deref()
            .is_some_and(|source| self.draft_current_binding_matches(draft, source))
        {
            return Err(conflict(
                "draft policy or replica checkpoint is stale; create a new draft",
            ));
        }
        Ok(())
    }

    fn draft_current_binding_matches(
        &self,
        draft: &ActionDraft,
        current_source_fingerprint: &str,
    ) -> bool {
        if draft.account_id != self.policy.account_id
            || draft.api_version != CONNECTOR_API_VERSION
            || draft.connector_version != CONNECTOR_VERSION
            || draft.rendered_text.len() > self.policy.maximum_draft_bytes
            || draft.source_fingerprint != current_source_fingerprint
        {
            return false;
        }
        let attachment_ids = draft
            .attachments
            .iter()
            .map(|attachment| attachment.artifact_id.clone())
            .collect::<Vec<_>>();
        self.policy_decision_identity(
            &draft.requester_id,
            &draft.conversation_id,
            draft
                .reply_target
                .as_ref()
                .map(|target| target.canonical_id.as_str()),
            &attachment_ids,
            &draft.source_fingerprint,
        ) == draft.policy_decision_id
    }

    fn authorize(
        &self,
        request: &ConnectorRequest,
        conversation_id: &str,
        capability: ToolCapability,
        operation: &str,
    ) -> Result<&ConversationToolScope, ConnectorErrorBody> {
        let destination = ToolDataDestination::from(request.destination);
        let scope = self.policy.conversation_scopes.get(conversation_id);
        if let Some(scope) = scope.filter(|scope| {
            scope.capabilities.contains(&capability)
                && (destination == ToolDataDestination::LocalModel || scope.allow_remote_model)
        }) {
            return Ok(scope);
        }
        let _ = self.audit(
            request,
            operation,
            Some(conversation_id),
            ConnectorAuditOutcome::Denied,
            0,
            0,
            0,
            None,
            None,
        );
        Err(unauthorized(
            "operation is outside the authorized conversation or destination scope",
        ))
    }

    fn authorize_cached_moments(
        &self,
        request: &ConnectorRequest,
    ) -> Result<&CachedMomentsToolScope, ConnectorErrorBody> {
        let destination = ToolDataDestination::from(request.destination);
        if let Some(scope) = self.policy.cached_moments_scope.as_ref().filter(|scope| {
            destination == ToolDataDestination::LocalModel || scope.allow_remote_model
        }) {
            return Ok(scope);
        }
        let _ = self.audit(
            request,
            "getCachedMoments",
            None,
            ConnectorAuditOutcome::Denied,
            0,
            0,
            0,
            None,
            None,
        );
        Err(unauthorized(
            "cached Moments access is outside its independent policy or destination scope",
        ))
    }

    fn enforce_cached_moment_rate(
        &self,
        request: &ConnectorRequest,
    ) -> Result<(), ConnectorErrorBody> {
        let now = unix_nanoseconds().map_err(integrity_error)?;
        let window_start = now.saturating_sub(60 * 1_000_000_000);
        let mut timestamps = self.cached_moment_request_times.lock().map_err(|_| {
            integrity_error(RestoreError::Integrity(
                "cached Moments rate limiter is unavailable".to_string(),
            ))
        })?;
        while timestamps
            .front()
            .is_some_and(|timestamp| *timestamp < window_start)
        {
            timestamps.pop_front();
        }
        if timestamps.len() >= MAX_CACHED_MOMENT_REQUESTS_PER_MINUTE {
            drop(timestamps);
            let _ = self.audit(
                request,
                "getCachedMoments",
                None,
                ConnectorAuditOutcome::Denied,
                0,
                0,
                0,
                None,
                None,
            );
            return Err(unavailable(
                "cachedMomentsRateLimited",
                "Passive cached Moments reads are limited to 60 requests per rolling minute",
            ));
        }
        timestamps.push_back(now);
        Ok(())
    }

    fn policy_decision_id(
        &self,
        request: &ConnectorRequest,
        conversation_id: &str,
        reply_target_id: Option<&str>,
        attachment_ids: &[String],
        source_fingerprint: &str,
    ) -> String {
        self.policy_decision_identity(
            &request.requester_id,
            conversation_id,
            reply_target_id,
            attachment_ids,
            source_fingerprint,
        )
    }

    fn policy_decision_identity(
        &self,
        requester_id: &str,
        conversation_id: &str,
        reply_target_id: Option<&str>,
        attachment_ids: &[String],
        source_fingerprint: &str,
    ) -> String {
        let mut hasher = Sha256::new();
        for value in [
            self.policy_sha256.as_str(),
            self.policy.account_id.as_str(),
            source_fingerprint,
            requester_id,
            conversation_id,
            reply_target_id.unwrap_or(""),
        ] {
            hasher.update(value.as_bytes());
            hasher.update([0]);
        }
        for identifier in attachment_ids {
            hasher.update(identifier.as_bytes());
            hasher.update([0]);
        }
        hex::encode(hasher.finalize())
    }

    #[allow(clippy::too_many_arguments)]
    fn audit(
        &self,
        request: &ConnectorRequest,
        operation: &str,
        conversation_id: Option<&str>,
        outcome: ConnectorAuditOutcome,
        returned_item_count: usize,
        released_body_byte_count: usize,
        request_body_byte_count: usize,
        draft_id: Option<&str>,
        policy_decision_id: Option<&str>,
    ) -> Result<(), ConnectorErrorBody> {
        let observed = unix_nanoseconds().map_err(integrity_error)?;
        let identity = format!(
            "{}:{}:{}:{operation}:{conversation_id:?}:{outcome:?}:{observed}",
            self.policy.account_id, request.requester_id, request.request_id
        );
        let event = ConnectorAuditEvent {
            format_version: 2,
            event_id: hex::encode(Sha256::digest(identity.as_bytes())),
            observed_at_unix_nanoseconds: observed,
            account_id: self.policy.account_id.clone(),
            requester_id: request.requester_id.clone(),
            request_id: request.request_id.clone(),
            operation: operation.to_string(),
            stage: match operation {
                "createDraft" => ConnectorAuditStage::DraftRequested,
                "previewAction" => ConnectorAuditStage::DraftReviewed,
                _ => ConnectorAuditStage::Request,
            },
            conversation_id: conversation_id.map(str::to_string),
            destination: request.destination,
            outcome,
            returned_item_count,
            released_body_byte_count,
            request_body_byte_count,
            draft_id: draft_id.map(str::to_string),
            policy_decision_id: policy_decision_id.map(str::to_string),
            previous_event_sha256: None,
            event_sha256: String::new(),
        };
        append_owner_only_connector_event(&self.audit_path, event).map_err(integrity_error)
    }

    fn audit_metadata(
        &self,
        request: &ConnectorRequest,
        operation: &str,
    ) -> Result<(), ConnectorErrorBody> {
        self.audit(
            request,
            operation,
            None,
            ConnectorAuditOutcome::Completed,
            1,
            0,
            0,
            None,
            None,
        )
    }
}

impl ConnectorRequestHandler for ConnectorService<'_> {
    fn handle_connector_request(&self, request: ConnectorRequest) -> ConnectorResponse {
        self.handle(request)
    }
}

fn linked_audit_draft<'a>(
    event: &ConnectorAuditEvent,
    drafts: &'a BTreeMap<String, ActionDraft>,
    expected_operation: &str,
) -> Result<&'a ActionDraft, RestoreError> {
    let draft_id = event.draft_id.as_ref().ok_or_else(|| {
        RestoreError::Integrity("completed draft audit event lacks a draft identity".to_string())
    })?;
    let draft = drafts.get(draft_id).ok_or_else(|| {
        RestoreError::Integrity("completed draft audit event has no draft file".to_string())
    })?;
    if event.operation != expected_operation
        || event.account_id != draft.account_id
        || event.conversation_id.as_deref() != Some(draft.conversation_id.as_str())
        || event.policy_decision_id.as_deref() != Some(draft.policy_decision_id.as_str())
    {
        return Err(RestoreError::Integrity(
            "connector draft audit linkage is inconsistent".to_string(),
        ));
    }
    Ok(draft)
}

fn validate_current_connector_stage_evidence(
    event: &ConnectorAuditEvent,
) -> Result<(), RestoreError> {
    let valid = match (event.stage, event.outcome) {
        (ConnectorAuditStage::Request, _) => {
            event.draft_id.is_none() && event.policy_decision_id.is_none()
        }
        (ConnectorAuditStage::DraftRequested, ConnectorAuditOutcome::Completed) => {
            event.operation == "createDraft"
                && event.draft_id.is_some()
                && event.policy_decision_id.is_some()
        }
        (ConnectorAuditStage::DraftRequested, ConnectorAuditOutcome::Denied) => {
            event.operation == "createDraft"
                && event.draft_id.is_none()
                && event.policy_decision_id.is_none()
        }
        (ConnectorAuditStage::DraftReviewed, ConnectorAuditOutcome::Completed) => {
            event.operation == "previewAction"
                && event.draft_id.is_some()
                && event.policy_decision_id.is_some()
        }
        (ConnectorAuditStage::DraftReviewed, ConnectorAuditOutcome::Denied) => {
            event.operation == "previewAction"
                && event.draft_id.is_none()
                && event.policy_decision_id.is_none()
        }
        (
            ConnectorAuditStage::ApprovalRecorded
            | ConnectorAuditStage::AttemptRecorded
            | ConnectorAuditStage::ReconciliationRecorded,
            _,
        ) => false,
    };
    if !valid {
        return Err(RestoreError::Integrity(
            "connector journal stage evidence is invalid for the current product phase".to_string(),
        ));
    }
    Ok(())
}

fn read_connector_draft_file(path: &Path) -> Result<Vec<u8>, RestoreError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let before = file.metadata()?;
    if !before.is_file()
        || before.permissions().mode() & 0o077 != 0
        || before.nlink() != 1
        || before.len() > MAX_CONNECTOR_DRAFT_BYTES
    {
        return Err(RestoreError::Integrity(
            "connector draft must be a bounded owner-only single-link regular file".to_string(),
        ));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(MAX_CONNECTOR_DRAFT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() as u64 != before.len()
        || bytes.len() as u64 > MAX_CONNECTOR_DRAFT_BYTES
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(RestoreError::Integrity(
            "connector draft changed while it was being verified".to_string(),
        ));
    }
    Ok(bytes)
}

fn connector_draft_store_snapshot(
    directory: &Path,
) -> Result<BTreeMap<String, String>, RestoreError> {
    let mut snapshot = BTreeMap::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if snapshot.len() >= MAX_CONNECTOR_DRAFT_COUNT {
            return Err(RestoreError::Integrity(
                "connector draft store exceeds the verification limit".to_string(),
            ));
        }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            return Err(RestoreError::Integrity(
                "connector draft store contains an unsupported entry".to_string(),
            ));
        }
        let file_id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .filter(|value| valid_sha256(value))
            .ok_or_else(|| {
                RestoreError::Integrity(
                    "connector draft filename is not an immutable draft identity".to_string(),
                )
            })?;
        let bytes = read_connector_draft_file(&path)?;
        if snapshot
            .insert(file_id.to_string(), hex::encode(Sha256::digest(bytes)))
            .is_some()
        {
            return Err(RestoreError::Integrity(
                "connector draft store repeats an immutable identity".to_string(),
            ));
        }
    }
    Ok(snapshot)
}

fn audit_event_snapshot(events: &[ConnectorAuditEvent]) -> Result<String, RestoreError> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(events)?)))
}

fn validate_draft_structure(draft: &ActionDraft) -> Result<(), RestoreError> {
    let maximum_expiry_nanoseconds =
        u128::from(MAX_DRAFT_EXPIRY_SECONDS).saturating_mul(1_000_000_000);
    let participant_ids = draft
        .recipient
        .participants
        .iter()
        .map(|participant| participant.participant_id.as_str())
        .collect::<BTreeSet<_>>();
    let attachment_ids = draft
        .attachments
        .iter()
        .map(|attachment| attachment.artifact_id.as_str())
        .collect::<BTreeSet<_>>();
    let participants_valid = participant_ids.len() == draft.recipient.participants.len()
        && draft.recipient.participants.iter().all(|participant| {
            !participant.participant_id.is_empty()
                && !participant.display_name.is_empty()
                && !participant.role.is_empty()
        });
    let attachments_valid = attachment_ids.len() == draft.attachments.len()
        && draft.attachments.len() <= 20
        && draft.attachments.iter().all(|attachment| {
            !attachment.artifact_id.is_empty()
                && matches!(
                    attachment.digest_kind.as_str(),
                    "decodedSha256" | "sourceSha256"
                )
                && valid_sha256(&attachment.sha256)
                && attachment.byte_count.is_some()
                && !attachment.display_file_name.is_empty()
                && !attachment.display_file_name.contains('/')
                && !attachment.display_file_name.contains('\0')
                && !matches!(attachment.display_file_name.as_str(), "." | "..")
        });
    let reply_valid = draft.reply_target.as_ref().is_none_or(|target| {
        !target.canonical_id.is_empty()
            && valid_sha256(&target.canonical_record_sha256)
            && target
                .sender_id
                .as_ref()
                .is_none_or(|sender| !sender.is_empty())
    });
    let expected = draft_identity(
        &draft.account_id,
        &draft.conversation_id,
        &draft.recipient,
        draft.reply_target.as_ref(),
        &draft.rendered_text_sha256,
        &draft.attachments,
        &draft.source_fingerprint,
        &draft.policy_decision_id,
        &draft.requester_id,
        &draft.connector_version,
        &draft.api_version,
        draft.created_at_unix_nanoseconds,
        draft.expires_at_unix_nanoseconds,
    );
    if draft.format_version != 1
        || draft.state != DraftState::DraftOnly
        || !valid_sha256(&draft.draft_id)
        || draft.account_id.is_empty()
        || draft.conversation_id.is_empty()
        || draft.requester_id.is_empty()
        || draft.requester_id.len() > MAX_REQUESTER_ID_BYTES
        || draft.connector_version.is_empty()
        || draft.connector_version.len() > 256
        || draft.api_version.is_empty()
        || draft.api_version.len() > 256
        || draft.source_fingerprint.is_empty()
        || !valid_sha256(&draft.policy_decision_id)
        || draft.rendered_text.len() as u64 > MAX_CONNECTOR_DRAFT_BYTES
        || (draft.rendered_text.is_empty() && draft.attachments.is_empty())
        || draft.rendered_text_sha256 != hex::encode(Sha256::digest(draft.rendered_text.as_bytes()))
        || draft.created_at_unix_nanoseconds >= draft.expires_at_unix_nanoseconds
        || draft.expires_at_unix_nanoseconds - draft.created_at_unix_nanoseconds
            > maximum_expiry_nanoseconds
        || draft.recipient.conversation_id != draft.conversation_id
        || draft.recipient.human_label.is_empty()
        || draft.recipient.participant_count != draft.recipient.participants.len()
        || !participants_valid
        || !attachments_valid
        || !reply_valid
        || expected != draft.draft_id
    {
        return Err(RestoreError::Integrity(
            "connector draft immutable structure is invalid".to_string(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draft_identity(
    account_id: &str,
    conversation_id: &str,
    recipient: &ResolvedConversation,
    reply_target: Option<&DraftReplyTarget>,
    text_sha256: &str,
    attachments: &[DraftAttachment],
    source_fingerprint: &str,
    policy_decision_id: &str,
    requester_id: &str,
    connector_version: &str,
    api_version: &str,
    created: u128,
    expires: u128,
) -> String {
    let mut hasher = Sha256::new();
    for value in [
        account_id,
        conversation_id,
        text_sha256,
        source_fingerprint,
        policy_decision_id,
        requester_id,
        connector_version,
        api_version,
    ] {
        hasher.update(value.as_bytes());
        hasher.update([0]);
    }
    hasher
        .update(serde_json::to_vec(recipient).expect("recipient draft evidence always serializes"));
    hasher.update([0]);
    hasher
        .update(serde_json::to_vec(&reply_target).expect("reply draft evidence always serializes"));
    hasher.update([0]);
    hasher.update(
        serde_json::to_vec(attachments).expect("attachment draft evidence always serializes"),
    );
    hasher.update(created.to_le_bytes());
    hasher.update(expires.to_le_bytes());
    hex::encode(hasher.finalize())
}

fn participant_display_name(participant: &CanonicalParticipant) -> String {
    [
        participant.display_name_base64.as_deref(),
        participant.remark_base64.as_deref(),
        participant.nickname_base64.as_deref(),
        participant.alias_base64.as_deref(),
    ]
    .into_iter()
    .flatten()
    .find_map(decode_base64_text)
    .unwrap_or_else(|| short_identifier(&participant.participant_id))
}

fn derived_conversation(conversation_id: &str) -> ResolvedConversation {
    ResolvedConversation {
        conversation_id: conversation_id.to_string(),
        kind: ConversationKind::Unresolved,
        human_label: format!("Unresolved {}", short_identifier(conversation_id)),
        participant_count: 0,
        participants: Vec::new(),
        owner_participant_id: None,
        entity_decode_state: EntityDecodeState::Failed,
        source_database_freshness: ToolSourceDatabaseFreshness::Derived,
        limitation_codes: vec!["unavailableConversationMetadataSynthesized".to_string()],
    }
}

fn extend_message_projection_limitations(
    messages: &[MinimizedMessage],
    limitation_codes: &mut BTreeSet<String>,
) {
    if messages
        .iter()
        .any(|message| message.omitted_artifact_reference_count > 0)
    {
        limitation_codes.insert("malformedArtifactReferenceOmitted".to_string());
    }
    if messages
        .iter()
        .any(|message| message.omitted_relationship_reference_count > 0)
    {
        limitation_codes.insert("malformedRelationshipReferenceOmitted".to_string());
    }
}

fn decode_base64_text(value: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .ok()?;
    let value = String::from_utf8(bytes).ok()?;
    (!value.trim().is_empty()).then_some(value)
}

fn short_identifier(value: &str) -> String {
    value.chars().take(12).collect()
}

fn unix_nanoseconds() -> Result<u128, RestoreError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(|_| RestoreError::Integrity("system clock predates Unix epoch".to_string()))
}

fn write_owner_only_json(path: &Path, value: &impl Serialize) -> Result<(), RestoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath("private output has no parent".to_string()))?;
    ensure_private_directory(parent)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}

pub(crate) fn append_owner_only_connector_event(
    path: &Path,
    mut event: ConnectorAuditEvent,
) -> Result<(), RestoreError> {
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 || metadata.nlink() != 1 {
        return Err(RestoreError::Integrity(
            "connector audit log must be an owner-only regular file with one link".to_string(),
        ));
    }
    let descriptor = std::os::fd::AsRawFd::as_raw_fd(&file);
    if unsafe { libc::flock(descriptor, libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let result = (|| -> Result<(), RestoreError> {
        let previous = read_last_audit_line(&mut file)?
            .as_deref()
            .map(previous_audit_event_digest)
            .transpose()?;
        event.format_version = 2;
        event.previous_event_sha256 = previous;
        event.event_sha256 = connector_audit_event_digest(&event)?;
        validate_connector_audit_event(&event)?;
        serde_json::to_writer(&mut file, &event)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        Ok(())
    })();
    let unlock = unsafe { libc::flock(descriptor, libc::LOCK_UN) };
    result?;
    if unlock != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

pub fn audit_connector_log(path: &Path) -> Result<ConnectorAuditReport, RestoreError> {
    audit_connector_log_for_account(path, None)
}

pub(crate) fn audit_connector_log_for_account(
    path: &Path,
    expected_account_id: Option<&str>,
) -> Result<ConnectorAuditReport, RestoreError> {
    verified_connector_log_for_account(path, expected_account_id).map(|(report, _)| report)
}

fn verified_connector_log_for_account(
    path: &Path,
    expected_account_id: Option<&str>,
) -> Result<(ConnectorAuditReport, Vec<ConnectorAuditEvent>), RestoreError> {
    ensure_private_regular_file(path)?;
    let file = File::open(path)?;
    let descriptor = std::os::fd::AsRawFd::as_raw_fd(&file);
    if unsafe { libc::flock(descriptor, libc::LOCK_SH) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let result = audit_connector_log_file(&file, expected_account_id);
    let unlock = unsafe { libc::flock(descriptor, libc::LOCK_UN) };
    let report = result?;
    if unlock != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(report)
}

fn audit_connector_log_file(
    file: &File,
    expected_account_id: Option<&str>,
) -> Result<(ConnectorAuditReport, Vec<ConnectorAuditEvent>), RestoreError> {
    if file.metadata()?.len() > MAX_CONNECTOR_AUDIT_BYTES {
        return Err(RestoreError::Integrity(
            "connector audit log exceeds the verification limit".to_string(),
        ));
    }
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut previous_digest: Option<String> = None;
    let mut seen_chained = false;
    let mut account_id: Option<String> = None;
    let mut event_ids = BTreeSet::new();
    let mut events = Vec::new();
    let mut report = ConnectorAuditReport {
        format_version: 1,
        privacy_safe_summary: true,
        chain_verified: true,
        fully_chained: true,
        event_count: 0,
        legacy_unchained_event_count: 0,
        chained_event_count: 0,
        completed_event_count: 0,
        denied_event_count: 0,
        draft_requested_event_count: 0,
        draft_reviewed_event_count: 0,
        approval_event_count: 0,
        attempt_event_count: 0,
        reconciliation_event_count: 0,
    };
    loop {
        line.clear();
        let bytes_read = reader.read_until(b'\n', &mut line)?;
        if bytes_read == 0 {
            break;
        }
        while line
            .last()
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
        {
            line.pop();
        }
        if line.is_empty() || line.len() > MAX_CONNECTOR_AUDIT_RECORD_BYTES {
            return Err(RestoreError::Integrity(
                "connector audit log contains an empty or oversized record".to_string(),
            ));
        }
        let event: ConnectorAuditEvent = serde_json::from_slice(&line)?;
        validate_connector_audit_event(&event)?;
        if expected_account_id.is_some_and(|expected| event.account_id != expected) {
            return Err(RestoreError::Integrity(
                "connector audit log belongs to a different account".to_string(),
            ));
        }
        if account_id
            .as_ref()
            .is_some_and(|account| account != &event.account_id)
        {
            return Err(RestoreError::Integrity(
                "connector audit log mixes account identities".to_string(),
            ));
        }
        account_id.get_or_insert_with(|| event.account_id.clone());
        if !event_ids.insert(event.event_id.clone()) {
            return Err(RestoreError::Integrity(
                "connector audit log repeats an event identity".to_string(),
            ));
        }
        match event.format_version {
            1 => {
                if seen_chained
                    || event.previous_event_sha256.is_some()
                    || !event.event_sha256.is_empty()
                {
                    return Err(RestoreError::Integrity(
                        "legacy connector audit record appears inside a chained suffix".to_string(),
                    ));
                }
                previous_digest = Some(hex::encode(Sha256::digest(&line)));
                report.legacy_unchained_event_count += 1;
                report.fully_chained = false;
            }
            2 => {
                if event.previous_event_sha256 != previous_digest {
                    return Err(RestoreError::Integrity(
                        "connector audit chain predecessor does not match".to_string(),
                    ));
                }
                let expected = connector_audit_event_digest(&event)?;
                if event.event_sha256 != expected {
                    return Err(RestoreError::Integrity(
                        "connector audit event digest does not match its contents".to_string(),
                    ));
                }
                previous_digest = Some(event.event_sha256.clone());
                seen_chained = true;
                report.chained_event_count += 1;
            }
            _ => {
                return Err(RestoreError::Integrity(
                    "unsupported connector audit event format".to_string(),
                ));
            }
        }
        report.event_count += 1;
        match event.outcome {
            ConnectorAuditOutcome::Completed => report.completed_event_count += 1,
            ConnectorAuditOutcome::Denied => report.denied_event_count += 1,
        }
        match event.stage {
            ConnectorAuditStage::DraftRequested => report.draft_requested_event_count += 1,
            ConnectorAuditStage::DraftReviewed => report.draft_reviewed_event_count += 1,
            ConnectorAuditStage::ApprovalRecorded => report.approval_event_count += 1,
            ConnectorAuditStage::AttemptRecorded => report.attempt_event_count += 1,
            ConnectorAuditStage::ReconciliationRecorded => {
                report.reconciliation_event_count += 1;
            }
            ConnectorAuditStage::Request => {}
        }
        events.push(event);
    }
    Ok((report, events))
}

fn validate_connector_audit_event(event: &ConnectorAuditEvent) -> Result<(), RestoreError> {
    if !matches!(event.format_version, 1 | 2)
        || !valid_sha256(&event.event_id)
        || event.account_id.is_empty()
        || event.requester_id.is_empty()
        || event.request_id.is_empty()
        || event.operation.is_empty()
        || event
            .draft_id
            .as_ref()
            .is_some_and(|value| !valid_sha256(value))
        || event
            .policy_decision_id
            .as_ref()
            .is_some_and(|value| !valid_sha256(value))
        || event
            .previous_event_sha256
            .as_ref()
            .is_some_and(|value| !valid_sha256(value))
        || (!event.event_sha256.is_empty() && !valid_sha256(&event.event_sha256))
    {
        return Err(RestoreError::Integrity(
            "connector audit event is malformed".to_string(),
        ));
    }
    Ok(())
}

fn connector_audit_event_digest(event: &ConnectorAuditEvent) -> Result<String, RestoreError> {
    let mut canonical = event.clone();
    canonical.event_sha256.clear();
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&canonical)?)))
}

fn previous_audit_event_digest(line: &[u8]) -> Result<String, RestoreError> {
    let event: ConnectorAuditEvent = serde_json::from_slice(line)?;
    validate_connector_audit_event(&event)?;
    match event.format_version {
        1 if event.previous_event_sha256.is_none() && event.event_sha256.is_empty() => {
            Ok(hex::encode(Sha256::digest(line)))
        }
        2 if connector_audit_event_digest(&event)? == event.event_sha256 => Ok(event.event_sha256),
        1 => Err(RestoreError::Integrity(
            "legacy connector audit event contains chained fields".to_string(),
        )),
        _ => Err(RestoreError::Integrity(
            "connector audit tail is unsupported or corrupt".to_string(),
        )),
    }
}

fn read_last_audit_line(file: &mut File) -> Result<Option<Vec<u8>>, RestoreError> {
    let mut position = file.metadata()?.len();
    if position == 0 {
        return Ok(None);
    }
    let mut reversed = Vec::new();
    let mut byte = [0_u8; 1];
    while position > 0 {
        position -= 1;
        file.seek(SeekFrom::Start(position))?;
        file.read_exact(&mut byte)?;
        if matches!(byte[0], b'\n' | b'\r') {
            if reversed.is_empty() {
                continue;
            }
            break;
        }
        reversed.push(byte[0]);
        if reversed.len() > MAX_CONNECTOR_AUDIT_RECORD_BYTES {
            return Err(RestoreError::Integrity(
                "connector audit tail exceeds the record limit".to_string(),
            ));
        }
    }
    reversed.reverse();
    if reversed.is_empty() {
        return Err(RestoreError::Integrity(
            "connector audit log contains no complete record".to_string(),
        ));
    }
    Ok(Some(reversed))
}

fn connector_artifact_view(
    artifact: CanonicalArtifact,
) -> Result<ConnectorArtifactView, RestoreError> {
    let source = if let Some(path) = artifact.source_local_path.as_ref() {
        Some(connector_artifact_file(
            ConnectorArtifactFileOrigin::DownloadedSource,
            path,
            artifact.account_relative_path.clone(),
            artifact.source_byte_count,
            artifact.source_sha256.as_deref(),
            artifact.detected_format.as_deref(),
        )?)
    } else if let Some(path) = artifact.materialized_local_path.as_ref() {
        Some(connector_artifact_file(
            ConnectorArtifactFileOrigin::DatabaseMaterializedSource,
            path,
            None,
            artifact.source_byte_count,
            artifact.source_sha256.as_deref(),
            artifact.detected_format.as_deref(),
        )?)
    } else {
        None
    };
    let decoded = artifact
        .decoded_local_path
        .as_ref()
        .map(|path| {
            connector_artifact_file(
                ConnectorArtifactFileOrigin::DecodedDerivative,
                path,
                None,
                artifact.decoded_byte_count,
                artifact.decoded_sha256.as_deref(),
                artifact.decoded_format.as_deref(),
            )
        })
        .transpose()?;
    let verification_detail = artifact
        .verification_detail
        .clone()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            RestoreError::Integrity("artifact lacks verification evidence".to_string())
        })?;
    Ok(ConnectorArtifactView {
        artifact_id: artifact.artifact_id,
        kind: artifact.kind,
        role: artifact.role,
        availability: artifact.availability,
        decode_state: artifact.decode_state,
        source,
        decoded,
        verification_detail,
        limitation_codes: Vec::new(),
    })
}

fn unavailable_artifact(artifact_id: &str) -> ConnectorArtifactView {
    ConnectorArtifactView {
        artifact_id: artifact_id.to_string(),
        kind: ArtifactKind::Unknown,
        role: ArtifactRole::Unknown,
        availability: ArtifactAvailability::MetadataMissing,
        decode_state: ArtifactDecodeState::Failed,
        source: None,
        decoded: None,
        verification_detail: "artifactMetadataUnavailable".to_string(),
        limitation_codes: vec!["unavailableArtifactMetadataSynthesized".to_string()],
    }
}

fn connector_artifact_file(
    origin: ConnectorArtifactFileOrigin,
    path: &str,
    account_relative_path: Option<String>,
    byte_count: Option<u64>,
    sha256: Option<&str>,
    format: Option<&str>,
) -> Result<ConnectorArtifactFile, RestoreError> {
    let byte_count = byte_count.ok_or_else(|| {
        RestoreError::Integrity("artifact file lacks its verified byte count".to_string())
    })?;
    let sha256 = sha256.filter(|value| valid_sha256(value)).ok_or_else(|| {
        RestoreError::Integrity("artifact file lacks its verified SHA-256".to_string())
    })?;
    let format = format.filter(|value| !value.is_empty()).ok_or_else(|| {
        RestoreError::Integrity("artifact file lacks its detected format".to_string())
    })?;
    Ok(ConnectorArtifactFile {
        origin,
        absolute_path: path.to_string(),
        account_relative_path,
        byte_count,
        sha256: sha256.to_string(),
        format: format.to_string(),
    })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn connector_checkpoint_binding(
    status: &ReplicaStatus,
) -> Result<ConnectorCheckpointBinding, RestoreError> {
    Ok(ConnectorCheckpointBinding {
        source_fingerprint: status.current_source_fingerprint.clone().ok_or_else(|| {
            RestoreError::Integrity(
                "connector replica has no current source fingerprint".to_string(),
            )
        })?,
        checkpoint_revision: status.checkpoint_revision.clone().ok_or_else(|| {
            RestoreError::Integrity("connector replica has no checkpoint revision".to_string())
        })?,
    })
}

pub(crate) fn encode_connector_conversation_cursor(
    source_identity: &str,
    policy_sha256: &str,
    destination: ConnectorDestination,
    after_conversation_id: &str,
) -> Result<String, ConnectorErrorBody> {
    let bytes = serde_json::to_vec(&ConnectorConversationCursor {
        version: 1,
        source_identity: source_identity.to_string(),
        policy_sha256: policy_sha256.to_string(),
        destination,
        after_conversation_id: after_conversation_id.to_string(),
    })
    .map_err(|_| invalid("conversation cursor could not be encoded"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

pub(crate) fn decode_connector_conversation_cursor(
    value: Option<&str>,
    source_identity: &str,
    policy_sha256: &str,
    destination: ConnectorDestination,
) -> Result<Option<String>, ConnectorErrorBody> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_empty() || value.len() > 4_096 {
        return Err(invalid(
            "conversation cursor is empty or outside safe limits",
        ));
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| invalid("conversation cursor is not valid base64url"))?;
    if bytes.len() > 4_096 {
        return Err(invalid("conversation cursor is outside safe limits"));
    }
    let cursor: ConnectorConversationCursor = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("conversation cursor structure is invalid"))?;
    if cursor.version != 1
        || cursor.source_identity != source_identity
        || cursor.policy_sha256 != policy_sha256
        || cursor.destination != destination
        || cursor.after_conversation_id.is_empty()
    {
        return Err(invalid(
            "conversation cursor does not belong to this source, policy, and destination",
        ));
    }
    Ok(Some(cursor.after_conversation_id))
}

pub(crate) fn connector_response(
    request_id: String,
    result: Result<ConnectorResult, ConnectorErrorBody>,
) -> ConnectorResponse {
    match result {
        Ok(result) => ConnectorResponse {
            api_version: CONNECTOR_API_VERSION.to_string(),
            request_id,
            ok: true,
            result: Some(result),
            error: None,
        },
        Err(failure) => ConnectorResponse {
            api_version: CONNECTOR_API_VERSION.to_string(),
            request_id,
            ok: false,
            result: None,
            error: Some(failure),
        },
    }
}

pub(crate) fn invalid(message: &str) -> ConnectorErrorBody {
    ConnectorErrorBody {
        code: ConnectorErrorCode::InvalidRequest,
        message: message.to_string(),
        retryable: false,
    }
}

pub(crate) fn unauthorized(message: &str) -> ConnectorErrorBody {
    ConnectorErrorBody {
        code: ConnectorErrorCode::Unauthorized,
        message: message.to_string(),
        retryable: false,
    }
}

pub(crate) fn not_found(message: &str) -> ConnectorErrorBody {
    ConnectorErrorBody {
        code: ConnectorErrorCode::NotFound,
        message: message.to_string(),
        retryable: false,
    }
}

pub(crate) fn unavailable(code: &str, message: &str) -> ConnectorErrorBody {
    ConnectorErrorBody {
        code: ConnectorErrorCode::Unavailable,
        message: format!("{code}: {message}"),
        retryable: false,
    }
}

fn conflict(message: &str) -> ConnectorErrorBody {
    ConnectorErrorBody {
        code: ConnectorErrorCode::Conflict,
        message: message.to_string(),
        retryable: false,
    }
}

fn retryable_conflict(message: &str) -> ConnectorErrorBody {
    ConnectorErrorBody {
        code: ConnectorErrorCode::Conflict,
        message: message.to_string(),
        retryable: true,
    }
}

pub(crate) fn integrity_error(error: RestoreError) -> ConnectorErrorBody {
    ConnectorErrorBody {
        code: ConnectorErrorCode::IntegrityFailure,
        message: error.to_string(),
        retryable: false,
    }
}

fn connector_error_as_restore_error(error: ConnectorErrorBody) -> RestoreError {
    RestoreError::Integrity(format!(
        "connector artifact export failed ({:?}): {}",
        error.code, error.message
    ))
}

#[cfg(test)]
mod audit_tests {
    use super::*;

    fn event(format_version: u32, event_id: char, operation: &str) -> ConnectorAuditEvent {
        ConnectorAuditEvent {
            format_version,
            event_id: std::iter::repeat_n(event_id, 64).collect(),
            observed_at_unix_nanoseconds: 1,
            account_id: "synthetic-account".to_string(),
            requester_id: "synthetic-requester".to_string(),
            request_id: format!("request-{event_id}"),
            operation: operation.to_string(),
            stage: ConnectorAuditStage::Request,
            conversation_id: None,
            destination: ConnectorDestination::Local,
            outcome: ConnectorAuditOutcome::Completed,
            returned_item_count: 1,
            released_body_byte_count: 0,
            request_body_byte_count: 0,
            draft_id: None,
            policy_decision_id: None,
            previous_event_sha256: None,
            event_sha256: String::new(),
        }
    }

    #[test]
    fn chains_new_events_after_a_reported_legacy_prefix_and_detects_tampering() {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = temporary.path().join("audit.ndjson");
        let mut legacy = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        serde_json::to_writer(&mut legacy, &event(1, 'a', "legacy")).unwrap();
        legacy.write_all(b"\n").unwrap();
        legacy.sync_all().unwrap();
        drop(legacy);

        append_owner_only_connector_event(&path, event(2, 'b', "current")).unwrap();
        let report = audit_connector_log(&path).unwrap();
        assert!(report.chain_verified);
        assert!(!report.fully_chained);
        assert_eq!(report.event_count, 2);
        assert_eq!(report.legacy_unchained_event_count, 1);
        assert_eq!(report.chained_event_count, 1);

        let mut bytes = fs::read(&path).unwrap();
        let offset = bytes
            .windows(b"current".len())
            .position(|window| window == b"current")
            .unwrap();
        bytes[offset] = b'C';
        fs::write(&path, bytes).unwrap();
        assert!(audit_connector_log(&path).is_err());
    }
}
