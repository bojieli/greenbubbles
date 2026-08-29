use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::archive::{ensure_private_directory, ensure_private_regular_file};
use crate::connector::{
    ConnectorArtifactFile, ConnectorArtifactFileOrigin, ConnectorArtifactView,
    ConnectorCachedMomentPage, ConnectorCapabilities, ConnectorConversationList,
    ConnectorDestination, ConnectorErrorBody, ConnectorErrorCode, ConnectorMessagePage,
    ConnectorOperation, ConnectorRequest, ConnectorResult, ConnectorService, ConnectorStatus,
    RecipientParticipantEvidence, ResolvedContact, ResolvedConversation, ScopedChangePage,
    CONNECTOR_API_VERSION,
};
use crate::replica::{
    count_replica_messages_for_scopes, replica_status, ReplicaHealthState, ReplicaStatus,
};
use crate::tools::{
    load_tool_policy, MinimizedMessage, ToolCapability, ToolMessageField,
    ToolSourceDatabaseFreshness,
};
use crate::{
    ArtifactAvailability, ArtifactDecodeState, ArtifactKind, ArtifactRole,
    ClientBuildCompatibilityState, ConversationKind, EntityDecodeState, NoProgress, ProgressEvent,
    ProgressObserver, ProgressPhase, ProgressState, ProgressUnit, ReplicaKey, RestoreError,
};

pub const AI_QUERY_SCHEMA: &str = "greenbubbles.ai-query.v1";
pub const AI_CONTEXT_SCHEMA: &str = "greenbubbles.ai-context.v2";
pub const LEGACY_AI_CONTEXT_SCHEMA: &str = "greenbubbles.ai-context.v1";
const AI_QUERY_FORMAT_VERSION: u32 = 1;
const AI_CONTEXT_FORMAT_VERSION: u32 = 2;
const MAX_AI_QUERY_BYTES: u64 = 1024 * 1024;
const EXPORT_PAGE_SIZE: usize = 1_000;
const EXPORT_PROGRESS_RECORD_INTERVAL: u64 = 1_000;
const PHASE_RESOLUTION: u64 = 1_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiQueryRequest {
    pub format_version: u32,
    pub request_id: String,
    pub requester_id: String,
    #[serde(default)]
    pub destination: ConnectorDestination,
    pub operation: ConnectorOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiQueryResponse {
    pub format_version: u32,
    pub schema: String,
    pub api_version: String,
    pub request_id: String,
    pub ok: bool,
    pub context: AiContextHealth,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<AiQueryResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ConnectorErrorBody>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum AiQueryResult {
    Capabilities(Box<ConnectorCapabilities>),
    Status(Box<ConnectorStatus>),
    Coverage(AiContextHealth),
    Changes(ScopedChangePage),
    CachedMoments(ConnectorCachedMomentPage),
    Conversations(ConnectorConversationList),
    Messages(ConnectorMessagePage),
    Message(Option<MinimizedMessage>),
    Artifact(ConnectorArtifactView),
    Contact(ResolvedContact),
    Conversation(ResolvedConversation),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiContextHealth {
    pub account_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_participant_id: Option<String>,
    pub replica_id: String,
    pub source_fingerprint: String,
    pub checkpoint_revision: String,
    pub health: ReplicaHealthState,
    pub client_build_compatibility: Option<ClientBuildCompatibilityState>,
    pub archive_scope: Option<crate::RestorationArchiveScope>,
    pub authoritative_database_coverage: Option<bool>,
    pub total_database_count: Option<usize>,
    pub fresh_database_count: Option<usize>,
    pub unavailable_database_count: Option<usize>,
    pub preserved_stale_database_count: Option<usize>,
    pub conversation_count: u64,
    pub participant_count: u64,
    pub message_count: u64,
    pub artifact_count: u64,
    pub semantic_gap_count: Option<u64>,
    pub message_candidate_gap_count: Option<u64>,
    pub unavailable_artifact_count: Option<u64>,
    pub artifact_decode_gap_count: Option<u64>,
    pub entity_decode_gap_count: Option<u64>,
    pub checkpoint_age_seconds: Option<u64>,
    pub source_coverage_complete: bool,
    pub limitation_codes: Vec<String>,
    pub coverage_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiContextManifest {
    pub format_version: u32,
    pub schema: String,
    pub bundle_id: String,
    pub created_at_unix_nanoseconds: u128,
    pub destination: ConnectorDestination,
    pub requester_id: String,
    #[serde(rename = "policySHA256")]
    pub policy_sha256: String,
    pub policy_source_fingerprint: String,
    pub context: AiContextHealth,
    pub enabled_conversation_count: usize,
    pub exported_contact_count: u64,
    pub exported_message_count: u64,
    pub exported_artifact_count: u64,
    pub artifact_resolution_error_count: u64,
    /// Authorized conversations that could not be read from this checkpoint.
    #[serde(default)]
    pub omitted_conversation_count: u64,
    /// Matching message rows that were deliberately withheld because their
    /// record could not be decoded, validated, or paged safely.
    #[serde(default)]
    pub omitted_message_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
    pub export_complete: bool,
    pub files: Vec<AiContextFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiContextFile {
    pub role: String,
    pub relative_path: String,
    pub record_count: u64,
    pub byte_count: u64,
    #[serde(rename = "sha256")]
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiContextConversation {
    pub format_version: u32,
    pub conversation_id: String,
    pub human_label: String,
    pub kind: ConversationKind,
    pub participant_count: usize,
    pub participants: Vec<RecipientParticipantEvidence>,
    #[serde(rename = "groupOwnerParticipantId", alias = "ownerParticipantId")]
    pub group_owner_participant_id: Option<String>,
    pub entity_decode_state: EntityDecodeState,
    pub source_database_freshness: ToolSourceDatabaseFreshness,
    pub capabilities: BTreeSet<ToolCapability>,
    pub message_fields: BTreeSet<ToolMessageField>,
    pub not_before_unix: Option<i64>,
    pub not_after_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiContactConversationProfile {
    pub conversation_id: String,
    pub conversation_label: String,
    pub display_name: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiContextContact {
    pub format_version: u32,
    pub participant_id: String,
    pub display_name: String,
    pub local_profile_available: bool,
    pub source_database_freshness: ToolSourceDatabaseFreshness,
    pub enabled_conversation_ids: Vec<String>,
    pub conversation_profiles: Vec<AiContactConversationProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_error_code: Option<ConnectorErrorCode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiContextMessage {
    pub format_version: u32,
    pub conversation_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_display_name: Option<String>,
    #[serde(flatten)]
    pub message: MinimizedMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiContextArtifactFile {
    pub origin: ConnectorArtifactFileOrigin,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_relative_path: Option<String>,
    pub byte_count: u64,
    #[serde(rename = "sha256")]
    pub sha256: String,
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiContextArtifactDetail {
    pub kind: ArtifactKind,
    pub role: ArtifactRole,
    pub availability: ArtifactAvailability,
    pub decode_state: ArtifactDecodeState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<AiContextArtifactFile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decoded: Option<AiContextArtifactFile>,
    pub verification_state: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiContextArtifactError {
    pub code: ConnectorErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiContextArtifact {
    pub format_version: u32,
    pub artifact_id: String,
    pub conversation_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<AiContextArtifactDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AiContextArtifactError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AiContextAuditReport {
    pub format_version: u32,
    pub privacy_safe_summary: bool,
    pub schema: String,
    pub export_complete: bool,
    pub file_inventory_verified: bool,
    pub owner_only_permissions_verified: bool,
    pub file_digests_verified: bool,
    pub record_counts_verified: bool,
    pub schemas_verified: bool,
    pub unique_identities_verified: bool,
    pub references_verified: bool,
    pub source_freshness_verified: bool,
    pub conversation_count: u64,
    pub contact_count: u64,
    pub message_count: u64,
    pub artifact_count: u64,
    pub artifact_resolution_error_count: u64,
    pub omitted_artifact_reference_count: u64,
    pub omitted_relationship_reference_count: u64,
    pub preserved_stale_message_count: u64,
    pub omitted_conversation_count: u64,
    pub omitted_message_count: u64,
    pub content_complete: bool,
}

struct AiContextAuditProgress<'a> {
    observer: &'a dyn ProgressObserver,
    started: Instant,
    source_byte_count: u64,
    source_record_count: u64,
    conversation_record_count: u64,
    message_record_count: u64,
    completed_file_byte_count: u64,
    processed_conversation_count: u64,
    processed_message_count: u64,
}

impl<'a> AiContextAuditProgress<'a> {
    fn new(observer: &'a dyn ProgressObserver, files: &BTreeMap<&str, &AiContextFile>) -> Self {
        Self {
            observer,
            started: Instant::now(),
            source_byte_count: files
                .values()
                .map(|file| file.byte_count)
                .fold(0_u64, u64::saturating_add),
            source_record_count: files
                .values()
                .map(|file| file.record_count)
                .fold(0_u64, u64::saturating_add),
            conversation_record_count: files["conversations"].record_count,
            message_record_count: files["messages"].record_count,
            completed_file_byte_count: 0,
            processed_conversation_count: 0,
            processed_message_count: 0,
        }
    }

    fn plan(&self) {
        self.observe(
            ProgressState::Planned,
            "planAiContextAudit",
            ProgressUnit::Bytes,
            0,
            self.source_byte_count,
            0,
            None,
            None,
            None,
            None,
        );
        self.observe(
            ProgressState::Started,
            "auditAiContext",
            ProgressUnit::Bytes,
            0,
            self.source_byte_count,
            0,
            None,
            None,
            None,
            None,
        );
    }

    fn begin_file(&self, index: usize, operation: &str, file: &AiContextFile) {
        self.observe(
            ProgressState::Started,
            operation,
            ProgressUnit::Records,
            0,
            file.record_count,
            self.phase_position(0),
            Some(index),
            Some(&file.relative_path),
            Some(0),
            Some(file.byte_count),
        );
    }

    fn advance_file(
        &mut self,
        index: usize,
        role: &str,
        operation: &str,
        file: &AiContextFile,
        completed_records: u64,
        completed_bytes: u64,
    ) {
        if role == "conversations" {
            self.processed_conversation_count = completed_records;
        } else if role == "messages" {
            self.processed_message_count = completed_records;
        }
        if completed_records != 1 && !completed_records.is_multiple_of(1_000) {
            return;
        }
        self.observe(
            ProgressState::Advanced,
            operation,
            ProgressUnit::Records,
            completed_records,
            file.record_count,
            self.phase_position(completed_bytes),
            Some(index),
            Some(&file.relative_path),
            Some(completed_bytes),
            Some(file.byte_count),
        );
    }

    fn complete_file(&mut self, index: usize, role: &str, operation: &str, file: &AiContextFile) {
        if role == "conversations" {
            self.processed_conversation_count = file.record_count;
        } else if role == "messages" {
            self.processed_message_count = file.record_count;
        }
        self.observe(
            ProgressState::Advanced,
            operation,
            ProgressUnit::Records,
            file.record_count,
            file.record_count,
            self.phase_position(file.byte_count),
            Some(index),
            Some(&file.relative_path),
            Some(file.byte_count),
            Some(file.byte_count),
        );
        self.completed_file_byte_count = self
            .completed_file_byte_count
            .saturating_add(file.byte_count);
    }

    fn finish(&self) {
        self.observe(
            ProgressState::Completed,
            "finalizeAiContextAudit",
            ProgressUnit::Records,
            self.source_record_count,
            self.source_record_count,
            PHASE_RESOLUTION,
            None,
            None,
            None,
            None,
        );
    }

    fn phase_position(&self, current_file_bytes: u64) -> u64 {
        progress_fraction(
            self.completed_file_byte_count
                .saturating_add(current_file_bytes),
            self.source_byte_count,
            PHASE_RESOLUTION,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn observe(
        &self,
        state: ProgressState,
        operation: &str,
        unit: ProgressUnit,
        completed: u64,
        total: u64,
        phase_completed: u64,
        file_index: Option<usize>,
        logical_path: Option<&str>,
        file_completed_byte_count: Option<u64>,
        file_byte_count: Option<u64>,
    ) {
        let mut event = ProgressEvent::new(
            ProgressPhase::ContextAudit,
            state,
            operation,
            unit,
            completed,
            total,
            phase_completed.min(PHASE_RESOLUTION),
            PHASE_RESOLUTION,
        );
        event.file_index = file_index;
        event.file_count = Some(4);
        event.logical_path = logical_path.map(str::to_string);
        event.file_completed_byte_count = file_completed_byte_count;
        event.file_byte_count = file_byte_count;
        event.source_byte_count = Some(self.source_byte_count);
        event.source_record_count = Some(self.source_record_count);
        event.conversation_record_count = Some(self.conversation_record_count);
        event.message_record_count = Some(self.message_record_count);
        event.processed_conversation_count = Some(self.processed_conversation_count);
        event.processed_message_count = Some(self.processed_message_count);
        event.elapsed_milliseconds =
            Some(self.started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        self.observer.observe(event);
    }
}

#[derive(Default)]
struct ContactAccumulator {
    profiles: Vec<AiContactConversationProfile>,
}

pub fn load_ai_query_request(path: &Path) -> Result<AiQueryRequest, RestoreError> {
    ensure_private_regular_file(path)?;
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_AI_QUERY_BYTES {
        return Err(RestoreError::Integrity(
            "AI query request exceeds its byte limit".to_string(),
        ));
    }
    let request: AiQueryRequest = serde_json::from_slice(&fs::read(path)?)?;
    validate_ai_query(&request)?;
    Ok(request)
}

pub fn query_ai_context(
    replica_path: &Path,
    key: &ReplicaKey,
    policy_path: &Path,
    audit_path: &Path,
    request: AiQueryRequest,
) -> Result<AiQueryResponse, RestoreError> {
    validate_ai_query(&request)?;
    let audit_parent = audit_path
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath("AI query audit path has no parent".to_string()))?;
    ensure_private_directory(audit_parent)?;
    let drafts = tempfile::Builder::new()
        .prefix(".greenbubbles-ai-query-")
        .tempdir_in(audit_parent)?;
    fs::set_permissions(drafts.path(), fs::Permissions::from_mode(0o700))?;
    let service =
        ConnectorService::open(replica_path, key, policy_path, audit_path, drafts.path())?;
    let before = replica_status(replica_path, key)?;
    service.prepare_external_checkpoint_guard()?;
    let connector_response = service.handle_with_external_checkpoint_guard(ConnectorRequest {
        api_version: CONNECTOR_API_VERSION.to_string(),
        request_id: request.request_id,
        requester_id: request.requester_id,
        destination: request.destination,
        operation: request.operation,
    });
    let after = replica_status(replica_path, key)?;
    require_same_checkpoint(&before, &after, "AI query")?;
    let context = context_health(&after)?;
    let result = connector_response
        .result
        .map(|result| sanitize_query_result(result, &context))
        .transpose()?;
    Ok(AiQueryResponse {
        format_version: AI_QUERY_FORMAT_VERSION,
        schema: AI_QUERY_SCHEMA.to_string(),
        api_version: connector_response.api_version,
        request_id: connector_response.request_id,
        ok: connector_response.ok,
        context,
        result,
        error: connector_response.error.map(sanitize_query_error),
    })
}

#[allow(clippy::too_many_arguments)]
pub fn export_ai_context(
    replica_path: &Path,
    key: &ReplicaKey,
    policy_path: &Path,
    audit_path: &Path,
    output_directory: &Path,
    requester_id: &str,
    destination: ConnectorDestination,
    progress: &dyn ProgressObserver,
) -> Result<AiContextManifest, RestoreError> {
    if requester_id.is_empty() || requester_id.len() > 256 {
        return Err(RestoreError::Integrity(
            "AI export requester ID must be between 1 and 256 bytes".to_string(),
        ));
    }
    ensure_private_regular_file(policy_path)?;
    let output_parent = output_directory.parent().unwrap_or_else(|| Path::new("."));
    ensure_private_directory(output_parent)?;
    if output_directory.try_exists()? {
        return Err(RestoreError::Integrity(
            "AI context output directory already exists".to_string(),
        ));
    }
    let audit_parent = audit_path.parent().ok_or_else(|| {
        RestoreError::UnsafePath("AI export audit path has no parent".to_string())
    })?;
    ensure_private_directory(audit_parent)?;

    let started = Instant::now();
    observe_export(
        progress,
        ProgressState::Planned,
        "planAiContextExport",
        0,
        0,
        0,
        None,
        4,
        started,
    );
    let scratch_drafts = tempfile::Builder::new()
        .prefix(".greenbubbles-ai-export-requests-")
        .tempdir_in(audit_parent)?;
    fs::set_permissions(scratch_drafts.path(), fs::Permissions::from_mode(0o700))?;
    let staging = tempfile::Builder::new()
        .prefix(".greenbubbles-ai-context-")
        .tempdir_in(output_parent)?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))?;
    let service = ConnectorService::open(
        replica_path,
        key,
        policy_path,
        audit_path,
        scratch_drafts.path(),
    )?;
    let mut reader = AiConnectorReader::new(&service, requester_id, destination);
    let start_status = expect_status(reader.call(ConnectorOperation::Status)?)?;
    service.prepare_external_checkpoint_guard()?;
    let start_health = context_health(&start_status.replica)?;
    require_bound_account_holder(&start_health)?;
    let list = list_all_conversations(&mut reader)?;
    let policy = load_tool_policy(policy_path)?;
    let policy_sha256 = hex::encode(Sha256::digest(fs::read(policy_path)?));

    let message_scopes = policy
        .conversation_scopes
        .iter()
        .filter(|(_, scope)| {
            scope
                .capabilities
                .contains(&ToolCapability::ReadRecentMessages)
                && scope
                    .capabilities
                    .contains(&ToolCapability::ListConversations)
                && (destination == ConnectorDestination::Local || scope.allow_remote_model)
        })
        .map(|(conversation_id, scope)| {
            (
                conversation_id.clone(),
                scope.not_before_unix,
                scope.not_after_unix,
            )
        })
        .collect::<Vec<_>>();
    let expected_message_count =
        count_replica_messages_for_scopes(replica_path, key, &message_scopes)?;

    let mut conversation_writer =
        NdjsonWriter::create(staging.path(), "conversations", "conversations.jsonl")?;
    let mut contact_writer = NdjsonWriter::create(staging.path(), "contacts", "contacts.jsonl")?;
    let mut message_writer = NdjsonWriter::create(staging.path(), "messages", "messages.jsonl")?;
    let mut artifact_writer = NdjsonWriter::create(staging.path(), "artifacts", "artifacts.jsonl")?;

    let mut limitation_codes = list
        .limitation_codes
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let omitted_conversation_count = list.omitted_conversation_count;
    let mut resolved_conversations = BTreeMap::<String, ResolvedConversation>::new();
    let mut contacts = BTreeMap::<String, ContactAccumulator>::new();
    for listed in &list.conversations {
        let response = reader.raw_call(ConnectorOperation::ResolveConversation {
            conversation_id: listed.conversation_id.clone(),
        });
        let mut resolved = match response.result {
            Some(ConnectorResult::Conversation(resolved)) if response.ok => resolved,
            _ => ResolvedConversation {
                conversation_id: listed.conversation_id.clone(),
                kind: listed.kind,
                human_label: listed.human_label.clone(),
                participant_count: listed.participant_count,
                participants: Vec::new(),
                owner_participant_id: None,
                entity_decode_state: listed.entity_decode_state,
                source_database_freshness: listed.source_database_freshness,
                limitation_codes: vec!["conversationResolutionUnavailable".to_string()],
            },
        };
        mark_self_participant(&mut resolved, &start_health);
        conversation_writer.write(&AiContextConversation {
            format_version: AI_CONTEXT_FORMAT_VERSION,
            conversation_id: resolved.conversation_id.clone(),
            human_label: resolved.human_label.clone(),
            kind: resolved.kind,
            participant_count: resolved.participant_count,
            participants: resolved.participants.clone(),
            group_owner_participant_id: resolved.owner_participant_id.clone(),
            entity_decode_state: resolved.entity_decode_state,
            source_database_freshness: resolved.source_database_freshness,
            capabilities: listed.capabilities.clone(),
            message_fields: listed.message_fields.clone(),
            not_before_unix: listed.not_before_unix,
            not_after_unix: listed.not_after_unix,
            limitation_codes: resolved.limitation_codes.clone(),
        })?;
        limitation_codes.extend(resolved.limitation_codes.iter().cloned());
        for participant in &resolved.participants {
            contacts
                .entry(participant.participant_id.clone())
                .or_default()
                .profiles
                .push(AiContactConversationProfile {
                    conversation_id: resolved.conversation_id.clone(),
                    conversation_label: resolved.human_label.clone(),
                    display_name: participant.display_name.clone(),
                    role: participant.role.clone(),
                });
        }
        resolved_conversations.insert(resolved.conversation_id.clone(), resolved);
    }
    observe_export(
        progress,
        ProgressState::Advanced,
        "exportConversations",
        list.conversations.len() as u64,
        list.conversations.len() as u64,
        100_000,
        Some(1),
        4,
        started,
    );

    let mut exported_contact_ids = BTreeSet::new();
    for (participant_id, mut accumulated) in contacts {
        accumulated.profiles.sort_by(|left, right| {
            (&left.conversation_id, &left.role, &left.display_name).cmp(&(
                &right.conversation_id,
                &right.role,
                &right.display_name,
            ))
        });
        let response = reader.raw_call(ConnectorOperation::ResolveContact {
            participant_id: participant_id.clone(),
        });
        let (
            display_name,
            local_profile_available,
            source_database_freshness,
            enabled_conversation_ids,
            resolution_error_code,
            contact_limitation_codes,
        ) = match response.result {
            Some(ConnectorResult::Contact(contact)) if response.ok => (
                contact.display_name,
                contact.local_profile_available,
                contact.source_database_freshness,
                contact.enabled_conversation_ids,
                None,
                contact.limitation_codes,
            ),
            _ => (
                accumulated
                    .profiles
                    .first()
                    .map(|profile| profile.display_name.clone())
                    .unwrap_or_else(|| short_identifier(&participant_id)),
                false,
                ToolSourceDatabaseFreshness::Derived,
                accumulated
                    .profiles
                    .iter()
                    .map(|profile| profile.conversation_id.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                Some(
                    response
                        .error
                        .map_or(ConnectorErrorCode::IntegrityFailure, |error| error.code),
                ),
                vec!["contactResolutionUnavailable".to_string()],
            ),
        };
        limitation_codes.extend(contact_limitation_codes.iter().cloned());
        let is_self = start_health.self_participant_id.as_deref() == Some(participant_id.as_str());
        exported_contact_ids.insert(participant_id.clone());
        contact_writer.write(&AiContextContact {
            format_version: AI_CONTEXT_FORMAT_VERSION,
            participant_id,
            display_name: if is_self {
                "You".to_string()
            } else {
                display_name
            },
            local_profile_available,
            source_database_freshness,
            enabled_conversation_ids,
            conversation_profiles: accumulated.profiles,
            resolution_error_code,
            limitation_codes: contact_limitation_codes,
        })?;
    }
    observe_export(
        progress,
        ProgressState::Advanced,
        "exportContacts",
        contact_writer.record_count,
        contact_writer.record_count,
        100_000,
        Some(2),
        4,
        started,
    );

    let mut artifact_conversations = BTreeMap::<String, BTreeSet<String>>::new();
    let mut exported_message_count = 0_u64;
    let mut observed_omitted_message_count = 0_u64;
    for conversation in &list.conversations {
        if !conversation
            .capabilities
            .contains(&ToolCapability::ReadRecentMessages)
        {
            continue;
        }
        let resolved = resolved_conversations
            .get(&conversation.conversation_id)
            .ok_or_else(|| {
                RestoreError::Integrity(
                    "AI context export lost a resolved conversation".to_string(),
                )
            })?;
        let sender_names = resolved
            .participants
            .iter()
            .map(|participant| {
                (
                    participant.participant_id.as_str(),
                    participant.display_name.as_str(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut cursor = None;
        loop {
            let response = reader.raw_call(ConnectorOperation::GetMessages {
                conversation_id: conversation.conversation_id.clone(),
                cursor: cursor.clone(),
                limit: Some(EXPORT_PAGE_SIZE),
            });
            let page = match response.result {
                Some(ConnectorResult::Messages(page)) if response.ok => page,
                _ => {
                    limitation_codes.insert("unavailableConversationMessagesOmitted".to_string());
                    break;
                }
            };
            if page.source_fingerprint != start_health.source_fingerprint {
                return Err(RestoreError::Integrity(
                    "replica changed while the AI context bundle was being exported".to_string(),
                ));
            }
            observed_omitted_message_count =
                observed_omitted_message_count.saturating_add(page.omitted_message_count);
            limitation_codes.extend(page.limitation_codes.iter().cloned());
            for mut message in page.messages {
                normalize_message_identity(&mut message, &start_health);
                let sender_display_name = message.sender_id.as_deref().and_then(|identifier| {
                    if start_health.self_participant_id.as_deref() == Some(identifier) {
                        Some("You".to_string())
                    } else {
                        sender_names.get(identifier).copied().map(str::to_string)
                    }
                });
                if let Some(sender_id) = message.sender_id.as_ref() {
                    if !exported_contact_ids.contains(sender_id) {
                        let is_self =
                            start_health.self_participant_id.as_deref() == Some(sender_id.as_str());
                        let display_name = if is_self {
                            "You".to_string()
                        } else {
                            sender_display_name
                                .clone()
                                .unwrap_or_else(|| short_identifier(sender_id))
                        };
                        contact_writer.write(&AiContextContact {
                            format_version: AI_CONTEXT_FORMAT_VERSION,
                            participant_id: sender_id.clone(),
                            display_name: display_name.clone(),
                            local_profile_available: false,
                            source_database_freshness: ToolSourceDatabaseFreshness::Derived,
                            enabled_conversation_ids: vec![conversation.conversation_id.clone()],
                            conversation_profiles: vec![AiContactConversationProfile {
                                conversation_id: conversation.conversation_id.clone(),
                                conversation_label: resolved.human_label.clone(),
                                display_name,
                                role: "MessageSender".to_string(),
                            }],
                            resolution_error_code: Some(ConnectorErrorCode::NotFound),
                            limitation_codes: vec!["missingSenderProfileSynthesized".to_string()],
                        })?;
                        exported_contact_ids.insert(sender_id.clone());
                        limitation_codes.insert("missingSenderProfileSynthesized".to_string());
                    }
                }
                for artifact in &message.artifact_references {
                    artifact_conversations
                        .entry(artifact.artifact_id.clone())
                        .or_default()
                        .insert(conversation.conversation_id.clone());
                }
                message_writer.write(&AiContextMessage {
                    format_version: AI_CONTEXT_FORMAT_VERSION,
                    conversation_label: resolved.human_label.clone(),
                    sender_display_name,
                    message,
                })?;
                exported_message_count = exported_message_count.saturating_add(1);
            }
            let phase_completed = 100_000_u64.saturating_add(progress_fraction(
                exported_message_count,
                expected_message_count,
                800_000,
            ));
            observe_export(
                progress,
                ProgressState::Advanced,
                "exportMessages",
                exported_message_count,
                expected_message_count,
                phase_completed,
                Some(3),
                4,
                started,
            );
            let Some(next_cursor) = page.next_cursor else {
                break;
            };
            if cursor.as_deref() == Some(next_cursor.as_str()) {
                limitation_codes.insert("nonAdvancingMessageCursorOmitted".to_string());
                break;
            }
            cursor = Some(next_cursor);
        }
    }
    let omitted_message_count = expected_message_count
        .saturating_sub(exported_message_count)
        .max(observed_omitted_message_count);
    if exported_message_count != expected_message_count {
        limitation_codes.insert("messageCountMismatchOmitted".to_string());
    }

    let artifact_count = artifact_conversations.len() as u64;
    let mut artifact_error_count = 0_u64;
    let mut processed_artifact_count = 0_u64;
    observe_export(
        progress,
        ProgressState::Advanced,
        "exportArtifacts",
        0,
        artifact_count,
        900_000,
        Some(4),
        4,
        started,
    );
    let artifact_summary = reader.export_artifacts(
        &artifact_conversations,
        |artifact_id, conversation_ids, result| {
            let (detail, error) = match result {
                Ok(artifact) => {
                    let detail = sanitize_artifact(artifact);
                    limitation_codes.extend(detail.limitation_codes.iter().cloned());
                    (Some(detail), None)
                }
                Err(error) => {
                    artifact_error_count = artifact_error_count.saturating_add(1);
                    limitation_codes.insert("artifactResolutionUnavailable".to_string());
                    (
                        None,
                        Some(AiContextArtifactError {
                            code: error.code,
                            message: safe_query_error_message(error.code).to_string(),
                            retryable: error.retryable,
                        }),
                    )
                }
            };
            artifact_writer.write(&AiContextArtifact {
                format_version: AI_CONTEXT_FORMAT_VERSION,
                artifact_id: artifact_id.to_string(),
                conversation_ids: conversation_ids.iter().cloned().collect(),
                detail,
                error,
            })?;
            processed_artifact_count = processed_artifact_count.saturating_add(1);
            if processed_artifact_count == artifact_count
                || processed_artifact_count.is_multiple_of(EXPORT_PROGRESS_RECORD_INTERVAL)
            {
                observe_export(
                    progress,
                    ProgressState::Advanced,
                    "exportArtifacts",
                    processed_artifact_count,
                    artifact_count,
                    900_000_u64.saturating_add(progress_fraction(
                        processed_artifact_count,
                        artifact_count,
                        90_000,
                    )),
                    Some(4),
                    4,
                    started,
                );
            }
            Ok(())
        },
    )?;
    if artifact_summary.checkpoint_revision != start_health.checkpoint_revision
        || artifact_summary.requested_count != artifact_count
        || artifact_summary
            .resolved_count
            .saturating_add(artifact_summary.error_count)
            != artifact_count
        || artifact_summary.error_count != artifact_error_count
        || processed_artifact_count != artifact_count
    {
        return Err(RestoreError::Integrity(
            "AI artifact export changed checkpoint or lost batch accounting".to_string(),
        ));
    }

    let files = vec![
        conversation_writer.finish()?,
        contact_writer.finish()?,
        message_writer.finish()?,
        artifact_writer.finish()?,
    ];
    let final_status = replica_status(replica_path, key)?;
    require_same_checkpoint(&start_status.replica, &final_status, "AI context export")?;
    let final_health = context_health(&final_status)?;
    require_bound_account_holder(&final_health)?;
    let bundle_id = context_bundle_id(
        AI_CONTEXT_FORMAT_VERSION,
        &final_health,
        &policy_sha256,
        destination,
        &policy.created_from_source_fingerprint,
    )?;
    let manifest = AiContextManifest {
        format_version: AI_CONTEXT_FORMAT_VERSION,
        schema: AI_CONTEXT_SCHEMA.to_string(),
        bundle_id,
        created_at_unix_nanoseconds: unix_nanoseconds()?,
        destination,
        requester_id: requester_id.to_string(),
        policy_sha256,
        policy_source_fingerprint: policy.created_from_source_fingerprint,
        context: final_health,
        enabled_conversation_count: list
            .conversations
            .len()
            .saturating_add(usize::try_from(omitted_conversation_count).unwrap_or(usize::MAX)),
        exported_contact_count: files[1].record_count,
        exported_message_count,
        exported_artifact_count: files[3].record_count,
        artifact_resolution_error_count: artifact_error_count,
        omitted_conversation_count,
        omitted_message_count,
        limitation_codes: limitation_codes.into_iter().collect(),
        export_complete: true,
        files,
    };
    write_private_json(&staging.path().join("manifest.json"), &manifest)?;
    File::open(staging.path())?.sync_all()?;
    if output_directory.try_exists()? {
        return Err(RestoreError::Integrity(
            "AI context output directory appeared during export".to_string(),
        ));
    }
    fs::rename(staging.path(), output_directory)?;
    File::open(output_parent)?.sync_all()?;
    observe_export(
        progress,
        ProgressState::Completed,
        "finalizeAiContextExport",
        exported_message_count,
        expected_message_count,
        PHASE_RESOLUTION,
        None,
        4,
        started,
    );
    Ok(manifest)
}

pub fn audit_ai_context(bundle_directory: &Path) -> Result<AiContextAuditReport, RestoreError> {
    audit_ai_context_with_progress(bundle_directory, &NoProgress)
}

pub fn audit_ai_context_with_progress(
    bundle_directory: &Path,
    observer: &dyn ProgressObserver,
) -> Result<AiContextAuditReport, RestoreError> {
    ensure_private_directory(bundle_directory)?;
    let expected_entries = BTreeSet::from([
        "manifest.json".to_string(),
        "conversations.jsonl".to_string(),
        "contacts.jsonl".to_string(),
        "messages.jsonl".to_string(),
        "artifacts.jsonl".to_string(),
    ]);
    let mut observed_entries = BTreeSet::new();
    for entry in fs::read_dir(bundle_directory)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| {
            RestoreError::Integrity("AI context bundle contains a non-UTF-8 entry".to_string())
        })?;
        if !observed_entries.insert(name) {
            return Err(RestoreError::Integrity(
                "AI context bundle repeats a filesystem entry".to_string(),
            ));
        }
    }
    if observed_entries != expected_entries {
        return Err(RestoreError::Integrity(
            "AI context bundle file inventory is incomplete or contains an unexpected entry"
                .to_string(),
        ));
    }

    let manifest_path = bundle_directory.join("manifest.json");
    ensure_private_regular_file(&manifest_path)?;
    let manifest_bytes = read_bounded_file(&manifest_path, 4 * 1024 * 1024)?;
    let manifest: AiContextManifest = serde_json::from_slice(&manifest_bytes)?;
    validate_ai_manifest(&manifest)?;
    let files = manifest
        .files
        .iter()
        .map(|file| (file.role.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    let expected_roles = BTreeMap::from([
        ("conversations", "conversations.jsonl"),
        ("contacts", "contacts.jsonl"),
        ("messages", "messages.jsonl"),
        ("artifacts", "artifacts.jsonl"),
    ]);
    if files.len() != expected_roles.len()
        || expected_roles.iter().any(|(role, path)| {
            files
                .get(role)
                .is_none_or(|file| file.relative_path != *path)
        })
    {
        return Err(RestoreError::Integrity(
            "AI context manifest file roles or paths are invalid".to_string(),
        ));
    }
    let mut progress = AiContextAuditProgress::new(observer, &files);
    progress.plan();

    let mut conversation_ids = BTreeSet::new();
    let mut required_contact_ids = BTreeSet::new();
    let conversation_file = files["conversations"];
    progress.begin_file(1, "auditAiContextConversations", conversation_file);
    audit_ndjson_values_with_progress(
        bundle_directory,
        conversation_file,
        |value| {
            let object = value.as_object().ok_or_else(|| {
                RestoreError::Integrity(
                    "AI context conversation record is not an object".to_string(),
                )
            })?;
            let (required_owner_field, forbidden_owner_field) =
                if manifest.format_version == AI_CONTEXT_FORMAT_VERSION {
                    ("groupOwnerParticipantId", "ownerParticipantId")
                } else {
                    ("ownerParticipantId", "groupOwnerParticipantId")
                };
            if !object.contains_key(required_owner_field)
                || object.contains_key(forbidden_owner_field)
            {
                return Err(RestoreError::Integrity(
                    "AI context conversation uses the wrong group-owner field".to_string(),
                ));
            }
            let conversation: AiContextConversation = serde_json::from_value(value)?;
            require_ai_record_format(
                conversation.format_version,
                manifest.format_version,
                "conversation",
            )?;
            require_nonempty(&conversation.conversation_id, "conversation ID")?;
            require_nonempty(&conversation.human_label, "conversation label")?;
            if !conversation_ids.insert(conversation.conversation_id.clone()) {
                return Err(RestoreError::Integrity(
                    "AI context conversations repeat an identity".to_string(),
                ));
            }
            if (conversation.participant_count != conversation.participants.len()
                && !conversation
                    .limitation_codes
                    .iter()
                    .any(|code| code == "conversationResolutionUnavailable"))
                || conversation
                    .not_before_unix
                    .zip(conversation.not_after_unix)
                    .is_some_and(|(start, end)| start > end)
            {
                return Err(RestoreError::Integrity(
                    "AI context conversation metadata is internally inconsistent".to_string(),
                ));
            }
            let mut participants = BTreeSet::new();
            for participant in &conversation.participants {
                require_nonempty(&participant.participant_id, "participant ID")?;
                require_nonempty(&participant.display_name, "participant display name")?;
                require_nonempty(&participant.role, "participant role")?;
                if manifest.format_version == AI_CONTEXT_FORMAT_VERSION
                    && manifest.context.self_participant_id.as_deref()
                        == Some(participant.participant_id.as_str())
                    && participant.display_name != "You"
                {
                    return Err(RestoreError::Integrity(
                        "AI context self participant is not labelled as You".to_string(),
                    ));
                }
                if !participants.insert(participant.participant_id.clone()) {
                    return Err(RestoreError::Integrity(
                        "AI context conversation repeats a participant".to_string(),
                    ));
                }
                required_contact_ids.insert(participant.participant_id.clone());
            }
            if conversation
                .group_owner_participant_id
                .as_ref()
                .is_some_and(|owner| !participants.contains(owner))
            {
                return Err(RestoreError::Integrity(
                    "AI context group owner is not a conversation participant".to_string(),
                ));
            }
            validate_entity_freshness(conversation.source_database_freshness, &manifest.context)
        },
        |records, bytes| {
            progress.advance_file(
                1,
                "conversations",
                "auditAiContextConversations",
                conversation_file,
                records,
                bytes,
            );
        },
    )?;
    progress.complete_file(
        1,
        "conversations",
        "auditAiContextConversations",
        conversation_file,
    );

    let mut contact_ids = BTreeSet::new();
    let contact_file = files["contacts"];
    progress.begin_file(2, "auditAiContextContacts", contact_file);
    audit_ndjson_with_progress::<AiContextContact, _, _>(
        bundle_directory,
        contact_file,
        |contact| {
            require_ai_record_format(contact.format_version, manifest.format_version, "contact")?;
            require_nonempty(&contact.participant_id, "contact ID")?;
            require_nonempty(&contact.display_name, "contact display name")?;
            if manifest.format_version == AI_CONTEXT_FORMAT_VERSION
                && manifest.context.self_participant_id.as_deref()
                    == Some(contact.participant_id.as_str())
                && contact.display_name != "You"
            {
                return Err(RestoreError::Integrity(
                    "AI context self contact is not labelled as You".to_string(),
                ));
            }
            if !contact_ids.insert(contact.participant_id.clone()) {
                return Err(RestoreError::Integrity(
                    "AI context contacts repeat an identity".to_string(),
                ));
            }
            if contact
                .limitation_codes
                .iter()
                .any(|code| !manifest.limitation_codes.contains(code))
            {
                return Err(RestoreError::Integrity(
                    "AI context contact limitations lack manifest evidence".to_string(),
                ));
            }
            validate_entity_freshness(contact.source_database_freshness, &manifest.context)?;
            let enabled = contact
                .enabled_conversation_ids
                .iter()
                .collect::<BTreeSet<_>>();
            if enabled.len() != contact.enabled_conversation_ids.len()
                || enabled
                    .iter()
                    .any(|conversation| !conversation_ids.contains(*conversation))
            {
                return Err(RestoreError::Integrity(
                    "AI context contact references an absent or repeated conversation".to_string(),
                ));
            }
            let mut profiles = BTreeSet::new();
            for profile in &contact.conversation_profiles {
                require_nonempty(&profile.display_name, "contact profile display name")?;
                require_nonempty(&profile.role, "contact profile role")?;
                if manifest.format_version == AI_CONTEXT_FORMAT_VERSION
                    && manifest.context.self_participant_id.as_deref()
                        == Some(contact.participant_id.as_str())
                    && profile.display_name != "You"
                {
                    return Err(RestoreError::Integrity(
                        "AI context self contact profile is not labelled as You".to_string(),
                    ));
                }
                if !conversation_ids.contains(&profile.conversation_id)
                    || !profiles.insert((profile.conversation_id.clone(), profile.role.clone()))
                {
                    return Err(RestoreError::Integrity(
                        "AI context contact profile is absent or repeated".to_string(),
                    ));
                }
            }
            Ok(())
        },
        |records, bytes| {
            progress.advance_file(
                2,
                "contacts",
                "auditAiContextContacts",
                contact_file,
                records,
                bytes,
            );
        },
    )?;
    progress.complete_file(2, "contacts", "auditAiContextContacts", contact_file);
    let mut artifact_ids = BTreeSet::new();
    let mut artifact_error_count = 0_u64;
    let artifact_file = files["artifacts"];
    progress.begin_file(3, "auditAiContextArtifacts", artifact_file);
    audit_ndjson_with_progress::<AiContextArtifact, _, _>(
        bundle_directory,
        artifact_file,
        |artifact| {
            require_ai_record_format(artifact.format_version, manifest.format_version, "artifact")?;
            require_nonempty(&artifact.artifact_id, "artifact ID")?;
            if !artifact_ids.insert(artifact.artifact_id.clone()) {
                return Err(RestoreError::Integrity(
                    "AI context artifacts repeat an identity".to_string(),
                ));
            }
            let conversations = artifact.conversation_ids.iter().collect::<BTreeSet<_>>();
            if conversations.is_empty()
                || conversations.len() != artifact.conversation_ids.len()
                || conversations
                    .iter()
                    .any(|conversation| !conversation_ids.contains(*conversation))
            {
                return Err(RestoreError::Integrity(
                    "AI context artifact references an absent or repeated conversation".to_string(),
                ));
            }
            match (&artifact.detail, &artifact.error) {
                (Some(detail), None) => {
                    validate_ai_artifact_detail(detail)?;
                    if detail
                        .limitation_codes
                        .iter()
                        .any(|code| !manifest.limitation_codes.contains(code))
                    {
                        return Err(RestoreError::Integrity(
                            "AI context artifact limitations lack manifest evidence".to_string(),
                        ));
                    }
                }
                (None, Some(error)) => {
                    artifact_error_count = artifact_error_count.saturating_add(1);
                    require_nonempty(&error.message, "artifact error message")?;
                    if error.message != safe_query_error_message(error.code) {
                        return Err(RestoreError::Integrity(
                            "AI context artifact contains a non-canonical error message"
                                .to_string(),
                        ));
                    }
                }
                _ => {
                    return Err(RestoreError::Integrity(
                        "AI context artifact must contain exactly one detail or error result"
                            .to_string(),
                    ));
                }
            }
            Ok(())
        },
        |records, bytes| {
            progress.advance_file(
                3,
                "artifacts",
                "auditAiContextArtifacts",
                artifact_file,
                records,
                bytes,
            );
        },
    )?;
    progress.complete_file(3, "artifacts", "auditAiContextArtifacts", artifact_file);

    let mut message_ids = BTreeSet::new();
    let mut referenced_artifact_ids = BTreeSet::new();
    let mut referenced_sender_ids = BTreeSet::new();
    let mut stale_message_count = 0_u64;
    let mut omitted_artifact_reference_count = 0_u64;
    let mut omitted_relationship_reference_count = 0_u64;
    let message_file = files["messages"];
    progress.begin_file(4, "auditAiContextMessages", message_file);
    audit_ai_messages_with_progress(
        bundle_directory,
        message_file,
        |message| {
            require_ai_record_format(message.format_version, manifest.format_version, "message")?;
            require_nonempty(&message.message.canonical_id, "message ID")?;
            require_nonempty(&message.message.conversation_id, "message conversation ID")?;
            require_nonempty(&message.conversation_label, "message conversation label")?;
            if !message_ids.insert(message.message.canonical_id.clone()) {
                return Err(RestoreError::Integrity(
                    "AI context messages repeat an identity".to_string(),
                ));
            }
            if !conversation_ids.contains(&message.message.conversation_id) {
                return Err(RestoreError::Integrity(
                    "AI context message references an absent conversation".to_string(),
                ));
            }
            if message
                .message
                .sender_id
                .as_ref()
                .is_some_and(|sender| !contact_ids.contains(sender))
            {
                return Err(RestoreError::Integrity(
                    "AI context message references an absent contact".to_string(),
                ));
            }
            if let Some(sender_id) = message.message.sender_id.as_ref() {
                referenced_sender_ids.insert(sender_id.clone());
            }
            if manifest.format_version == AI_CONTEXT_FORMAT_VERSION {
                let self_participant_id = manifest
                    .context
                    .self_participant_id
                    .as_deref()
                    .ok_or_else(|| {
                        RestoreError::Integrity(
                            "AI context account-holder identity disappeared during audit"
                                .to_string(),
                        )
                    })?;
                if let Some(sender_id) = message.message.sender_id.as_deref() {
                    let expected = if sender_id == self_participant_id {
                        crate::MessageDirection::Outgoing
                    } else {
                        crate::MessageDirection::Incoming
                    };
                    if message
                        .message
                        .direction
                        .is_some_and(|direction| direction != expected)
                    {
                        return Err(RestoreError::Integrity(
                            "AI context message direction disagrees with the bound account holder"
                                .to_string(),
                        ));
                    }
                    if sender_id == self_participant_id
                        && message.sender_display_name.as_deref() != Some("You")
                    {
                        return Err(RestoreError::Integrity(
                            "AI context self-authored message is not labelled as You".to_string(),
                        ));
                    }
                }
            }
            match message.message.source_database_freshness {
                ToolSourceDatabaseFreshness::Fresh => {}
                ToolSourceDatabaseFreshness::PreservedStale => {
                    if manifest.context.preserved_stale_database_count.unwrap_or(0) == 0 {
                        return Err(RestoreError::Integrity(
                            "AI context message claims stale provenance without stale database coverage"
                                .to_string(),
                        ));
                    }
                    stale_message_count = stale_message_count.saturating_add(1);
                }
                ToolSourceDatabaseFreshness::Mixed | ToolSourceDatabaseFreshness::Derived => {
                    return Err(RestoreError::Integrity(
                        "AI context message has an invalid source freshness state".to_string(),
                    ));
                }
            }
            for reference in &message.message.artifact_references {
                if !artifact_ids.contains(&reference.artifact_id) {
                    return Err(RestoreError::Integrity(
                        "AI context message references an absent artifact".to_string(),
                    ));
                }
                referenced_artifact_ids.insert(reference.artifact_id.clone());
            }
            omitted_artifact_reference_count = omitted_artifact_reference_count
                .saturating_add(message.message.omitted_artifact_reference_count);
            omitted_relationship_reference_count = omitted_relationship_reference_count
                .saturating_add(message.message.omitted_relationship_reference_count);
            let mut message_artifact_ids = BTreeSet::new();
            for reference in &message.message.artifact_references {
                if reference.artifact_id.is_empty()
                    || !message_artifact_ids.insert(reference.artifact_id.as_str())
                    || !artifact_ids.contains(&reference.artifact_id)
                {
                    return Err(RestoreError::Integrity(
                        "AI context message references an absent, empty, or repeated artifact"
                            .to_string(),
                    ));
                }
                referenced_artifact_ids.insert(reference.artifact_id.clone());
            }
            if message.message.relationships.iter().any(|relationship| {
                relationship
                    .target_canonical_id
                    .as_deref()
                    .is_some_and(str::is_empty)
                    || relationship.resolved && relationship.target_canonical_id.is_none()
            }) {
                return Err(RestoreError::Integrity(
                    "AI context message contains a malformed relationship reference".to_string(),
                ));
            }
            Ok(())
        },
        |records, bytes| {
            progress.advance_file(
                4,
                "messages",
                "auditAiContextMessages",
                message_file,
                records,
                bytes,
            );
        },
    )?;
    progress.complete_file(4, "messages", "auditAiContextMessages", message_file);
    if referenced_artifact_ids != artifact_ids {
        return Err(RestoreError::Integrity(
            "AI context artifacts do not exactly match message references".to_string(),
        ));
    }
    required_contact_ids.extend(referenced_sender_ids);
    if contact_ids != required_contact_ids {
        return Err(RestoreError::Integrity(
            "AI context contacts do not exactly cover conversation participants and message senders"
                .to_string(),
        ));
    }
    if (omitted_artifact_reference_count > 0
        && !manifest
            .limitation_codes
            .iter()
            .any(|code| code == "malformedArtifactReferenceOmitted"))
        || (omitted_relationship_reference_count > 0
            && !manifest
                .limitation_codes
                .iter()
                .any(|code| code == "malformedRelationshipReferenceOmitted"))
    {
        return Err(RestoreError::Integrity(
            "AI context reference omission counts lack limitation evidence".to_string(),
        ));
    }

    if manifest.enabled_conversation_count as u64
        != conversation_file
            .record_count
            .saturating_add(manifest.omitted_conversation_count)
        || manifest.exported_contact_count != contact_file.record_count
        || manifest.exported_message_count != message_file.record_count
        || manifest.exported_artifact_count != artifact_file.record_count
        || manifest.artifact_resolution_error_count != artifact_error_count
    {
        return Err(RestoreError::Integrity(
            "AI context manifest aggregate counts do not match the audited records".to_string(),
        ));
    }
    let expected_bundle_id = context_bundle_id(
        manifest.format_version,
        &manifest.context,
        &manifest.policy_sha256,
        manifest.destination,
        &manifest.policy_source_fingerprint,
    )?;
    if manifest.bundle_id != expected_bundle_id {
        return Err(RestoreError::Integrity(
            "AI context bundle identity does not match its checkpoint and policy".to_string(),
        ));
    }
    let report = AiContextAuditReport {
        format_version: AI_CONTEXT_FORMAT_VERSION,
        privacy_safe_summary: true,
        schema: manifest.schema,
        export_complete: manifest.export_complete,
        file_inventory_verified: true,
        owner_only_permissions_verified: true,
        file_digests_verified: true,
        record_counts_verified: true,
        schemas_verified: true,
        unique_identities_verified: true,
        references_verified: true,
        source_freshness_verified: true,
        conversation_count: conversation_file.record_count,
        contact_count: contact_file.record_count,
        message_count: message_file.record_count,
        artifact_count: artifact_file.record_count,
        artifact_resolution_error_count: artifact_error_count,
        omitted_artifact_reference_count,
        omitted_relationship_reference_count,
        preserved_stale_message_count: stale_message_count,
        omitted_conversation_count: manifest.omitted_conversation_count,
        omitted_message_count: manifest.omitted_message_count,
        content_complete: manifest.omitted_conversation_count == 0
            && manifest.omitted_message_count == 0
            && artifact_error_count == 0
            && manifest.limitation_codes.is_empty()
            && manifest.context.source_coverage_complete,
    };
    progress.finish();
    Ok(report)
}

/// Loads the small identity manifest for a downstream projection. Callers must
/// still verify every referenced file digest before publishing derived data.
/// Keeping this check here ensures all consumers use the same checkpoint,
/// policy, account-holder, and format binding rules as `audit-ai-context`.
pub(crate) fn load_validated_ai_context_manifest(
    bundle_directory: &Path,
) -> Result<AiContextManifest, RestoreError> {
    ensure_private_directory(bundle_directory)?;
    let manifest_path = bundle_directory.join("manifest.json");
    ensure_private_regular_file(&manifest_path)?;
    let manifest_bytes = read_bounded_file(&manifest_path, 4 * 1024 * 1024)?;
    let manifest: AiContextManifest = serde_json::from_slice(&manifest_bytes)?;
    validate_ai_manifest(&manifest)?;
    let expected_bundle_id = context_bundle_id(
        manifest.format_version,
        &manifest.context,
        &manifest.policy_sha256,
        manifest.destination,
        &manifest.policy_source_fingerprint,
    )?;
    if manifest.bundle_id != expected_bundle_id {
        return Err(RestoreError::Integrity(
            "AI context bundle identity does not match its checkpoint and policy".to_string(),
        ));
    }
    Ok(manifest)
}

fn validate_ai_manifest(manifest: &AiContextManifest) -> Result<(), RestoreError> {
    require_supported_ai_context_format(manifest.format_version, "manifest")?;
    let expected_schema = if manifest.format_version == AI_CONTEXT_FORMAT_VERSION {
        AI_CONTEXT_SCHEMA
    } else {
        LEGACY_AI_CONTEXT_SCHEMA
    };
    let binding_is_valid = match manifest.format_version {
        AI_CONTEXT_FORMAT_VERSION => manifest
            .context
            .self_participant_id
            .as_deref()
            .is_some_and(valid_sha256),
        1 => manifest.context.self_participant_id.is_none(),
        _ => false,
    };
    if manifest.schema != expected_schema
        || !binding_is_valid
        || !manifest.export_complete
        || manifest.created_at_unix_nanoseconds == 0
        || !valid_sha256(&manifest.bundle_id)
        || !valid_sha256(&manifest.policy_sha256)
        || manifest.policy_source_fingerprint.is_empty()
        || manifest.requester_id.is_empty()
        || manifest.requester_id.len() > 256
        || manifest.context.account_id.is_empty()
        || manifest.context.replica_id.is_empty()
        || manifest.context.source_fingerprint.is_empty()
        || manifest.context.checkpoint_revision.is_empty()
    {
        return Err(RestoreError::Integrity(
            "AI context manifest identity or completion evidence is invalid".to_string(),
        ));
    }
    let mut roles = BTreeSet::new();
    let mut paths = BTreeSet::new();
    if manifest.files.len() != 4
        || manifest.files.iter().any(|file| {
            file.role.is_empty()
                || !roles.insert(file.role.clone())
                || !paths.insert(file.relative_path.clone())
                || !valid_sha256(&file.sha256)
        })
    {
        return Err(RestoreError::Integrity(
            "AI context manifest file evidence is invalid".to_string(),
        ));
    }
    Ok(())
}

fn validate_entity_freshness(
    freshness: ToolSourceDatabaseFreshness,
    context: &AiContextHealth,
) -> Result<(), RestoreError> {
    if matches!(
        freshness,
        ToolSourceDatabaseFreshness::PreservedStale | ToolSourceDatabaseFreshness::Mixed
    ) && context.preserved_stale_database_count.unwrap_or(0) == 0
    {
        return Err(RestoreError::Integrity(
            "AI context entity claims stale provenance without stale database coverage".to_string(),
        ));
    }
    Ok(())
}

fn validate_ai_artifact_detail(detail: &AiContextArtifactDetail) -> Result<(), RestoreError> {
    let metadata_unavailable = detail.verification_state == "metadataUnavailable"
        && detail.kind == ArtifactKind::Unknown
        && detail.role == ArtifactRole::Unknown
        && detail.availability == ArtifactAvailability::MetadataMissing
        && detail.decode_state == ArtifactDecodeState::Failed
        && detail.source.is_none()
        && detail.decoded.is_none()
        && detail
            .limitation_codes
            .iter()
            .any(|code| code == "unavailableArtifactMetadataSynthesized");
    let digest_verified = detail.verification_state == "connectorDigestVerified"
        && !detail
            .limitation_codes
            .iter()
            .any(|code| code == "unavailableArtifactMetadataSynthesized")
        && !((detail.source.is_none() && detail.decoded.is_none())
            && matches!(
                detail.availability,
                ArtifactAvailability::Downloaded | ArtifactAvailability::MaterializedFromDatabase
            ));
    if !metadata_unavailable && !digest_verified {
        return Err(RestoreError::Integrity(
            "AI context artifact verification evidence is inconsistent".to_string(),
        ));
    }
    for file in [detail.source.as_ref(), detail.decoded.as_ref()]
        .into_iter()
        .flatten()
    {
        if !valid_sha256(&file.sha256) || file.format.is_empty() {
            return Err(RestoreError::Integrity(
                "AI context artifact file evidence is invalid".to_string(),
            ));
        }
        if let Some(relative) = &file.account_relative_path {
            let path = Path::new(relative);
            if path.is_absolute()
                || path
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                return Err(RestoreError::Integrity(
                    "AI context artifact contains an unsafe relative path".to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn audit_ai_messages_with_progress(
    directory: &Path,
    evidence: &AiContextFile,
    mut visitor: impl FnMut(AiContextMessage) -> Result<(), RestoreError>,
    progress: impl FnMut(u64, u64),
) -> Result<(), RestoreError> {
    audit_ndjson_values_with_progress(
        directory,
        evidence,
        |value| {
            let object = value.as_object().ok_or_else(|| {
                RestoreError::Integrity("AI context message record is not an object".to_string())
            })?;
            const ALLOWED_FIELDS: &[&str] = &[
                "formatVersion",
                "conversationLabel",
                "senderDisplayName",
                "canonicalId",
                "conversationId",
                "sourceDatabaseFreshness",
                "senderId",
                "createdAtUnix",
                "conversationOrdinal",
                "direction",
                "logicalType",
                "subType",
                "payloadKind",
                "payloadSummary",
                "payloadSummaryTruncated",
                "artifactReferences",
                "relationships",
                "omittedArtifactReferenceCount",
                "omittedRelationshipReferenceCount",
            ];
            if object
                .keys()
                .any(|field| !ALLOWED_FIELDS.contains(&field.as_str()))
            {
                return Err(RestoreError::Integrity(
                    "AI context message contains an unknown field".to_string(),
                ));
            }
            visitor(serde_json::from_value(value)?)
        },
        progress,
    )
}

fn audit_ndjson_with_progress<T, F, P>(
    directory: &Path,
    evidence: &AiContextFile,
    mut visitor: F,
    progress: P,
) -> Result<(), RestoreError>
where
    T: serde::de::DeserializeOwned,
    F: FnMut(T) -> Result<(), RestoreError>,
    P: FnMut(u64, u64),
{
    audit_ndjson_values_with_progress(
        directory,
        evidence,
        |value| visitor(serde_json::from_value(value)?),
        progress,
    )
}

fn audit_ndjson_values_with_progress(
    directory: &Path,
    evidence: &AiContextFile,
    mut visitor: impl FnMut(serde_json::Value) -> Result<(), RestoreError>,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), RestoreError> {
    const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;
    let path = directory.join(&evidence.relative_path);
    ensure_private_regular_file(&path)?;
    let metadata = fs::metadata(&path)?;
    if metadata.len() != evidence.byte_count {
        return Err(RestoreError::Integrity(
            "AI context file byte count does not match its manifest".to_string(),
        ));
    }
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = Vec::new();
    let mut count = 0_u64;
    let mut completed_byte_count = 0_u64;
    loop {
        buffer.clear();
        let read = reader.read_until(b'\n', &mut buffer)?;
        if read == 0 {
            break;
        }
        if buffer.len() > MAX_RECORD_BYTES {
            return Err(RestoreError::Integrity(
                "AI context record exceeds its audit byte limit".to_string(),
            ));
        }
        if buffer.last() != Some(&b'\n') || buffer.len() == 1 {
            return Err(RestoreError::Integrity(
                "AI context JSONL record is empty or lacks a newline".to_string(),
            ));
        }
        hasher.update(&buffer);
        visitor(serde_json::from_slice(&buffer[..buffer.len() - 1])?)?;
        count = count.saturating_add(1);
        completed_byte_count =
            completed_byte_count.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        progress(count, completed_byte_count);
    }
    if count != evidence.record_count || hex::encode(hasher.finalize()) != evidence.sha256 {
        return Err(RestoreError::Integrity(
            "AI context file digest or record count does not match its manifest".to_string(),
        ));
    }
    Ok(())
}

fn read_bounded_file(path: &Path, maximum_bytes: u64) -> Result<Vec<u8>, RestoreError> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum_bytes {
        return Err(RestoreError::Integrity(
            "AI context file exceeds its audit byte limit".to_string(),
        ));
    }
    Ok(bytes)
}

fn require_supported_ai_context_format(
    format_version: u32,
    kind: &str,
) -> Result<(), RestoreError> {
    if !matches!(format_version, 1 | AI_CONTEXT_FORMAT_VERSION) {
        return Err(RestoreError::Integrity(format!(
            "AI context {kind} has an unsupported format version"
        )));
    }
    Ok(())
}

fn require_ai_record_format(
    format_version: u32,
    manifest_format_version: u32,
    kind: &str,
) -> Result<(), RestoreError> {
    require_supported_ai_context_format(format_version, kind)?;
    if format_version != manifest_format_version {
        return Err(RestoreError::Integrity(format!(
            "AI context {kind} format version disagrees with its manifest"
        )));
    }
    Ok(())
}

fn require_nonempty(value: &str, kind: &str) -> Result<(), RestoreError> {
    if value.is_empty() {
        return Err(RestoreError::Integrity(format!(
            "AI context {kind} cannot be empty"
        )));
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sanitize_query_result(
    result: ConnectorResult,
    context: &AiContextHealth,
) -> Result<AiQueryResult, RestoreError> {
    Ok(match result {
        ConnectorResult::Capabilities(value) => AiQueryResult::Capabilities(Box::new(value)),
        ConnectorResult::Status(value) => AiQueryResult::Status(Box::new(value)),
        ConnectorResult::DirectStatus(_) => {
            return Err(RestoreError::Integrity(
                "replica AI query received a direct-source status".to_string(),
            ));
        }
        ConnectorResult::Coverage(_) => AiQueryResult::Coverage(context.clone()),
        ConnectorResult::Changes(value) => AiQueryResult::Changes(value),
        ConnectorResult::CachedMoments(value) => AiQueryResult::CachedMoments(value),
        ConnectorResult::Conversations(value) => AiQueryResult::Conversations(value),
        ConnectorResult::Messages(mut value) => {
            for message in &mut value.messages {
                normalize_message_identity(message, context);
            }
            AiQueryResult::Messages(value)
        }
        ConnectorResult::Message(mut value) => {
            if let Some(message) = &mut value {
                normalize_message_identity(message, context);
            }
            AiQueryResult::Message(value)
        }
        ConnectorResult::Artifact(value) => AiQueryResult::Artifact(value),
        ConnectorResult::Contact(mut value) => {
            if context.self_participant_id.as_deref() == Some(value.participant_id.as_str()) {
                value.display_name = "You".to_string();
            }
            AiQueryResult::Contact(value)
        }
        ConnectorResult::Conversation(mut value) => {
            mark_self_participant(&mut value, context);
            AiQueryResult::Conversation(value)
        }
        ConnectorResult::Draft(_) | ConnectorResult::Preview(_) => {
            return Err(RestoreError::Integrity(
                "AI query returned a write-capable result".to_string(),
            ));
        }
    })
}

fn sanitize_query_error(error: ConnectorErrorBody) -> ConnectorErrorBody {
    ConnectorErrorBody {
        code: error.code,
        message: safe_query_error_message(error.code).to_string(),
        retryable: error.retryable,
    }
}

fn safe_query_error_message(code: ConnectorErrorCode) -> &'static str {
    match code {
        ConnectorErrorCode::InvalidRequest => {
            "The request is invalid; inspect the documented operation schema."
        }
        ConnectorErrorCode::Unauthorized => {
            "The current owner-created policy does not authorize this read."
        }
        ConnectorErrorCode::NotFound => "The authorized replica has no matching record.",
        ConnectorErrorCode::Unavailable => "The requested read surface is currently unavailable.",
        ConnectorErrorCode::Conflict => {
            "The request conflicts with the current replica or policy checkpoint."
        }
        ConnectorErrorCode::IntegrityFailure => {
            "The read failed an integrity check; inspect local operator diagnostics."
        }
    }
}

fn validate_ai_query(request: &AiQueryRequest) -> Result<(), RestoreError> {
    if request.format_version != AI_QUERY_FORMAT_VERSION {
        return Err(RestoreError::Integrity(format!(
            "unsupported AI query format version {}",
            request.format_version
        )));
    }
    if !is_read_operation(&request.operation) {
        return Err(RestoreError::Integrity(
            "AI query CLI accepts read operations only".to_string(),
        ));
    }
    Ok(())
}

fn is_read_operation(operation: &ConnectorOperation) -> bool {
    matches!(
        operation,
        ConnectorOperation::Capabilities
            | ConnectorOperation::Status
            | ConnectorOperation::Coverage
            | ConnectorOperation::GetChanges { .. }
            | ConnectorOperation::GetCachedMoments { .. }
            | ConnectorOperation::ListConversations { .. }
            | ConnectorOperation::SearchMessages { .. }
            | ConnectorOperation::GetMessages { .. }
            | ConnectorOperation::GetMessage { .. }
            | ConnectorOperation::GetArtifact { .. }
            | ConnectorOperation::ResolveContact { .. }
            | ConnectorOperation::ResolveConversation { .. }
    )
}

fn context_health(status: &ReplicaStatus) -> Result<AiContextHealth, RestoreError> {
    let account_id = status.account_id.clone().ok_or_else(|| {
        RestoreError::Integrity("AI context replica is not initialized".to_string())
    })?;
    let source_fingerprint = status.current_source_fingerprint.clone().ok_or_else(|| {
        RestoreError::Integrity("AI context replica has no source checkpoint".to_string())
    })?;
    let checkpoint_revision = status.checkpoint_revision.clone().ok_or_else(|| {
        RestoreError::Integrity("AI context replica has no checkpoint revision".to_string())
    })?;
    let unavailable = status.unavailable_database_count.unwrap_or(0);
    let stale = status.preserved_stale_database_count.unwrap_or(0);
    let semantic_gaps = status.semantic_gap_count.unwrap_or(0);
    let candidate_gaps = status.message_candidate_gap_count.unwrap_or(0);
    let unavailable_artifacts = status.unavailable_artifact_count.unwrap_or(0);
    let artifact_decode_gaps = status.artifact_decode_gap_count.unwrap_or(0);
    let entity_gaps = status.entity_decode_gap_count.unwrap_or(0);
    let mut limitations = status.limitation_codes.clone();
    if unavailable > 0 {
        limitations.push("unavailableDatabases".to_string());
    }
    if stale > 0 {
        limitations.push("preservedStaleDatabases".to_string());
    }
    if semantic_gaps > 0 {
        limitations.push("semanticDecodeGaps".to_string());
    }
    if candidate_gaps > 0 {
        limitations.push("unhandledMessageCandidates".to_string());
    }
    if unavailable_artifacts > 0 {
        limitations.push("unavailableArtifacts".to_string());
    }
    if artifact_decode_gaps > 0 {
        limitations.push("artifactDecodeGaps".to_string());
    }
    if entity_gaps > 0 {
        limitations.push("entityDecodeGaps".to_string());
    }
    if status.restoration_complete != Some(true) {
        limitations.push("restorationIncomplete".to_string());
    }
    if status.self_participant_id.is_none() {
        limitations.push("accountHolderUnbound".to_string());
    }
    limitations.sort();
    limitations.dedup();
    let source_coverage_complete = status.health == ReplicaHealthState::CurrentComplete
        && unavailable == 0
        && stale == 0
        && limitations.is_empty();
    let coverage_note = if unavailable > 0 || stale > 0 {
        format!(
            "Synchronization continued with {unavailable} unavailable database(s); {stale} database(s) contribute preserved stale records. Absence from unavailable shards is not evidence of deletion."
        )
    } else if source_coverage_complete {
        "The current authorized replica reports complete source coverage.".to_string()
    } else {
        "The current authorized replica has explicit semantic, entity, media, or restoration limitations; inspect limitationCodes before drawing absence conclusions."
            .to_string()
    };
    Ok(AiContextHealth {
        account_id,
        self_participant_id: status.self_participant_id.clone(),
        replica_id: status.replica_id.clone(),
        source_fingerprint,
        checkpoint_revision,
        health: status.health,
        client_build_compatibility: status
            .client_build_compatibility
            .as_ref()
            .map(|compatibility| compatibility.state),
        archive_scope: status.archive_scope,
        authoritative_database_coverage: status.authoritative_database_coverage,
        total_database_count: status.total_database_count,
        fresh_database_count: status.restored_database_count,
        unavailable_database_count: status.unavailable_database_count,
        preserved_stale_database_count: status.preserved_stale_database_count,
        conversation_count: status.conversation_count,
        participant_count: status.participant_count,
        message_count: status.message_count,
        artifact_count: status.artifact_count,
        semantic_gap_count: status.semantic_gap_count,
        message_candidate_gap_count: status.message_candidate_gap_count,
        unavailable_artifact_count: status.unavailable_artifact_count,
        artifact_decode_gap_count: status.artifact_decode_gap_count,
        entity_decode_gap_count: status.entity_decode_gap_count,
        checkpoint_age_seconds: status.checkpoint_age_seconds,
        source_coverage_complete,
        limitation_codes: limitations,
        coverage_note,
    })
}

fn require_same_checkpoint(
    before: &ReplicaStatus,
    after: &ReplicaStatus,
    operation: &str,
) -> Result<(), RestoreError> {
    if before.replica_id != after.replica_id
        || before.account_id != after.account_id
        || before.self_participant_id != after.self_participant_id
        || before.current_source_fingerprint != after.current_source_fingerprint
        || before.checkpoint_revision != after.checkpoint_revision
    {
        return Err(RestoreError::Integrity(format!(
            "replica changed during {operation}; retry against one checkpoint"
        )));
    }
    Ok(())
}

struct AiConnectorReader<'a, 'b> {
    service: &'a ConnectorService<'b>,
    requester_id: &'a str,
    destination: ConnectorDestination,
    sequence: u64,
}

impl<'a, 'b> AiConnectorReader<'a, 'b> {
    fn new(
        service: &'a ConnectorService<'b>,
        requester_id: &'a str,
        destination: ConnectorDestination,
    ) -> Self {
        Self {
            service,
            requester_id,
            destination,
            sequence: 0,
        }
    }

    fn next_request(&mut self, operation: ConnectorOperation) -> ConnectorRequest {
        self.sequence = self.sequence.saturating_add(1);
        ConnectorRequest {
            api_version: CONNECTOR_API_VERSION.to_string(),
            request_id: format!("ai-export-{}", self.sequence),
            requester_id: self.requester_id.to_string(),
            destination: self.destination,
            operation,
        }
    }

    fn raw_call(&mut self, operation: ConnectorOperation) -> crate::connector::ConnectorResponse {
        let request = self.next_request(operation);
        self.service.handle_with_external_checkpoint_guard(request)
    }

    fn export_artifacts(
        &mut self,
        artifact_conversations: &BTreeMap<String, BTreeSet<String>>,
        visit: impl FnMut(
            &str,
            &BTreeSet<String>,
            Result<ConnectorArtifactView, ConnectorErrorBody>,
        ) -> Result<(), RestoreError>,
    ) -> Result<crate::connector::ConnectorArtifactExportSummary, RestoreError> {
        let (conversation_id, artifact_id) = artifact_conversations
            .iter()
            .next()
            .and_then(|(artifact_id, conversations)| {
                conversations
                    .iter()
                    .next()
                    .map(|conversation_id| (conversation_id.clone(), artifact_id.clone()))
            })
            .unwrap_or_default();
        let request = self.next_request(ConnectorOperation::GetArtifact {
            conversation_id,
            artifact_id,
        });
        self.service
            .export_authorized_artifacts(&request, artifact_conversations, visit)
    }

    fn call(&mut self, operation: ConnectorOperation) -> Result<ConnectorResult, RestoreError> {
        let response = self.raw_call(operation);
        if response.ok {
            return response.result.ok_or_else(|| {
                RestoreError::Integrity("AI connector response omitted its result".to_string())
            });
        }
        let error = response.error.ok_or_else(|| {
            RestoreError::Integrity("AI connector failure omitted its error".to_string())
        })?;
        Err(RestoreError::Integrity(format!(
            "AI connector request failed ({:?}): {}",
            error.code, error.message
        )))
    }
}

fn expect_status(
    result: ConnectorResult,
) -> Result<crate::connector::ConnectorStatus, RestoreError> {
    match result {
        ConnectorResult::Status(value) => Ok(value),
        _ => Err(unexpected_connector_result("status")),
    }
}

fn expect_conversations(
    result: ConnectorResult,
) -> Result<crate::connector::ConnectorConversationList, RestoreError> {
    match result {
        ConnectorResult::Conversations(value) => Ok(value),
        _ => Err(unexpected_connector_result("conversations")),
    }
}

fn list_all_conversations(
    reader: &mut AiConnectorReader<'_, '_>,
) -> Result<ConnectorConversationList, RestoreError> {
    let mut cursor = None;
    let mut combined: Option<ConnectorConversationList> = None;
    loop {
        let mut page =
            expect_conversations(reader.call(ConnectorOperation::ListConversations {
                cursor: cursor.clone(),
                limit: Some(EXPORT_PAGE_SIZE),
            })?)?;
        let next = page.next_cursor.take();
        if let Some(combined) = &mut combined {
            if combined.account_id != page.account_id {
                return Err(RestoreError::Integrity(
                    "connector conversation pages changed account identity".to_string(),
                ));
            }
            combined.conversations.append(&mut page.conversations);
            combined.omitted_conversation_count = combined
                .omitted_conversation_count
                .saturating_add(page.omitted_conversation_count);
            combined.limitation_codes.extend(page.limitation_codes);
            combined.limitation_codes.sort();
            combined.limitation_codes.dedup();
        } else {
            combined = Some(page);
        }
        match next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    combined.ok_or_else(|| unexpected_connector_result("conversations"))
}

fn unexpected_connector_result(expected: &str) -> RestoreError {
    RestoreError::Integrity(format!(
        "AI connector returned an unexpected result instead of {expected}"
    ))
}

fn sanitize_artifact(artifact: ConnectorArtifactView) -> AiContextArtifactDetail {
    let verification_state = if artifact
        .limitation_codes
        .iter()
        .any(|code| code == "unavailableArtifactMetadataSynthesized")
    {
        "metadataUnavailable"
    } else {
        "connectorDigestVerified"
    };
    AiContextArtifactDetail {
        kind: artifact.kind,
        role: artifact.role,
        availability: artifact.availability,
        decode_state: artifact.decode_state,
        source: artifact.source.map(sanitize_artifact_file),
        decoded: artifact.decoded.map(sanitize_artifact_file),
        verification_state: verification_state.to_string(),
        limitation_codes: artifact.limitation_codes,
    }
}

fn sanitize_artifact_file(file: ConnectorArtifactFile) -> AiContextArtifactFile {
    AiContextArtifactFile {
        origin: file.origin,
        account_relative_path: file.account_relative_path,
        byte_count: file.byte_count,
        sha256: file.sha256,
        format: file.format,
    }
}

fn context_bundle_id(
    format_version: u32,
    health: &AiContextHealth,
    policy_sha256: &str,
    destination: ConnectorDestination,
    policy_source_fingerprint: &str,
) -> Result<String, RestoreError> {
    require_supported_ai_context_format(format_version, "bundle identity")?;
    let identity = if format_version == AI_CONTEXT_FORMAT_VERSION {
        serde_json::json!({
            "formatVersion": AI_CONTEXT_FORMAT_VERSION,
            "schema": AI_CONTEXT_SCHEMA,
            "accountId": health.account_id,
            "selfParticipantId": health.self_participant_id,
            "replicaId": health.replica_id,
            "sourceFingerprint": health.source_fingerprint,
            "checkpointRevision": health.checkpoint_revision,
            "policySHA256": policy_sha256,
            "policySourceFingerprint": policy_source_fingerprint,
            "destination": destination,
        })
    } else {
        serde_json::json!({
            "formatVersion": 1,
            "schema": LEGACY_AI_CONTEXT_SCHEMA,
            "accountId": health.account_id,
            "replicaId": health.replica_id,
            "sourceFingerprint": health.source_fingerprint,
            "checkpointRevision": health.checkpoint_revision,
            "policySHA256": policy_sha256,
            "policySourceFingerprint": policy_source_fingerprint,
            "destination": destination,
        })
    };
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&identity)?)))
}

fn require_bound_account_holder(health: &AiContextHealth) -> Result<(), RestoreError> {
    if health
        .self_participant_id
        .as_deref()
        .is_none_or(|identifier| !valid_sha256(identifier))
    {
        return Err(RestoreError::Integrity(
            "AI context export requires a replica with a verified account-holder binding"
                .to_string(),
        ));
    }
    Ok(())
}

fn mark_self_participant(conversation: &mut ResolvedConversation, health: &AiContextHealth) {
    let Some(self_participant_id) = health.self_participant_id.as_deref() else {
        return;
    };
    let mut found = false;
    for participant in &mut conversation.participants {
        if participant.participant_id == self_participant_id {
            participant.display_name = "You".to_string();
            found = true;
        }
    }
    if found {
        conversation.participants.sort_by(|left, right| {
            (&left.display_name, &left.participant_id)
                .cmp(&(&right.display_name, &right.participant_id))
        });
        let names = conversation
            .participants
            .iter()
            .map(|participant| participant.display_name.as_str())
            .take(4)
            .collect::<Vec<_>>();
        conversation.human_label = if conversation.participants.len() > names.len() {
            format!(
                "{} +{}",
                names.join(", "),
                conversation.participants.len() - names.len()
            )
        } else {
            names.join(", ")
        };
    }
}

fn normalize_message_identity(
    message: &mut crate::tools::MinimizedMessage,
    health: &AiContextHealth,
) {
    let (Some(sender_id), Some(direction), Some(self_participant_id)) = (
        message.sender_id.as_deref(),
        message.direction.as_mut(),
        health.self_participant_id.as_deref(),
    ) else {
        return;
    };
    *direction = if sender_id == self_participant_id {
        crate::MessageDirection::Outgoing
    } else {
        crate::MessageDirection::Incoming
    };
}

struct NdjsonWriter {
    role: String,
    relative_path: String,
    writer: BufWriter<File>,
    hasher: Sha256,
    record_count: u64,
    byte_count: u64,
}

impl NdjsonWriter {
    fn create(directory: &Path, role: &str, relative_path: &str) -> Result<Self, RestoreError> {
        let path = directory.join(relative_path);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        Ok(Self {
            role: role.to_string(),
            relative_path: relative_path.to_string(),
            writer: BufWriter::new(file),
            hasher: Sha256::new(),
            record_count: 0,
            byte_count: 0,
        })
    }

    fn write<T: Serialize>(&mut self, value: &T) -> Result<(), RestoreError> {
        let mut encoded = serde_json::to_vec(value)?;
        encoded.push(b'\n');
        self.writer.write_all(&encoded)?;
        self.hasher.update(&encoded);
        self.record_count = self.record_count.saturating_add(1);
        self.byte_count = self.byte_count.saturating_add(encoded.len() as u64);
        Ok(())
    }

    fn finish(mut self) -> Result<AiContextFile, RestoreError> {
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        Ok(AiContextFile {
            role: self.role,
            relative_path: self.relative_path,
            record_count: self.record_count,
            byte_count: self.byte_count,
            sha256: hex::encode(self.hasher.finalize()),
        })
    }
}

fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<(), RestoreError> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn observe_export(
    observer: &dyn ProgressObserver,
    state: ProgressState,
    operation: &str,
    completed: u64,
    total: u64,
    phase_completed: u64,
    file_index: Option<usize>,
    file_count: usize,
    started: Instant,
) {
    let mut event = ProgressEvent::new(
        ProgressPhase::ContextExport,
        state,
        operation,
        ProgressUnit::Records,
        completed,
        total,
        phase_completed.min(PHASE_RESOLUTION),
        PHASE_RESOLUTION,
    );
    event.file_index = file_index;
    event.file_count = Some(file_count);
    event.restored_record_count = Some(completed);
    event.elapsed_milliseconds = Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64);
    observer.observe(event);
}

fn progress_fraction(completed: u64, total: u64, span: u64) -> u64 {
    if total == 0 {
        return span;
    }
    u64::try_from(completed.min(total) as u128 * span as u128 / total as u128).unwrap_or(span)
}

fn unix_nanoseconds() -> Result<u128, RestoreError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(|_| RestoreError::Integrity("system clock predates Unix epoch".to_string()))
}

fn short_identifier(value: &str) -> String {
    value.chars().take(12).collect()
}
