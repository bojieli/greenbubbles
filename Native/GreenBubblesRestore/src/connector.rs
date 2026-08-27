use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::archive::{ensure_private_directory, ensure_private_regular_file};
use crate::audit::verify_recorded_artifact_files;
use crate::replica::{
    get_replica_artifact, get_replica_changes, get_replica_conversation, get_replica_message,
    get_replica_participant, replica_conversation_references_artifact_in_range, replica_coverage,
    replica_restoration_report, replica_status, search_replica_cached_moments,
    search_replica_messages, ReplicaCachedMomentFilter, ReplicaCachedSurfaceAvailability,
    ReplicaCoverageView, ReplicaMessageFilter, ReplicaStatus,
};
use crate::tools::{
    load_tool_policy, minimize_cached_moment, minimize_message, released_body_bytes,
    released_cached_moment_body_bytes, CachedMomentsToolScope, ConversationToolScope,
    MinimizedCachedMoment, MinimizedMessage, ToolAuthorizationPolicy, ToolCapability,
    ToolDataDestination, ToolMessageField, MAX_SEARCH_QUERY_BYTES,
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
    ListConversations,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorMessagePage {
    pub account_id: String,
    pub source_fingerprint: String,
    pub messages: Vec<MinimizedMessage>,
    pub next_cursor: Option<String>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopedChangePage {
    pub account_id: String,
    pub items: Vec<crate::replica::ReplicaChange>,
    pub next_cursor: Option<String>,
    pub scope_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedContact {
    pub participant_id: String,
    pub display_name: String,
    pub local_profile_available: bool,
    pub enabled_conversation_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipientParticipantEvidence {
    pub participant_id: String,
    pub display_name: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedConversation {
    pub conversation_id: String,
    pub kind: ConversationKind,
    pub human_label: String,
    pub participant_count: usize,
    pub participants: Vec<RecipientParticipantEvidence>,
    pub owner_participant_id: Option<String>,
    pub entity_decode_state: EntityDecodeState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftReplyTarget {
    pub canonical_id: String,
    pub canonical_record_sha256: String,
    pub sender_id: Option<String>,
    pub created_at_unix: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
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
}

pub struct ConnectorService<'a> {
    replica_path: PathBuf,
    key: &'a ReplicaKey,
    policy: ToolAuthorizationPolicy,
    policy_sha256: String,
    audit_path: PathBuf,
    draft_directory: PathBuf,
    cached_moment_request_times: Mutex<VecDeque<u128>>,
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
        let account_id = status.account_id.ok_or_else(|| {
            RestoreError::Integrity("connector replica is not initialized".to_string())
        })?;
        if account_id != policy.account_id {
            return Err(RestoreError::Integrity(
                "connector policy belongs to a different replica account".to_string(),
            ));
        }
        for conversation_id in policy.conversation_scopes.keys() {
            if get_replica_conversation(replica_path, key, conversation_id)?.is_none() {
                return Err(RestoreError::Integrity(format!(
                    "policy conversation is absent from the replica: {conversation_id}"
                )));
            }
        }
        let audit_parent = audit_path
            .parent()
            .ok_or_else(|| RestoreError::UnsafePath("audit path has no parent".to_string()))?;
        ensure_private_directory(audit_parent)?;
        if audit_path.try_exists()? {
            ensure_private_regular_file(audit_path)?;
        }
        ensure_private_directory(draft_directory)?;
        Ok(Self {
            replica_path: replica_path.to_path_buf(),
            key,
            policy,
            policy_sha256,
            audit_path: audit_path.to_path_buf(),
            draft_directory: draft_directory.to_path_buf(),
            cached_moment_request_times: Mutex::new(VecDeque::new()),
        })
    }

    pub fn handle(&self, request: ConnectorRequest) -> ConnectorResponse {
        let request_id = request.request_id.clone();
        let result = self.dispatch(&request);
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

    fn dispatch(&self, request: &ConnectorRequest) -> Result<ConnectorResult, ConnectorErrorBody> {
        self.validate_request(request)?;
        match &request.operation {
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
            ConnectorOperation::ListConversations => self
                .list_conversations(request)
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
        }
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
        let cached_source_available = initialized
            && replica_coverage(&self.replica_path, self.key)?
                .cached_surfaces
                .is_some_and(|coverage| coverage.source_database_present);
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
        let moments = page
            .items
            .into_iter()
            .map(|moment| {
                minimize_cached_moment(
                    moment,
                    self.policy.maximum_message_summary_bytes,
                    &scope.fields,
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(integrity_error)?;
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
        })
    }

    fn list_conversations(
        &self,
        request: &ConnectorRequest,
    ) -> Result<ConnectorConversationList, ConnectorErrorBody> {
        let destination = ToolDataDestination::from(request.destination);
        let status = replica_status(&self.replica_path, self.key).map_err(integrity_error)?;
        let account_id = status
            .account_id
            .ok_or_else(|| unavailable("replicaUninitialized", "Replica is not initialized"))?;
        let mut conversations = Vec::new();
        for (conversation_id, scope) in &self.policy.conversation_scopes {
            if !scope
                .capabilities
                .contains(&ToolCapability::ListConversations)
                || (destination == ToolDataDestination::RemoteModel && !scope.allow_remote_model)
            {
                continue;
            }
            let conversation =
                get_replica_conversation(&self.replica_path, self.key, conversation_id)
                    .map_err(integrity_error)?
                    .ok_or_else(|| conflict("policy conversation disappeared from the replica"))?;
            let resolved = self.resolve_conversation(&conversation)?;
            conversations.push(ConnectorConversationView {
                conversation_id: conversation.conversation_id,
                kind: conversation.kind,
                participant_count: conversation.participant_ids.len(),
                entity_decode_state: conversation.entity_decode_state,
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
        let messages = page
            .items
            .into_iter()
            .map(|message| {
                minimize_message(
                    message,
                    self.policy.maximum_message_summary_bytes,
                    &scope.message_fields,
                )
            })
            .collect::<Vec<_>>();
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
                    )
                })
                .collect::<Vec<_>>();
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
            messages.extend(page.items.into_iter().map(|message| {
                minimize_message(
                    message,
                    self.policy.maximum_message_summary_bytes,
                    &scope.message_fields,
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
        let result = minimize_message(
            message,
            self.policy.maximum_message_summary_bytes,
            &scope.message_fields,
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
        let artifact = get_replica_artifact(&self.replica_path, self.key, artifact_id)
            .map_err(integrity_error)?
            .ok_or_else(|| not_found("artifact was not found"))?;
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
        })
    }

    fn resolve_contact(
        &self,
        request: &ConnectorRequest,
        participant_id: &str,
    ) -> Result<ResolvedContact, ConnectorErrorBody> {
        let participant = get_replica_participant(&self.replica_path, self.key, participant_id)
            .map_err(integrity_error)?
            .ok_or_else(|| not_found("participant was not found"))?;
        let destination = ToolDataDestination::from(request.destination);
        let enabled = participant
            .conversation_ids
            .iter()
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
            .collect::<Vec<_>>();
        if enabled.is_empty() {
            return Err(unauthorized(
                "participant has no enabled conversation for this destination",
            ));
        }
        let display_name = participant_display_name(&participant);
        let result = ResolvedContact {
            participant_id: participant.participant_id,
            display_name,
            local_profile_available: participant.local_profile_state
                == crate::LocalProfileState::Hydrated,
            enabled_conversation_ids: enabled,
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
        let conversation = get_replica_conversation(&self.replica_path, self.key, conversation_id)
            .map_err(integrity_error)?
            .ok_or_else(|| not_found("conversation was not found"))?;
        let result = self.resolve_conversation(&conversation)?;
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
        for participant_id in &conversation.participant_ids {
            let participant = get_replica_participant(&self.replica_path, self.key, participant_id)
                .map_err(integrity_error)?;
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
        Ok(ResolvedConversation {
            conversation_id: conversation.conversation_id.clone(),
            kind: conversation.kind,
            human_label,
            participant_count: conversation.participant_ids.len(),
            participants,
            owner_participant_id: conversation.owner_participant_id.clone(),
            entity_decode_state: conversation.entity_decode_state,
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
            byte_count,
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
        if draft.format_version != 1
            || draft.state != DraftState::DraftOnly
            || draft.account_id != self.policy.account_id
            || draft.api_version != CONNECTOR_API_VERSION
            || draft.connector_version != CONNECTOR_VERSION
            || draft.rendered_text_sha256
                != hex::encode(Sha256::digest(draft.rendered_text.as_bytes()))
        {
            return Err(conflict("draft binding evidence is invalid or stale"));
        }
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
            draft.created_at_unix_nanoseconds,
            draft.expires_at_unix_nanoseconds,
        );
        if expected != draft.draft_id {
            return Err(conflict(
                "draft immutable identity does not match its contents",
            ));
        }
        let attachment_ids = draft
            .attachments
            .iter()
            .map(|attachment| attachment.artifact_id.clone())
            .collect::<Vec<_>>();
        let expected_policy = self.policy_decision_identity(
            &draft.requester_id,
            &draft.conversation_id,
            draft
                .reply_target
                .as_ref()
                .map(|target| target.canonical_id.as_str()),
            &attachment_ids,
            &draft.source_fingerprint,
        );
        let status = replica_status(&self.replica_path, self.key).map_err(integrity_error)?;
        if expected_policy != draft.policy_decision_id
            || status.current_source_fingerprint.as_deref()
                != Some(draft.source_fingerprint.as_str())
        {
            return Err(conflict(
                "draft policy or replica checkpoint is stale; create a new draft",
            ));
        }
        Ok(())
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
            format_version: 1,
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
        };
        append_owner_only_json_line(&self.audit_path, &event).map_err(integrity_error)
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
        CONNECTOR_VERSION,
        CONNECTOR_API_VERSION,
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

fn append_owner_only_json_line(path: &Path, value: &impl Serialize) -> Result<(), RestoreError> {
    let mut file = OpenOptions::new()
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
        serde_json::to_writer(&mut file, value)?;
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
    })
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

fn invalid(message: &str) -> ConnectorErrorBody {
    ConnectorErrorBody {
        code: ConnectorErrorCode::InvalidRequest,
        message: message.to_string(),
        retryable: false,
    }
}

fn unauthorized(message: &str) -> ConnectorErrorBody {
    ConnectorErrorBody {
        code: ConnectorErrorCode::Unauthorized,
        message: message.to_string(),
        retryable: false,
    }
}

fn not_found(message: &str) -> ConnectorErrorBody {
    ConnectorErrorBody {
        code: ConnectorErrorCode::NotFound,
        message: message.to_string(),
        retryable: false,
    }
}

fn unavailable(code: &str, message: &str) -> ConnectorErrorBody {
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

fn integrity_error(error: RestoreError) -> ConnectorErrorBody {
    ConnectorErrorBody {
        code: ConnectorErrorCode::IntegrityFailure,
        message: error.to_string(),
        retryable: false,
    }
}
