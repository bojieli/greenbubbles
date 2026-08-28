use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::archive::{
    ensure_private_directory, ensure_private_regular_file, load_conversation_ids, load_report,
    private_regular_file_exists,
};
use crate::{
    ArtifactRole, CanonicalCachedMoment, CanonicalConversation, CanonicalMessage, ConversationKind,
    EntityDecodeState, MessageDirection, MessageRelationshipKind, RestorationCompletion,
    RestoreError, TypedPayload,
};

pub(crate) const MAX_TOOL_RESULTS: usize = 1_000;
pub(crate) const MAX_MESSAGE_SUMMARY_BYTES: usize = 64 * 1024;
pub(crate) const MAX_DRAFT_BYTES: usize = 256 * 1024;
pub(crate) const MAX_SEARCH_QUERY_BYTES: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolCapability {
    ListConversations,
    ReadRecentMessages,
    SearchMessages,
    CreateDraft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolDataDestination {
    LocalModel,
    RemoteModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolMessageField {
    Sender,
    CreatedAt,
    Direction,
    MessageType,
    Content,
    Attachments,
    Relationships,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationToolScope {
    pub capabilities: BTreeSet<ToolCapability>,
    pub message_fields: BTreeSet<ToolMessageField>,
    pub not_before_unix: Option<i64>,
    pub not_after_unix: Option<i64>,
    pub allow_remote_model: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CachedMomentField {
    Author,
    CreatedAt,
    ContentType,
    ContentDescription,
    Title,
    Description,
    ContentUrl,
    MediaCount,
    LikeCount,
    CommentCount,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedMomentsToolScope {
    pub fields: BTreeSet<CachedMomentField>,
    pub not_before_unix: Option<i64>,
    pub not_after_unix: Option<i64>,
    pub allow_remote_model: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAuthorizationPolicy {
    pub format_version: u32,
    pub account_id: String,
    pub created_from_source_fingerprint: String,
    pub conversation_scopes: BTreeMap<String, ConversationToolScope>,
    #[serde(default)]
    pub cached_moments_scope: Option<CachedMomentsToolScope>,
    pub maximum_result_count: usize,
    pub maximum_message_summary_bytes: usize,
    pub maximum_draft_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MinimizedCachedMoment {
    pub canonical_id: String,
    pub source_database_freshness: ToolSourceDatabaseFreshness,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at_unix: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub like_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment_count: Option<u64>,
    pub text_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolConversationView {
    pub conversation_id: String,
    pub kind: ConversationKind,
    pub participant_count: usize,
    pub entity_decode_state: EntityDecodeState,
    pub capabilities: BTreeSet<ToolCapability>,
    pub message_fields: BTreeSet<ToolMessageField>,
    pub not_before_unix: Option<i64>,
    pub not_after_unix: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolArtifactReference {
    pub artifact_id: String,
    pub role: ArtifactRole,
    pub preferred: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRelationshipReference {
    pub kind: MessageRelationshipKind,
    pub target_canonical_id: Option<String>,
    pub resolved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MinimizedMessage {
    pub canonical_id: String,
    pub conversation_id: String,
    pub source_database_freshness: ToolSourceDatabaseFreshness,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at_unix: Option<i64>,
    pub conversation_ordinal: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<MessageDirection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_type: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_type: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_summary_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_references: Vec<ToolArtifactReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationships: Vec<ToolRelationshipReference>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolSourceDatabaseFreshness {
    Fresh,
    PreservedStale,
    Mixed,
    Derived,
}

pub(crate) fn entity_source_database_freshness(
    source_set_ids: impl Iterator<Item = String>,
    preserved_stale_source_set_ids: &BTreeSet<String>,
) -> ToolSourceDatabaseFreshness {
    let mut fresh = false;
    let mut stale = false;
    for source_set_id in source_set_ids {
        if preserved_stale_source_set_ids.contains(&source_set_id) {
            stale = true;
        } else {
            fresh = true;
        }
    }
    match (fresh, stale) {
        (true, true) => ToolSourceDatabaseFreshness::Mixed,
        (true, false) => ToolSourceDatabaseFreshness::Fresh,
        (false, true) => ToolSourceDatabaseFreshness::PreservedStale,
        (false, false) => ToolSourceDatabaseFreshness::Derived,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolConversationList {
    pub destination: ToolDataDestination,
    pub conversations: Vec<ToolConversationView>,
    #[serde(default)]
    pub omitted_conversation_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolMessageResult {
    pub destination: ToolDataDestination,
    pub searched_conversation_count: usize,
    pub messages: Vec<MinimizedMessage>,
    pub restoration_completion: RestorationCompletion,
    #[serde(default)]
    pub omitted_message_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DraftState {
    DraftOnly,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DraftRecord<'a> {
    pub format_version: u32,
    pub draft_id: &'a str,
    pub account_id: &'a str,
    pub conversation_id: &'a str,
    pub created_at_unix_nanoseconds: u128,
    pub state: DraftState,
    pub body: &'a str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftReceipt {
    pub draft_id: String,
    pub conversation_id: String,
    pub created_at_unix_nanoseconds: u128,
    pub state: DraftState,
    pub body_byte_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolAuditOutcome {
    Completed,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolOperation {
    ListConversations,
    ReadRecentMessages,
    SearchMessages,
    CreateDraft,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAuditEvent {
    pub format_version: u32,
    pub event_id: String,
    pub observed_at_unix_nanoseconds: u128,
    pub account_id: String,
    pub requester_id: String,
    pub operation: ToolOperation,
    pub conversation_id: Option<String>,
    pub destination: ToolDataDestination,
    pub outcome: ToolAuditOutcome,
    pub returned_item_count: usize,
    pub released_body_byte_count: usize,
    pub request_body_byte_count: usize,
}

pub fn create_tool_policy(
    archive_directory: &Path,
    policy_path: &Path,
    conversation_scopes: BTreeMap<String, ConversationToolScope>,
    maximum_result_count: usize,
    maximum_message_summary_bytes: usize,
    maximum_draft_bytes: usize,
) -> Result<ToolAuthorizationPolicy, RestoreError> {
    create_tool_policy_with_cached_moments(
        archive_directory,
        policy_path,
        conversation_scopes,
        None,
        maximum_result_count,
        maximum_message_summary_bytes,
        maximum_draft_bytes,
    )
}

/// Creates one identical, explicit scope for every conversation in an
/// archive. Conversation identifiers are loaded internally so large accounts
/// do not expose thousands of opaque IDs through process arguments.
#[allow(clippy::too_many_arguments)]
pub fn create_all_conversations_tool_policy_with_cached_moments(
    archive_directory: &Path,
    policy_path: &Path,
    conversation_scope: ConversationToolScope,
    cached_moments_scope: Option<CachedMomentsToolScope>,
    maximum_result_count: usize,
    maximum_message_summary_bytes: usize,
    maximum_draft_bytes: usize,
) -> Result<ToolAuthorizationPolicy, RestoreError> {
    let report = load_report(archive_directory)?;
    let conversation_ids = load_conversation_ids(archive_directory, &report.account_id)?;
    let conversation_scopes = conversation_ids
        .into_iter()
        .map(|conversation_id| (conversation_id, conversation_scope.clone()))
        .collect();
    create_tool_policy_with_cached_moments(
        archive_directory,
        policy_path,
        conversation_scopes,
        cached_moments_scope,
        maximum_result_count,
        maximum_message_summary_bytes,
        maximum_draft_bytes,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn create_tool_policy_with_cached_moments(
    archive_directory: &Path,
    policy_path: &Path,
    conversation_scopes: BTreeMap<String, ConversationToolScope>,
    cached_moments_scope: Option<CachedMomentsToolScope>,
    maximum_result_count: usize,
    maximum_message_summary_bytes: usize,
    maximum_draft_bytes: usize,
) -> Result<ToolAuthorizationPolicy, RestoreError> {
    validate_tool_scopes(&conversation_scopes, cached_moments_scope.as_ref())?;
    let report = load_report(archive_directory)?;
    let known = load_conversation_ids(archive_directory, &report.account_id)?;
    if let Some(unknown) = conversation_scopes
        .keys()
        .find(|identifier| !known.contains(*identifier))
    {
        return Err(RestoreError::Integrity(format!(
            "conversation is not present in the archive: {unknown}"
        )));
    }
    let policy = ToolAuthorizationPolicy {
        format_version: 3,
        account_id: report.account_id,
        created_from_source_fingerprint: report.source_fingerprint,
        conversation_scopes,
        cached_moments_scope,
        maximum_result_count: maximum_result_count.clamp(1, MAX_TOOL_RESULTS),
        maximum_message_summary_bytes: maximum_message_summary_bytes
            .clamp(1, MAX_MESSAGE_SUMMARY_BYTES),
        maximum_draft_bytes: maximum_draft_bytes.clamp(1, MAX_DRAFT_BYTES),
    };
    write_owner_only_json(policy_path, &policy)?;
    Ok(policy)
}

pub struct LocalToolService {
    archive_directory: PathBuf,
    audit_path: PathBuf,
    requester_id: String,
    policy: ToolAuthorizationPolicy,
    restoration_completion: RestorationCompletion,
    preserved_stale_source_set_ids: BTreeSet<String>,
}

impl LocalToolService {
    pub fn open(
        archive_directory: &Path,
        policy_path: &Path,
        audit_path: &Path,
        requester_id: &str,
    ) -> Result<Self, RestoreError> {
        if requester_id.is_empty() || requester_id.len() > 256 {
            return Err(RestoreError::Integrity(
                "requester ID must be between 1 and 256 bytes".to_string(),
            ));
        }
        let report = load_report(archive_directory)?;
        let policy = load_tool_policy(policy_path)?;
        if report.account_id != policy.account_id {
            return Err(RestoreError::Integrity(
                "tool policy belongs to a different account".to_string(),
            ));
        }
        let parent = audit_path
            .parent()
            .ok_or_else(|| RestoreError::UnsafePath("audit log has no parent".to_string()))?;
        ensure_private_directory(parent)?;
        if audit_path.try_exists()? {
            ensure_private_regular_file(audit_path)?;
        }
        Ok(Self {
            archive_directory: archive_directory.to_path_buf(),
            audit_path: audit_path.to_path_buf(),
            requester_id: requester_id.to_string(),
            policy,
            restoration_completion: report.completion,
            preserved_stale_source_set_ids: report
                .database_coverage
                .map(|coverage| {
                    coverage
                        .preserved_stale_source_set_ids
                        .into_iter()
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    pub fn list_enabled_conversations(
        &self,
        destination: ToolDataDestination,
    ) -> Result<ToolConversationList, RestoreError> {
        let path = self.archive_directory.join("conversations.ndjson");
        let mut conversations = Vec::new();
        let mut seen = HashSet::new();
        let mut omitted_conversation_count = 0_u64;
        let mut limitation_codes = BTreeSet::new();
        if !private_regular_file_exists(&path)? {
            limitation_codes.insert("archiveConversationLedgerUnavailable".to_string());
        } else {
            let file = File::open(path)?;
            for line in BufReader::new(file).lines() {
                let line = match line {
                    Ok(line) => line,
                    Err(_) => {
                        omitted_conversation_count = omitted_conversation_count.saturating_add(1);
                        continue;
                    }
                };
                let conversation: CanonicalConversation = match serde_json::from_str(&line) {
                    Ok(conversation) => conversation,
                    Err(_) => {
                        omitted_conversation_count = omitted_conversation_count.saturating_add(1);
                        continue;
                    }
                };
                if conversation.account_id != self.policy.account_id
                    || conversation.conversation_id.is_empty()
                    || !seen.insert(conversation.conversation_id.clone())
                {
                    omitted_conversation_count = omitted_conversation_count.saturating_add(1);
                    continue;
                }
                let Some(scope) = self
                    .policy
                    .conversation_scopes
                    .get(&conversation.conversation_id)
                else {
                    continue;
                };
                if !scope
                    .capabilities
                    .contains(&ToolCapability::ListConversations)
                    || (destination == ToolDataDestination::RemoteModel
                        && !scope.allow_remote_model)
                {
                    continue;
                }
                conversations.push(ToolConversationView {
                    conversation_id: conversation.conversation_id,
                    kind: conversation.kind,
                    participant_count: conversation.participant_ids.len(),
                    entity_decode_state: conversation.entity_decode_state,
                    capabilities: scope.capabilities.clone(),
                    message_fields: scope.message_fields.clone(),
                    not_before_unix: scope.not_before_unix,
                    not_after_unix: scope.not_after_unix,
                });
            }
        }
        if omitted_conversation_count > 0 {
            limitation_codes.insert("malformedArchiveConversationOmitted".to_string());
        }
        conversations.sort_by(|left, right| left.conversation_id.cmp(&right.conversation_id));
        self.append_audit(
            ToolOperation::ListConversations,
            None,
            destination,
            ToolAuditOutcome::Completed,
            conversations.len(),
            0,
            0,
        )?;
        Ok(ToolConversationList {
            destination,
            conversations,
            omitted_conversation_count,
            limitation_codes: limitation_codes.into_iter().collect(),
        })
    }

    pub fn read_recent_messages(
        &self,
        conversation_id: &str,
        requested_limit: usize,
        destination: ToolDataDestination,
    ) -> Result<ToolMessageResult, RestoreError> {
        let scope = self.authorize(
            conversation_id,
            ToolCapability::ReadRecentMessages,
            ToolOperation::ReadRecentMessages,
            destination,
        )?;
        let limit = requested_limit
            .clamp(1, self.policy.maximum_result_count)
            .min(MAX_TOOL_RESULTS);
        let mut recent = VecDeque::with_capacity(limit);
        let mut seen = HashSet::new();
        let mut omitted_message_count = 0_u64;
        let mut limitation_codes = BTreeSet::new();
        let message_path = self.archive_directory.join("messages.ndjson");
        let reader = if private_regular_file_exists(&message_path)? {
            Some(self.message_reader()?)
        } else {
            limitation_codes.insert("archiveMessageLedgerUnavailable".to_string());
            None
        };
        for message in reader.into_iter().flatten() {
            let message = match message {
                Ok(message) => message,
                Err(_) => {
                    omitted_message_count = omitted_message_count.saturating_add(1);
                    continue;
                }
            };
            if message.account_id != self.policy.account_id
                || message.canonical_id.is_empty()
                || message.conversation_id.is_empty()
                || !seen.insert(message.canonical_id.clone())
            {
                omitted_message_count = omitted_message_count.saturating_add(1);
                continue;
            }
            if message.conversation_id != conversation_id || !scope.includes_message(&message) {
                continue;
            }
            if recent.len() == limit {
                recent.pop_front();
            }
            recent.push_back(minimize_message(
                message,
                self.policy.maximum_message_summary_bytes,
                &scope.message_fields,
                &self.preserved_stale_source_set_ids,
            ));
        }
        if omitted_message_count > 0 {
            limitation_codes.insert("malformedArchiveMessageOmitted".to_string());
        }
        let messages = recent.into_iter().collect::<Vec<_>>();
        let released_bytes = released_body_bytes(&messages);
        self.append_audit(
            ToolOperation::ReadRecentMessages,
            Some(conversation_id),
            destination,
            ToolAuditOutcome::Completed,
            messages.len(),
            released_bytes,
            0,
        )?;
        Ok(ToolMessageResult {
            destination,
            searched_conversation_count: 1,
            messages,
            restoration_completion: self.restoration_completion.clone(),
            omitted_message_count,
            limitation_codes: limitation_codes.into_iter().collect(),
        })
    }

    pub fn search_messages(
        &self,
        query: &str,
        conversation_id: Option<&str>,
        requested_limit: usize,
        destination: ToolDataDestination,
    ) -> Result<ToolMessageResult, RestoreError> {
        if query.is_empty() || query.len() > MAX_SEARCH_QUERY_BYTES {
            self.append_audit(
                ToolOperation::SearchMessages,
                conversation_id,
                destination,
                ToolAuditOutcome::Denied,
                0,
                0,
                query.len(),
            )?;
            return Err(RestoreError::Integrity(format!(
                "search query must be between 1 and {MAX_SEARCH_QUERY_BYTES} bytes"
            )));
        }
        let searchable = if let Some(conversation_id) = conversation_id {
            self.authorize(
                conversation_id,
                ToolCapability::SearchMessages,
                ToolOperation::SearchMessages,
                destination,
            )?;
            BTreeSet::from([conversation_id.to_string()])
        } else {
            let values = self
                .policy
                .conversation_scopes
                .iter()
                .filter(|(_, scope)| {
                    scope.capabilities.contains(&ToolCapability::SearchMessages)
                        && (destination == ToolDataDestination::LocalModel
                            || scope.allow_remote_model)
                })
                .map(|(identifier, _)| identifier.clone())
                .collect::<BTreeSet<_>>();
            if values.is_empty() {
                self.append_audit(
                    ToolOperation::SearchMessages,
                    None,
                    destination,
                    ToolAuditOutcome::Denied,
                    0,
                    0,
                    0,
                )?;
                return Err(RestoreError::Integrity(
                    "no conversation permits message search for this destination".to_string(),
                ));
            }
            values
        };
        let limit = requested_limit
            .clamp(1, self.policy.maximum_result_count)
            .min(MAX_TOOL_RESULTS);
        let query = query.to_lowercase();
        let mut messages = Vec::new();
        let mut seen = HashSet::new();
        let mut omitted_message_count = 0_u64;
        let mut limitation_codes = BTreeSet::new();
        let message_path = self.archive_directory.join("messages.ndjson");
        let reader = if private_regular_file_exists(&message_path)? {
            Some(self.message_reader()?)
        } else {
            limitation_codes.insert("archiveMessageLedgerUnavailable".to_string());
            None
        };
        for message in reader.into_iter().flatten() {
            let message = match message {
                Ok(message) => message,
                Err(_) => {
                    omitted_message_count = omitted_message_count.saturating_add(1);
                    continue;
                }
            };
            if message.account_id != self.policy.account_id
                || message.canonical_id.is_empty()
                || message.conversation_id.is_empty()
                || !seen.insert(message.canonical_id.clone())
            {
                omitted_message_count = omitted_message_count.saturating_add(1);
                continue;
            }
            if !searchable.contains(&message.conversation_id) {
                continue;
            }
            let scope = self
                .policy
                .conversation_scopes
                .get(&message.conversation_id)
                .expect("searchable conversations originate in the policy");
            if !scope.includes_message(&message) {
                continue;
            }
            let minimized = minimize_message(
                message,
                self.policy.maximum_message_summary_bytes,
                &scope.message_fields,
                &self.preserved_stale_source_set_ids,
            );
            if minimized
                .payload_summary
                .as_deref()
                .is_some_and(|summary| summary.to_lowercase().contains(&query))
            {
                messages.push(minimized);
                if messages.len() == limit {
                    break;
                }
            }
        }
        if omitted_message_count > 0 {
            limitation_codes.insert("malformedArchiveMessageOmitted".to_string());
        }
        let released_bytes = released_body_bytes(&messages);
        self.append_audit(
            ToolOperation::SearchMessages,
            conversation_id,
            destination,
            ToolAuditOutcome::Completed,
            messages.len(),
            released_bytes,
            0,
        )?;
        Ok(ToolMessageResult {
            destination,
            searched_conversation_count: searchable.len(),
            messages,
            restoration_completion: self.restoration_completion.clone(),
            omitted_message_count,
            limitation_codes: limitation_codes.into_iter().collect(),
        })
    }

    pub fn create_draft(
        &self,
        conversation_id: &str,
        body: &str,
        draft_directory: &Path,
    ) -> Result<DraftReceipt, RestoreError> {
        self.authorize(
            conversation_id,
            ToolCapability::CreateDraft,
            ToolOperation::CreateDraft,
            ToolDataDestination::LocalModel,
        )?;
        if body.is_empty() || body.len() > self.policy.maximum_draft_bytes {
            self.append_audit(
                ToolOperation::CreateDraft,
                Some(conversation_id),
                ToolDataDestination::LocalModel,
                ToolAuditOutcome::Denied,
                0,
                0,
                body.len(),
            )?;
            return Err(RestoreError::Integrity(format!(
                "draft body must be between 1 and {} bytes",
                self.policy.maximum_draft_bytes
            )));
        }
        ensure_private_directory(draft_directory)?;
        let observed_at = unix_nanoseconds()?;
        let mut identity = Sha256::new();
        identity.update(self.policy.account_id.as_bytes());
        identity.update(conversation_id.as_bytes());
        identity.update(observed_at.to_le_bytes());
        identity.update(std::process::id().to_le_bytes());
        identity.update(body.as_bytes());
        let draft_id = hex::encode(identity.finalize());
        let record = DraftRecord {
            format_version: 1,
            draft_id: &draft_id,
            account_id: &self.policy.account_id,
            conversation_id,
            created_at_unix_nanoseconds: observed_at,
            state: DraftState::DraftOnly,
            body,
        };
        let path = draft_directory.join(format!("{draft_id}.json"));
        write_owner_only_json(&path, &record)?;
        if let Err(error) = self.append_audit(
            ToolOperation::CreateDraft,
            Some(conversation_id),
            ToolDataDestination::LocalModel,
            ToolAuditOutcome::Completed,
            1,
            0,
            body.len(),
        ) {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        Ok(DraftReceipt {
            draft_id,
            conversation_id: conversation_id.to_string(),
            created_at_unix_nanoseconds: observed_at,
            state: DraftState::DraftOnly,
            body_byte_count: body.len(),
        })
    }

    fn message_reader(
        &self,
    ) -> Result<impl Iterator<Item = Result<CanonicalMessage, RestoreError>>, RestoreError> {
        let path = self.archive_directory.join("messages.ndjson");
        ensure_private_regular_file(&path)?;
        let lines = BufReader::new(File::open(path)?).lines();
        Ok(lines.map(|line| {
            let line = line?;
            Ok(serde_json::from_str(&line)?)
        }))
    }

    fn authorize(
        &self,
        conversation_id: &str,
        capability: ToolCapability,
        operation: ToolOperation,
        destination: ToolDataDestination,
    ) -> Result<&ConversationToolScope, RestoreError> {
        let scope = self.policy.conversation_scopes.get(conversation_id);
        let allowed = scope.is_some_and(|scope| {
            scope.capabilities.contains(&capability)
                && (destination == ToolDataDestination::LocalModel || scope.allow_remote_model)
        });
        if let (true, Some(scope)) = (allowed, scope) {
            return Ok(scope);
        }
        self.append_audit(
            operation,
            Some(conversation_id),
            destination,
            ToolAuditOutcome::Denied,
            0,
            0,
            0,
        )?;
        Err(RestoreError::Integrity(
            "tool operation is outside the authorized conversation scope or destination"
                .to_string(),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn append_audit(
        &self,
        operation: ToolOperation,
        conversation_id: Option<&str>,
        destination: ToolDataDestination,
        outcome: ToolAuditOutcome,
        returned_item_count: usize,
        released_body_byte_count: usize,
        request_body_byte_count: usize,
    ) -> Result<(), RestoreError> {
        let observed_at = unix_nanoseconds()?;
        let identity = format!(
            "{}:{}:{observed_at}:{operation:?}:{conversation_id:?}:{destination:?}:{outcome:?}:{}",
            self.policy.account_id,
            self.requester_id,
            std::process::id()
        );
        let event = ToolAuditEvent {
            format_version: 1,
            event_id: hex::encode(Sha256::digest(identity.as_bytes())),
            observed_at_unix_nanoseconds: observed_at,
            account_id: self.policy.account_id.clone(),
            requester_id: self.requester_id.clone(),
            operation,
            conversation_id: conversation_id.map(str::to_string),
            destination,
            outcome,
            returned_item_count,
            released_body_byte_count,
            request_body_byte_count,
        };
        append_owner_only_json_line(&self.audit_path, &event)
    }
}

pub(crate) fn load_tool_policy(path: &Path) -> Result<ToolAuthorizationPolicy, RestoreError> {
    ensure_private_regular_file(path)?;
    let policy: ToolAuthorizationPolicy = serde_json::from_slice(&fs::read(path)?)?;
    if !matches!(policy.format_version, 2 | 3)
        || policy.account_id.is_empty()
        || policy.maximum_result_count == 0
        || policy.maximum_result_count > MAX_TOOL_RESULTS
        || policy.maximum_message_summary_bytes == 0
        || policy.maximum_message_summary_bytes > MAX_MESSAGE_SUMMARY_BYTES
        || policy.maximum_draft_bytes == 0
        || policy.maximum_draft_bytes > MAX_DRAFT_BYTES
    {
        return Err(RestoreError::Integrity(
            "unsupported or unsafe tool policy".to_string(),
        ));
    }
    validate_tool_scopes(
        &policy.conversation_scopes,
        policy.cached_moments_scope.as_ref(),
    )?;
    Ok(policy)
}

fn validate_tool_scopes(
    conversation_scopes: &BTreeMap<String, ConversationToolScope>,
    cached_moments_scope: Option<&CachedMomentsToolScope>,
) -> Result<(), RestoreError> {
    if conversation_scopes.is_empty() && cached_moments_scope.is_none() {
        return Err(RestoreError::Integrity(
            "at least one conversation or cached-moments scope must be explicitly enabled"
                .to_string(),
        ));
    }
    if let Some((conversation_id, _)) = conversation_scopes
        .iter()
        .find(|(identifier, scope)| identifier.is_empty() || scope.capabilities.is_empty())
    {
        return Err(RestoreError::Integrity(format!(
            "conversation tool scope has no identity or capabilities: {conversation_id}"
        )));
    }
    if let Some((conversation_id, _)) = conversation_scopes.iter().find(|(_, scope)| {
        scope
            .not_before_unix
            .zip(scope.not_after_unix)
            .is_some_and(|(start, end)| start > end)
    }) {
        return Err(RestoreError::Integrity(format!(
            "conversation tool scope has an inverted time range: {conversation_id}"
        )));
    }
    if let Some((conversation_id, _)) = conversation_scopes.iter().find(|(_, scope)| {
        (scope
            .capabilities
            .contains(&ToolCapability::ReadRecentMessages)
            || scope.capabilities.contains(&ToolCapability::SearchMessages))
            && scope.message_fields.is_empty()
    }) {
        return Err(RestoreError::Integrity(format!(
            "message read/search scope has no enabled fields: {conversation_id}"
        )));
    }
    if let Some((conversation_id, _)) = conversation_scopes.iter().find(|(_, scope)| {
        scope.capabilities.contains(&ToolCapability::SearchMessages)
            && !scope.message_fields.contains(&ToolMessageField::Content)
    }) {
        return Err(RestoreError::Integrity(format!(
            "message search requires the content field: {conversation_id}"
        )));
    }
    if let Some(scope) = cached_moments_scope {
        if scope.fields.is_empty() {
            return Err(RestoreError::Integrity(
                "cached-moments scope has no enabled fields".to_string(),
            ));
        }
        if scope
            .not_before_unix
            .zip(scope.not_after_unix)
            .is_some_and(|(start, end)| start > end)
        {
            return Err(RestoreError::Integrity(
                "cached-moments scope has an inverted time range".to_string(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn minimize_message(
    message: CanonicalMessage,
    maximum_summary_bytes: usize,
    fields: &BTreeSet<ToolMessageField>,
    preserved_stale_source_set_ids: &BTreeSet<String>,
) -> MinimizedMessage {
    let (payload_kind, payload_summary, payload_summary_truncated) =
        if fields.contains(&ToolMessageField::Content) {
            let (kind, summary, truncated) =
                summarize_payload(&message.typed_payload, maximum_summary_bytes);
            (Some(kind), summary, Some(truncated))
        } else {
            (None, None, None)
        };
    MinimizedMessage {
        canonical_id: message.canonical_id,
        conversation_id: message.conversation_id,
        source_database_freshness: if preserved_stale_source_set_ids
            .contains(&message.source_set_id)
        {
            ToolSourceDatabaseFreshness::PreservedStale
        } else {
            ToolSourceDatabaseFreshness::Fresh
        },
        sender_id: fields
            .contains(&ToolMessageField::Sender)
            .then_some(message.sender_id)
            .flatten(),
        created_at_unix: fields
            .contains(&ToolMessageField::CreatedAt)
            .then_some(message.created_at_unix)
            .flatten(),
        conversation_ordinal: message.conversation_ordinal,
        direction: fields
            .contains(&ToolMessageField::Direction)
            .then_some(message.direction),
        logical_type: fields
            .contains(&ToolMessageField::MessageType)
            .then_some(message.logical_type)
            .flatten(),
        sub_type: fields
            .contains(&ToolMessageField::MessageType)
            .then_some(message.sub_type)
            .flatten(),
        payload_kind,
        payload_summary,
        payload_summary_truncated,
        artifact_references: if fields.contains(&ToolMessageField::Attachments) {
            message
                .artifact_references
                .into_iter()
                .map(|reference| ToolArtifactReference {
                    artifact_id: reference.artifact_id,
                    role: reference.role,
                    preferred: reference.preferred,
                })
                .collect()
        } else {
            Vec::new()
        },
        relationships: if fields.contains(&ToolMessageField::Relationships) {
            message
                .relationships
                .into_iter()
                .map(|relationship| ToolRelationshipReference {
                    kind: relationship.kind,
                    target_canonical_id: relationship.target_canonical_id,
                    resolved: relationship.resolved,
                })
                .collect()
        } else {
            Vec::new()
        },
    }
}

pub(crate) fn minimize_cached_moment(
    moment: CanonicalCachedMoment,
    maximum_text_bytes: usize,
    fields: &BTreeSet<CachedMomentField>,
    preserved_stale_source_set_ids: &BTreeSet<String>,
) -> Result<MinimizedCachedMoment, RestoreError> {
    let mut remaining = maximum_text_bytes;
    let mut text_truncated = false;
    let content_description = decode_cached_text_field(
        fields
            .contains(&CachedMomentField::ContentDescription)
            .then_some(moment.content_description_base64)
            .flatten(),
        &mut remaining,
        &mut text_truncated,
    )?;
    let title = decode_cached_text_field(
        fields
            .contains(&CachedMomentField::Title)
            .then_some(moment.title_base64)
            .flatten(),
        &mut remaining,
        &mut text_truncated,
    )?;
    let description = decode_cached_text_field(
        fields
            .contains(&CachedMomentField::Description)
            .then_some(moment.description_base64)
            .flatten(),
        &mut remaining,
        &mut text_truncated,
    )?;
    let content_url = decode_cached_text_field(
        fields
            .contains(&CachedMomentField::ContentUrl)
            .then_some(moment.content_url_base64)
            .flatten(),
        &mut remaining,
        &mut text_truncated,
    )?;
    Ok(MinimizedCachedMoment {
        canonical_id: moment.canonical_id,
        source_database_freshness: if preserved_stale_source_set_ids.contains(&moment.source_set_id)
        {
            ToolSourceDatabaseFreshness::PreservedStale
        } else {
            ToolSourceDatabaseFreshness::Fresh
        },
        author_id: fields
            .contains(&CachedMomentField::Author)
            .then_some(moment.author_id)
            .flatten(),
        created_at_unix: fields
            .contains(&CachedMomentField::CreatedAt)
            .then_some(moment.created_at_unix)
            .flatten(),
        content_type: fields
            .contains(&CachedMomentField::ContentType)
            .then_some(moment.content_type)
            .flatten(),
        content_description,
        title,
        description,
        content_url,
        media_count: fields
            .contains(&CachedMomentField::MediaCount)
            .then_some(moment.media_count),
        like_count: fields
            .contains(&CachedMomentField::LikeCount)
            .then_some(moment.like_count),
        comment_count: fields
            .contains(&CachedMomentField::CommentCount)
            .then_some(moment.comment_count),
        text_truncated,
    })
}

fn decode_cached_text_field(
    value: Option<String>,
    remaining: &mut usize,
    truncated: &mut bool,
) -> Result<Option<String>, RestoreError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| {
            RestoreError::Integrity("cached-moment minimized text is not valid base64".to_string())
        })?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let (text, was_truncated) = truncate_utf8(text, *remaining);
    *remaining = remaining.saturating_sub(text.len());
    *truncated |= was_truncated;
    Ok(Some(text))
}

fn summarize_payload(
    payload: &TypedPayload,
    maximum_bytes: usize,
) -> (String, Option<String>, bool) {
    let TypedPayload::Decoded(value) = payload else {
        return ("unknown".to_string(), None, false);
    };
    let Some((kind, content)) = value.as_object().and_then(|object| object.iter().next()) else {
        return ("decoded".to_string(), None, false);
    };
    let summary = if let Some(value) = content.as_str() {
        Some(value.to_string())
    } else if let Some(fields) = content.as_object() {
        const ALLOWED_FIELDS: &[&str] = &[
            "sub_type",
            "title",
            "des",
            "url",
            "file_ext",
            "file_size",
            "reply_text",
            "refer_sender",
            "refer_content",
            "refer_type",
            "amount_desc",
            "pay_memo",
            "pay_sub_type",
        ];
        let values = ALLOWED_FIELDS
            .iter()
            .filter_map(|name| {
                fields.get(*name).and_then(|value| match value {
                    serde_json::Value::String(value) => Some(format!("{name}={value}")),
                    serde_json::Value::Number(value) => Some(format!("{name}={value}")),
                    serde_json::Value::Bool(value) => Some(format!("{name}={value}")),
                    _ => None,
                })
            })
            .collect::<Vec<_>>();
        (!values.is_empty()).then(|| values.join("; "))
    } else {
        None
    };
    let Some(summary) = summary else {
        return (kind.clone(), None, false);
    };
    let (summary, truncated) = truncate_utf8(summary, maximum_bytes);
    (kind.clone(), Some(summary), truncated)
}

fn truncate_utf8(mut value: String, maximum_bytes: usize) -> (String, bool) {
    if value.len() <= maximum_bytes {
        return (value, false);
    }
    let mut boundary = maximum_bytes;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    (value, true)
}

pub(crate) fn released_body_bytes(messages: &[MinimizedMessage]) -> usize {
    messages
        .iter()
        .filter_map(|message| message.payload_summary.as_ref())
        .map(String::len)
        .sum()
}

pub(crate) fn released_cached_moment_body_bytes(moments: &[MinimizedCachedMoment]) -> usize {
    moments
        .iter()
        .map(|moment| {
            [
                moment.content_description.as_ref(),
                moment.title.as_ref(),
                moment.description.as_ref(),
                moment.content_url.as_ref(),
            ]
            .into_iter()
            .flatten()
            .map(String::len)
            .sum::<usize>()
        })
        .sum()
}

impl ConversationToolScope {
    pub(crate) fn includes_message(&self, message: &CanonicalMessage) -> bool {
        match message.created_at_unix {
            Some(created_at) => {
                !self
                    .not_before_unix
                    .is_some_and(|not_before| created_at < not_before)
                    && !self
                        .not_after_unix
                        .is_some_and(|not_after| created_at > not_after)
            }
            None => self.not_before_unix.is_none() && self.not_after_unix.is_none(),
        }
    }
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
    Ok(())
}

fn append_owner_only_json_line(path: &Path, value: &impl Serialize) -> Result<(), RestoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath("audit log has no parent".to_string()))?;
    ensure_private_directory(parent)?;
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 || metadata.nlink() != 1 {
        return Err(RestoreError::Integrity(
            "audit log must be an owner-only regular file with one link".to_string(),
        ));
    }
    let descriptor = std::os::fd::AsRawFd::as_raw_fd(&file);
    if unsafe { libc::flock(descriptor, libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let write_result = (|| -> Result<(), RestoreError> {
        serde_json::to_writer(&mut file, value)?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.sync_data()?;
        Ok(())
    })();
    let unlock_result = unsafe { libc::flock(descriptor, libc::LOCK_UN) };
    write_result?;
    if unlock_result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
