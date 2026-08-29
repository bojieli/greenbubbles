use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::archive::{ensure_private_directory, ensure_private_regular_file};
use crate::connector::{
    append_owner_only_connector_event, audit_connector_log_for_account, connector_response,
    decode_connector_conversation_cursor, encode_connector_conversation_cursor, invalid,
    unauthorized, unavailable, CapabilityState, ConnectorAuditEvent, ConnectorAuditOutcome,
    ConnectorAuditStage, ConnectorCapabilities, ConnectorConversationList,
    ConnectorConversationView, ConnectorDestination, ConnectorErrorBody, ConnectorOperation,
    ConnectorRequest, ConnectorRequestHandler, ConnectorResponse, ConnectorResult,
    DirectConnectorStatus, CONNECTOR_API_VERSION, CONNECTOR_VERSION,
};
use crate::live_query::{
    find_conversation, find_conversations, get_message as get_live_message,
    get_search_result_message, list_messages_in_time_range, search_messages_in_time_range,
    ConversationItem, LiveQueryError, LiveQuerySource, MessageItem, QueryWarning, SearchItem,
    MAX_PAGE_LIMIT, MAX_SEARCH_LIMIT,
};
use crate::tools::{
    load_tool_policy, released_body_bytes, summarize_decoded_payload, ConversationToolScope,
    MinimizedMessage, ToolArtifactReference, ToolAuthorizationPolicy, ToolCapability,
    ToolDataDestination, ToolMessageField, ToolRelationshipReference, ToolSourceDatabaseFreshness,
};
use crate::{ConversationKind, EntityDecodeState, RestoreError};

const DIRECT_CONNECTOR_FORMAT_VERSION: u32 = 1;
const MAX_DIRECT_CURSOR_BYTES: usize = 16 * 1024;
const MAX_CROSS_SEARCH_CONVERSATIONS_PER_PAGE: usize = 32;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DirectReadCursor {
    version: u32,
    kind: String,
    source_identity: String,
    policy_sha256: String,
    destination: ConnectorDestination,
    conversation_id: String,
    query_sha256: Option<String>,
    inner_cursor: Option<String>,
}

pub struct DirectConnectorService<'a> {
    source: LiveQuerySource<'a>,
    policy: ToolAuthorizationPolicy,
    policy_sha256: String,
    audit_path: std::path::PathBuf,
}

impl<'a> DirectConnectorService<'a> {
    pub fn open(
        source: LiveQuerySource<'a>,
        policy_path: &Path,
        audit_path: &Path,
    ) -> Result<Self, RestoreError> {
        let policy = load_tool_policy(policy_path)?;
        if policy.account_id != source.identity()
            || policy.created_from_source_fingerprint != source.identity()
        {
            return Err(RestoreError::Integrity(
                "direct connector policy belongs to a different SQLite source".to_string(),
            ));
        }
        if policy.cached_moments_scope.is_some()
            || policy
                .conversation_scopes
                .values()
                .any(|scope| scope.capabilities.contains(&ToolCapability::CreateDraft))
        {
            return Err(RestoreError::Integrity(
                "direct connector policy may authorize only ordinary read operations".to_string(),
            ));
        }
        let first_conversation =
            policy.conversation_scopes.keys().next().ok_or_else(|| {
                RestoreError::Integrity("direct connector policy is empty".into())
            })?;
        find_conversation(&source, first_conversation).map_err(direct_query_restore_error)?;

        let audit_parent = audit_path
            .parent()
            .ok_or_else(|| RestoreError::UnsafePath("audit path has no parent".to_string()))?;
        ensure_private_directory(audit_parent)?;
        if audit_path.try_exists()? {
            ensure_private_regular_file(audit_path)?;
            audit_connector_log_for_account(audit_path, Some(&policy.account_id))?;
        }
        Ok(Self {
            source,
            policy,
            policy_sha256: hex::encode(Sha256::digest(fs::read(policy_path)?)),
            audit_path: audit_path.to_path_buf(),
        })
    }

    pub fn handle(&self, request: ConnectorRequest) -> ConnectorResponse {
        let request_id = request.request_id.clone();
        connector_response(request_id, self.dispatch(&request))
    }

    fn dispatch(&self, request: &ConnectorRequest) -> Result<ConnectorResult, ConnectorErrorBody> {
        self.validate_request(request)?;
        match &request.operation {
            ConnectorOperation::Capabilities => {
                let result = self.capabilities();
                self.audit_metadata(request, "capabilities")?;
                Ok(ConnectorResult::Capabilities(result))
            }
            ConnectorOperation::Status => {
                let result = self.status();
                self.audit_metadata(request, "status")?;
                Ok(ConnectorResult::DirectStatus(result))
            }
            ConnectorOperation::ListConversations { cursor, limit } => self
                .list_conversations(request, cursor.as_deref(), limit.unwrap_or(100))
                .map(ConnectorResult::Conversations),
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
            ConnectorOperation::GetMessage { canonical_id } => self
                .get_message(request, canonical_id)
                .map(ConnectorResult::Message),
            _ => {
                let _ = self.audit(
                    request,
                    operation_name(&request.operation),
                    operation_conversation(&request.operation),
                    ConnectorAuditOutcome::Denied,
                    0,
                    0,
                    0,
                );
                Err(unavailable(
                    "replicaOnlyOperation",
                    "this operation remains on the encrypted replica connector; the direct connector serves only ordinary SQLite reads",
                ))
            }
        }
    }

    fn validate_request(&self, request: &ConnectorRequest) -> Result<(), ConnectorErrorBody> {
        if request.api_version != CONNECTOR_API_VERSION {
            return Err(invalid("unsupported connector API version"));
        }
        if request.request_id.is_empty() || request.request_id.len() > 256 {
            return Err(invalid("request ID must be between 1 and 256 bytes"));
        }
        if request.requester_id.is_empty() || request.requester_id.len() > 256 {
            return Err(invalid("requester ID must be between 1 and 256 bytes"));
        }
        Ok(())
    }

    fn capabilities(&self) -> ConnectorCapabilities {
        let read = CapabilityState {
            available: true,
            enabled: true,
            reason_code: "directReadOnlySqlite".to_string(),
            reason: "Bounded policy-scoped reads use the selected WeChat SQLite source directly"
                .to_string(),
        };
        let replica_only = CapabilityState {
            available: false,
            enabled: false,
            reason_code: "replicaOnlyOperation".to_string(),
            reason: "This surface is intentionally retained on the encrypted replica connector"
                .to_string(),
        };
        let mut operations = BTreeMap::new();
        for operation in [
            "capabilities",
            "status",
            "listConversations",
            "searchMessages",
            "getMessages",
            "getMessage",
        ] {
            operations.insert(operation.to_string(), read.clone());
        }
        for operation in [
            "coverage",
            "getChanges",
            "getCachedMoments",
            "getArtifact",
            "resolveContact",
            "resolveConversation",
            "createMessageDraft",
            "createReplyDraft",
            "createAttachmentDraft",
            "previewAction",
            "bootstrap",
            "synchronize",
            "refresh",
        ] {
            operations.insert(operation.to_string(), replica_only.clone());
        }
        ConnectorCapabilities {
            format_version: DIRECT_CONNECTOR_FORMAT_VERSION,
            api_version: CONNECTOR_API_VERSION.to_string(),
            connector_version: CONNECTOR_VERSION.to_string(),
            account_id: Some(self.policy.account_id.clone()),
            self_participant_id: None,
            passive_read: read,
            cached_moments_read: replica_only.clone(),
            authenticated_active_read: replica_only.clone(),
            draft: replica_only.clone(),
            text_send: replica_only.clone(),
            reply_send: replica_only.clone(),
            file_send: replica_only,
            operations,
            enabled_conversation_count: self.policy.conversation_scopes.len(),
            local_only: true,
        }
    }

    fn status(&self) -> DirectConnectorStatus {
        let locally_enabled_operation_count = self
            .policy
            .conversation_scopes
            .values()
            .map(|scope| {
                scope
                    .capabilities
                    .iter()
                    .filter(|capability| !matches!(capability, ToolCapability::CreateDraft))
                    .count()
            })
            .sum();
        DirectConnectorStatus {
            format_version: DIRECT_CONNECTOR_FORMAT_VERSION,
            api_version: CONNECTOR_API_VERSION.to_string(),
            connector_version: CONNECTOR_VERSION.to_string(),
            source_mode: self.source.mode(),
            source_identity: self.source.identity().to_string(),
            policy_created_from_source_fingerprint: self
                .policy
                .created_from_source_fingerprint
                .clone(),
            enabled_conversation_count: self.policy.conversation_scopes.len(),
            locally_enabled_operation_count,
            remotely_enabled_conversation_count: self
                .policy
                .conversation_scopes
                .values()
                .filter(|scope| scope.allow_remote_model)
                .count(),
            ordinary_reads_use_direct_sqlite: true,
        }
    }

    fn list_conversations(
        &self,
        request: &ConnectorRequest,
        cursor: Option<&str>,
        requested_limit: usize,
    ) -> Result<ConnectorConversationList, ConnectorErrorBody> {
        let destination = ToolDataDestination::from(request.destination);
        let after = decode_connector_conversation_cursor(
            cursor,
            self.source.identity(),
            &self.policy_sha256,
            request.destination,
        )?;
        let limit = requested_limit
            .clamp(1, self.policy.maximum_result_count)
            .min(MAX_PAGE_LIMIT);
        let mut selected = self
            .policy
            .conversation_scopes
            .iter()
            .filter(|(identifier, scope)| {
                scope
                    .capabilities
                    .contains(&ToolCapability::ListConversations)
                    && (destination != ToolDataDestination::RemoteModel || scope.allow_remote_model)
                    && after
                        .as_deref()
                        .is_none_or(|after| identifier.as_str() > after)
            })
            .take(limit.saturating_add(1))
            .map(|(identifier, scope)| (identifier.clone(), scope.clone()))
            .collect::<Vec<_>>();
        let has_more = selected.len() > limit;
        selected.truncate(limit);

        let mut conversations = Vec::with_capacity(selected.len());
        let mut limitation_codes =
            BTreeSet::from(["directConversationMembershipUnavailable".to_string()]);
        let selected_ids = selected
            .iter()
            .map(|(identifier, _)| identifier.clone())
            .collect::<Vec<_>>();
        let mut metadata = find_conversations(&self.source, &selected_ids).map_err(query_error)?;
        for (conversation_id, scope) in &selected {
            let item = metadata.remove(conversation_id);
            let (kind, participant_count) = direct_conversation_shape(conversation_id);
            let (entity_decode_state, human_label) = match item {
                Some(item) => {
                    let entity_decode_state = if item.display_name.is_some() {
                        EntityDecodeState::Complete
                    } else {
                        limitation_codes.insert("directContactDisplayNameUnavailable".to_string());
                        EntityDecodeState::RawOnly
                    };
                    (entity_decode_state, direct_conversation_label(&item))
                }
                None => {
                    limitation_codes
                        .insert("unavailableConversationMetadataSynthesized".to_string());
                    (EntityDecodeState::Failed, conversation_id.clone())
                }
            };
            conversations.push(ConnectorConversationView {
                conversation_id: conversation_id.clone(),
                kind,
                participant_count,
                entity_decode_state,
                source_database_freshness: ToolSourceDatabaseFreshness::Fresh,
                human_label,
                capabilities: scope.capabilities.clone(),
                message_fields: scope.message_fields.clone(),
                not_before_unix: scope.not_before_unix,
                not_after_unix: scope.not_after_unix,
            });
        }
        let next_cursor = if has_more {
            selected
                .last()
                .map(|(identifier, _)| {
                    encode_connector_conversation_cursor(
                        self.source.identity(),
                        &self.policy_sha256,
                        request.destination,
                        identifier,
                    )
                })
                .transpose()?
        } else {
            None
        };
        self.audit(
            request,
            "listConversations",
            None,
            ConnectorAuditOutcome::Completed,
            conversations.len(),
            0,
            0,
        )?;
        Ok(ConnectorConversationList {
            account_id: self.policy.account_id.clone(),
            conversations,
            next_cursor,
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
    ) -> Result<crate::connector::ConnectorMessagePage, ConnectorErrorBody> {
        let scope = self.authorize(
            request,
            conversation_id,
            ToolCapability::ReadRecentMessages,
            "getMessages",
        )?;
        let limit = requested_limit
            .clamp(1, self.policy.maximum_result_count)
            .min(MAX_PAGE_LIMIT);
        let inner_cursor = self
            .decode_read_cursor(
                cursor,
                "messages",
                request.destination,
                Some(conversation_id),
                None,
            )?
            .and_then(|cursor| cursor.inner_cursor);
        let page = list_messages_in_time_range(
            &self.source,
            conversation_id,
            limit,
            inner_cursor.as_deref(),
            scope.not_before_unix,
            scope.not_after_unix,
        )
        .map_err(query_error)?;
        let mut limitation_codes = warning_codes(&page.warnings);
        extend_direct_projection_limitations(scope, &mut limitation_codes);
        let messages = page
            .items
            .into_iter()
            .map(|message| self.project_message(message, scope))
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
        )?;
        Ok(crate::connector::ConnectorMessagePage {
            account_id: self.policy.account_id.clone(),
            source_fingerprint: self.source.identity().to_string(),
            messages,
            next_cursor: page
                .page
                .next_cursor
                .as_deref()
                .map(|inner| {
                    self.encode_read_cursor(
                        "messages",
                        request.destination,
                        conversation_id,
                        None,
                        Some(inner),
                    )
                })
                .transpose()?,
            omitted_message_count: omitted_warning_count(&page.warnings),
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
    ) -> Result<crate::connector::ConnectorMessagePage, ConnectorErrorBody> {
        let limit = requested_limit
            .clamp(1, self.policy.maximum_result_count)
            .min(MAX_SEARCH_LIMIT);
        let query_sha256 = direct_query_digest(query);
        if let Some(conversation_id) = conversation_id {
            let scope = self.authorize(
                request,
                conversation_id,
                ToolCapability::SearchMessages,
                "searchMessages",
            )?;
            let decoded = self.decode_read_cursor(
                cursor,
                "searchScoped",
                request.destination,
                Some(conversation_id),
                Some(&query_sha256),
            )?;
            let page = search_messages_in_time_range(
                &self.source,
                query,
                Some(conversation_id),
                limit,
                decoded
                    .as_ref()
                    .and_then(|cursor| cursor.inner_cursor.as_deref()),
                scope.not_before_unix,
                scope.not_after_unix,
            )
            .map_err(query_error)?;
            let mut limitation_codes = warning_codes(&page.warnings);
            extend_direct_projection_limitations(scope, &mut limitation_codes);
            let messages = page
                .items
                .into_iter()
                .map(|hit| self.project_search_hit(hit, scope))
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
            )?;
            return Ok(crate::connector::ConnectorMessagePage {
                account_id: self.policy.account_id.clone(),
                source_fingerprint: self.source.identity().to_string(),
                messages,
                next_cursor: page
                    .page
                    .next_cursor
                    .as_deref()
                    .map(|inner| {
                        self.encode_read_cursor(
                            "searchScoped",
                            request.destination,
                            conversation_id,
                            Some(&query_sha256),
                            Some(inner),
                        )
                    })
                    .transpose()?,
                omitted_message_count: omitted_warning_count(&page.warnings),
                limitation_codes: limitation_codes.into_iter().collect(),
            });
        }

        let destination = ToolDataDestination::from(request.destination);
        let searchable = self
            .policy
            .conversation_scopes
            .iter()
            .filter(|(_, scope)| {
                scope.capabilities.contains(&ToolCapability::SearchMessages)
                    && (destination != ToolDataDestination::RemoteModel || scope.allow_remote_model)
            })
            .collect::<Vec<_>>();
        if searchable.is_empty() {
            return Err(unauthorized(
                "no conversation permits message search for this destination",
            ));
        }
        let decoded = self.decode_read_cursor(
            cursor,
            "searchAcross",
            request.destination,
            None,
            Some(&query_sha256),
        )?;
        let mut conversation_index = if let Some(decoded) = &decoded {
            searchable
                .iter()
                .position(|(identifier, _)| *identifier == &decoded.conversation_id)
                .ok_or_else(|| {
                    invalid("search cursor conversation is not enabled by the current policy")
                })?
        } else {
            0
        };
        let mut inner_cursor = decoded.and_then(|cursor| cursor.inner_cursor);
        let mut messages = Vec::new();
        let mut limitation_codes =
            BTreeSet::from(["directCrossConversationSearchOrderedByConversation".to_string()]);
        let mut omitted_message_count = 0_u64;
        let mut next_cursor = None;
        let mut scanned_conversations = 0usize;
        while conversation_index < searchable.len()
            && messages.len() < limit
            && scanned_conversations < MAX_CROSS_SEARCH_CONVERSATIONS_PER_PAGE
        {
            let (identifier, scope) = searchable[conversation_index];
            let remaining = limit.saturating_sub(messages.len());
            let page = search_messages_in_time_range(
                &self.source,
                query,
                Some(identifier),
                remaining,
                inner_cursor.as_deref(),
                scope.not_before_unix,
                scope.not_after_unix,
            )
            .map_err(query_error)?;
            limitation_codes.extend(warning_codes(&page.warnings));
            extend_direct_projection_limitations(scope, &mut limitation_codes);
            omitted_message_count =
                omitted_message_count.saturating_add(omitted_warning_count(&page.warnings));
            messages.extend(
                page.items
                    .into_iter()
                    .map(|hit| self.project_search_hit(hit, scope)),
            );
            scanned_conversations = scanned_conversations.saturating_add(1);
            if let Some(native_next) = page.page.next_cursor {
                next_cursor = Some(self.encode_read_cursor(
                    "searchAcross",
                    request.destination,
                    identifier,
                    Some(&query_sha256),
                    Some(&native_next),
                )?);
                break;
            }
            conversation_index = conversation_index.saturating_add(1);
            inner_cursor = None;
            if conversation_index < searchable.len() {
                next_cursor = Some(self.encode_read_cursor(
                    "searchAcross",
                    request.destination,
                    searchable[conversation_index].0,
                    Some(&query_sha256),
                    None,
                )?);
            } else {
                next_cursor = None;
            }
        }
        let released = released_body_bytes(&messages);
        self.audit(
            request,
            "searchMessages",
            None,
            ConnectorAuditOutcome::Completed,
            messages.len(),
            released,
            query.len(),
        )?;
        Ok(crate::connector::ConnectorMessagePage {
            account_id: self.policy.account_id.clone(),
            source_fingerprint: self.source.identity().to_string(),
            messages,
            next_cursor,
            omitted_message_count,
            limitation_codes: limitation_codes.into_iter().collect(),
        })
    }

    fn get_message(
        &self,
        request: &ConnectorRequest,
        canonical_id: &str,
    ) -> Result<Option<MinimizedMessage>, ConnectorErrorBody> {
        let destination = ToolDataDestination::from(request.destination);
        for (conversation_id, scope) in &self.policy.conversation_scopes {
            if !scope
                .capabilities
                .contains(&ToolCapability::ReadRecentMessages)
                || (destination == ToolDataDestination::RemoteModel && !scope.allow_remote_model)
            {
                continue;
            }
            match get_live_message(&self.source, conversation_id, canonical_id) {
                Ok(resource) => {
                    if !includes_timestamp(scope, resource.item.created_at_unix) {
                        return Err(unauthorized("message is outside the authorized time range"));
                    }
                    let result = self.project_message(resource.item, scope);
                    let released = result
                        .payload_summary
                        .as_ref()
                        .map(String::len)
                        .unwrap_or_default();
                    self.audit(
                        request,
                        "getMessage",
                        Some(conversation_id),
                        ConnectorAuditOutcome::Completed,
                        1,
                        released,
                        0,
                    )?;
                    return Ok(Some(result));
                }
                Err(LiveQueryError::InvalidCursor(_)) => continue,
                Err(LiveQueryError::NotFound(_)) => return Ok(None),
                Err(error) => return Err(query_error(error)),
            }
        }
        for (conversation_id, scope) in &self.policy.conversation_scopes {
            if !scope.capabilities.contains(&ToolCapability::SearchMessages)
                || (destination == ToolDataDestination::RemoteModel && !scope.allow_remote_model)
            {
                continue;
            }
            let resource = match get_live_message(&self.source, conversation_id, canonical_id) {
                Ok(resource) => Ok(resource),
                Err(LiveQueryError::InvalidCursor(_)) => {
                    get_search_result_message(&self.source, conversation_id, canonical_id)
                }
                Err(error) => Err(error),
            };
            match resource {
                Ok(resource) => {
                    if !includes_timestamp(scope, resource.item.created_at_unix) {
                        return Err(unauthorized("message is outside the authorized time range"));
                    }
                    let result = self.project_message(resource.item, scope);
                    let released = result
                        .payload_summary
                        .as_ref()
                        .map(String::len)
                        .unwrap_or_default();
                    self.audit(
                        request,
                        "getMessage",
                        Some(conversation_id),
                        ConnectorAuditOutcome::Completed,
                        1,
                        released,
                        0,
                    )?;
                    return Ok(Some(result));
                }
                Err(LiveQueryError::InvalidCursor(_)) => continue,
                Err(LiveQueryError::NotFound(_)) => return Ok(None),
                Err(error) => return Err(query_error(error)),
            }
        }
        let _ = self.audit(
            request,
            "getMessage",
            None,
            ConnectorAuditOutcome::Denied,
            0,
            0,
            0,
        );
        Err(unauthorized(
            "message identity is outside the authorized conversation or destination scope",
        ))
    }

    fn project_message(
        &self,
        message: MessageItem,
        scope: &ConversationToolScope,
    ) -> MinimizedMessage {
        let (payload_kind, payload_summary, payload_summary_truncated) =
            if scope.message_fields.contains(&ToolMessageField::Content) {
                let (kind, summary, truncated) = summarize_decoded_payload(
                    &message.content,
                    self.policy.maximum_message_summary_bytes,
                );
                (
                    Some(kind),
                    summary,
                    Some(truncated || message.content_truncated),
                )
            } else {
                (None, None, None)
            };
        MinimizedMessage {
            canonical_id: message.id,
            conversation_id: message.conversation_id,
            source_database_freshness: ToolSourceDatabaseFreshness::Fresh,
            sender_id: scope
                .message_fields
                .contains(&ToolMessageField::Sender)
                .then_some(message.sender)
                .filter(|value| !value.is_empty()),
            sender_display_name: scope
                .message_fields
                .contains(&ToolMessageField::Sender)
                .then_some(message.sender_display_name)
                .flatten()
                .filter(|value| !value.is_empty()),
            created_at_unix: scope
                .message_fields
                .contains(&ToolMessageField::CreatedAt)
                .then_some(message.created_at_unix),
            conversation_ordinal: message.sort_sequence.max(0) as u64,
            direction: None,
            logical_type: scope
                .message_fields
                .contains(&ToolMessageField::MessageType)
                .then_some(message.message_type),
            sub_type: scope
                .message_fields
                .contains(&ToolMessageField::MessageType)
                .then_some(message.message_subtype),
            payload_kind,
            payload_summary,
            payload_summary_truncated,
            artifact_references: Vec::<ToolArtifactReference>::new(),
            relationships: Vec::<ToolRelationshipReference>::new(),
            omitted_artifact_reference_count: 0,
            omitted_relationship_reference_count: 0,
        }
    }

    fn project_search_hit(
        &self,
        hit: SearchItem,
        scope: &ConversationToolScope,
    ) -> MinimizedMessage {
        let content_enabled = scope.message_fields.contains(&ToolMessageField::Content);
        let (snippet, snippet_truncated) =
            truncate_utf8(hit.snippet, self.policy.maximum_message_summary_bytes);
        MinimizedMessage {
            canonical_id: hit.id,
            conversation_id: hit.conversation_id,
            source_database_freshness: ToolSourceDatabaseFreshness::Fresh,
            sender_id: scope
                .message_fields
                .contains(&ToolMessageField::Sender)
                .then_some(hit.sender)
                .filter(|value| !value.is_empty()),
            sender_display_name: scope
                .message_fields
                .contains(&ToolMessageField::Sender)
                .then_some(hit.sender_display_name)
                .flatten()
                .filter(|value| !value.is_empty()),
            created_at_unix: scope
                .message_fields
                .contains(&ToolMessageField::CreatedAt)
                .then_some(hit.created_at_unix),
            conversation_ordinal: hit.sort_sequence.max(0) as u64,
            direction: None,
            logical_type: scope
                .message_fields
                .contains(&ToolMessageField::MessageType)
                .then_some(hit.message_type),
            sub_type: scope
                .message_fields
                .contains(&ToolMessageField::MessageType)
                .then_some(hit.message_subtype),
            payload_kind: content_enabled.then(|| "searchSnippet".to_string()),
            payload_summary: content_enabled.then_some(snippet),
            payload_summary_truncated: content_enabled
                .then_some(hit.snippet_truncated || snippet_truncated),
            artifact_references: Vec::new(),
            relationships: Vec::new(),
            omitted_artifact_reference_count: 0,
            omitted_relationship_reference_count: 0,
        }
    }

    fn encode_read_cursor(
        &self,
        kind: &str,
        destination: ConnectorDestination,
        conversation_id: &str,
        query_sha256: Option<&str>,
        inner_cursor: Option<&str>,
    ) -> Result<String, ConnectorErrorBody> {
        let bytes = serde_json::to_vec(&DirectReadCursor {
            version: 1,
            kind: kind.to_string(),
            source_identity: self.source.identity().to_string(),
            policy_sha256: self.policy_sha256.clone(),
            destination,
            conversation_id: conversation_id.to_string(),
            query_sha256: query_sha256.map(str::to_string),
            inner_cursor: inner_cursor.map(str::to_string),
        })
        .map_err(|_| invalid("direct connector cursor could not be encoded"))?;
        if bytes.len() > MAX_DIRECT_CURSOR_BYTES {
            return Err(invalid("direct connector cursor exceeds its safety limit"));
        }
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
    }

    fn decode_read_cursor(
        &self,
        value: Option<&str>,
        kind: &str,
        destination: ConnectorDestination,
        expected_conversation_id: Option<&str>,
        query_sha256: Option<&str>,
    ) -> Result<Option<DirectReadCursor>, ConnectorErrorBody> {
        let Some(value) = value else {
            return Ok(None);
        };
        if value.is_empty() || value.len() > MAX_DIRECT_CURSOR_BYTES * 2 {
            return Err(invalid("direct connector cursor is outside safe limits"));
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| invalid("direct connector cursor is not valid base64url"))?;
        if bytes.len() > MAX_DIRECT_CURSOR_BYTES {
            return Err(invalid("direct connector cursor is outside safe limits"));
        }
        let cursor: DirectReadCursor = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("direct connector cursor structure is invalid"))?;
        if cursor.version != 1
            || cursor.kind != kind
            || cursor.source_identity != self.source.identity()
            || cursor.policy_sha256 != self.policy_sha256
            || cursor.destination != destination
            || cursor.conversation_id.is_empty()
            || cursor.conversation_id.len() > 4_096
            || cursor.query_sha256.as_deref() != query_sha256
            || expected_conversation_id.is_some_and(|expected| cursor.conversation_id != expected)
            || cursor
                .inner_cursor
                .as_ref()
                .is_some_and(|inner| inner.is_empty() || inner.len() > 8_192)
        {
            return Err(invalid(
                "direct connector cursor does not belong to this source, policy, destination, and query",
            ));
        }
        Ok(Some(cursor))
    }

    fn authorize(
        &self,
        request: &ConnectorRequest,
        conversation_id: &str,
        capability: ToolCapability,
        operation: &str,
    ) -> Result<&ConversationToolScope, ConnectorErrorBody> {
        let destination = ToolDataDestination::from(request.destination);
        if let Some(scope) = self
            .policy
            .conversation_scopes
            .get(conversation_id)
            .filter(|scope| {
                scope.capabilities.contains(&capability)
                    && (destination != ToolDataDestination::RemoteModel || scope.allow_remote_model)
            })
        {
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
        );
        Err(unauthorized(
            "operation is outside the authorized conversation or destination scope",
        ))
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
    ) -> Result<(), ConnectorErrorBody> {
        let observed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| unavailable("clockUnavailable", "system clock is before Unix epoch"))?
            .as_nanos();
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
            stage: ConnectorAuditStage::Request,
            conversation_id: conversation_id.map(str::to_string),
            destination: request.destination,
            outcome,
            returned_item_count,
            released_body_byte_count,
            request_body_byte_count,
            draft_id: None,
            policy_decision_id: None,
            previous_event_sha256: None,
            event_sha256: String::new(),
        };
        append_owner_only_connector_event(&self.audit_path, event)
            .map_err(|error| crate::connector::integrity_error(error))
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
        )
    }
}

impl ConnectorRequestHandler for DirectConnectorService<'_> {
    fn handle_connector_request(&self, request: ConnectorRequest) -> ConnectorResponse {
        self.handle(request)
    }
}

fn direct_query_restore_error(error: LiveQueryError) -> RestoreError {
    RestoreError::Integrity(format!("direct SQLite query failed safely: {error}"))
}

fn query_error(error: LiveQueryError) -> ConnectorErrorBody {
    match error {
        LiveQueryError::InvalidArgument(message) | LiveQueryError::InvalidCursor(message) => {
            invalid(&message)
        }
        LiveQueryError::NotFound(message) => crate::connector::not_found(&message),
        LiveQueryError::SearchUnavailable(message) => unavailable("searchUnavailable", &message),
        LiveQueryError::UnsafeSource(message) => {
            crate::connector::integrity_error(RestoreError::UnsafePath(message))
        }
        LiveQueryError::Database(message) => unavailable("directQueryFailed", &message),
        LiveQueryError::ResponseTooLarge { .. } => unavailable(
            "responseTooLarge",
            "bounded query response exceeded its safety limit",
        ),
    }
}

fn direct_conversation_shape(conversation_id: &str) -> (ConversationKind, usize) {
    if wx_db::is_group_chat(conversation_id) {
        (ConversationKind::Group, 0)
    } else if conversation_id.starts_with("gh_") {
        (ConversationKind::Business, 1)
    } else {
        (ConversationKind::Direct, 1)
    }
}

fn direct_conversation_label(item: &ConversationItem) -> String {
    item.display_name
        .as_ref()
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| item.id.clone())
}

fn includes_timestamp(scope: &ConversationToolScope, timestamp: i64) -> bool {
    scope
        .not_before_unix
        .is_none_or(|not_before| timestamp >= not_before)
        && scope
            .not_after_unix
            .is_none_or(|not_after| timestamp <= not_after)
}

fn direct_query_digest(query: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"greenbubbles-direct-connector-search-v1\0");
    hasher.update(query.trim().as_bytes());
    hex::encode(hasher.finalize())
}

fn warning_codes(warnings: &[QueryWarning]) -> BTreeSet<String> {
    warnings
        .iter()
        .map(|warning| format!("directQuery.{}", warning.code))
        .collect()
}

fn omitted_warning_count(warnings: &[QueryWarning]) -> u64 {
    warnings
        .iter()
        .filter(|warning| {
            matches!(
                warning.code,
                "messageRowDecodeFailed" | "shardRowReadFailed" | "shardQueryFailed"
            )
        })
        .map(|warning| warning.count.unwrap_or(1) as u64)
        .sum()
}

fn extend_direct_projection_limitations(
    scope: &ConversationToolScope,
    limitations: &mut BTreeSet<String>,
) {
    if scope.message_fields.contains(&ToolMessageField::Direction) {
        limitations.insert("directDirectionUnavailable".to_string());
    }
    if scope
        .message_fields
        .contains(&ToolMessageField::Attachments)
    {
        limitations.insert("directAttachmentReferencesUnavailable".to_string());
    }
    if scope
        .message_fields
        .contains(&ToolMessageField::Relationships)
    {
        limitations.insert("directRelationshipReferencesUnavailable".to_string());
    }
}

fn operation_name(operation: &ConnectorOperation) -> &'static str {
    match operation {
        ConnectorOperation::Capabilities => "capabilities",
        ConnectorOperation::Status => "status",
        ConnectorOperation::Coverage => "coverage",
        ConnectorOperation::GetChanges { .. } => "getChanges",
        ConnectorOperation::GetCachedMoments { .. } => "getCachedMoments",
        ConnectorOperation::ListConversations { .. } => "listConversations",
        ConnectorOperation::SearchMessages { .. } => "searchMessages",
        ConnectorOperation::GetMessages { .. } => "getMessages",
        ConnectorOperation::GetMessage { .. } => "getMessage",
        ConnectorOperation::GetArtifact { .. } => "getArtifact",
        ConnectorOperation::ResolveContact { .. } => "resolveContact",
        ConnectorOperation::ResolveConversation { .. } => "resolveConversation",
        ConnectorOperation::CreateMessageDraft { .. } => "createMessageDraft",
        ConnectorOperation::CreateReplyDraft { .. } => "createReplyDraft",
        ConnectorOperation::CreateAttachmentDraft { .. } => "createAttachmentDraft",
        ConnectorOperation::PreviewAction { .. } => "previewAction",
        ConnectorOperation::Bootstrap => "bootstrap",
        ConnectorOperation::Synchronize => "synchronize",
        ConnectorOperation::Refresh => "refresh",
    }
}

fn operation_conversation(operation: &ConnectorOperation) -> Option<&str> {
    match operation {
        ConnectorOperation::GetMessages {
            conversation_id, ..
        }
        | ConnectorOperation::GetArtifact {
            conversation_id, ..
        }
        | ConnectorOperation::ResolveConversation { conversation_id }
        | ConnectorOperation::CreateMessageDraft {
            conversation_id, ..
        }
        | ConnectorOperation::CreateReplyDraft {
            conversation_id, ..
        }
        | ConnectorOperation::CreateAttachmentDraft {
            conversation_id, ..
        } => Some(conversation_id),
        ConnectorOperation::SearchMessages {
            conversation_id, ..
        } => conversation_id.as_deref(),
        _ => None,
    }
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
