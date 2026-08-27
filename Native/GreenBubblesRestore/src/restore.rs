use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use rusqlite::{types::ValueRef, Connection, OpenFlags, OptionalExtension, Row};
use sha2::{Digest, Sha256};

use crate::entities::{restore_entities, EntitySeeds};
use crate::{
    artifact::ArtifactResolver, ArtifactAvailability, ArtifactDecodeState, CanonicalMessage,
    DirectionEvidence, MessageDirection, MessageOrderingBasis, MessageRelationship,
    MessageRelationshipKind, MessageTableCoverage, PreparedCatalog, RawSQLiteValue, RejectedRow,
    RelationshipResolutionState, RestorationCompletion, RestorationCoverage, RestorationIntegrity,
    RestorationReport, RestoreError, SemanticDecodeState, TableCoverageRole, TableSchemaCoverage,
    TypedPayload,
};

#[derive(Debug, Clone)]
pub struct RestorationOptions {
    pub output_directory: PathBuf,
    pub account_root: Option<PathBuf>,
}

pub fn restore_catalog(
    catalog: &PreparedCatalog,
    options: &RestorationOptions,
) -> Result<RestorationReport, RestoreError> {
    create_owner_only_directory(&options.output_directory)?;
    let messages_path = options.output_directory.join("messages.ndjson");
    let rejections_path = options.output_directory.join("rejections.ndjson");
    let artifacts_path = options.output_directory.join("artifacts.ndjson");
    let coverage_path = options.output_directory.join("coverage.json");
    let report_path = options.output_directory.join("report.json");
    let mut rejections = owner_only_writer(&rejections_path)?;
    let staging_directory = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(&options.output_directory)?;
    create_owner_only_directory(staging_directory.path())?;
    let staging_path = staging_directory.path().join("messages.sqlite");
    let staging = Connection::open(&staging_path)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&staging_path, fs::Permissions::from_mode(0o600))?;
    staging.execute_batch(
        "PRAGMA journal_mode = OFF;
         PRAGMA synchronous = OFF;
         CREATE TABLE staged_message(
           canonical_id TEXT PRIMARY KEY NOT NULL,
           conversation_id TEXT NOT NULL,
           sort_sequence INTEGER,
           server_id INTEGER,
           created_at INTEGER,
           local_id INTEGER,
           source_logical_path TEXT NOT NULL,
           source_table_id TEXT NOT NULL,
           source_row_id INTEGER NOT NULL,
           message_json BLOB NOT NULL
         );
         CREATE INDEX staged_by_order ON staged_message(
           conversation_id, sort_sequence, server_id, created_at, local_id,
           source_logical_path, source_table_id, source_row_id
         );
         CREATE INDEX staged_by_server ON staged_message(conversation_id, server_id);
         CREATE INDEX staged_by_local ON staged_message(conversation_id, local_id);",
    )?;
    let mut artifact_resolver = ArtifactResolver::new(
        catalog,
        options.account_root.as_deref(),
        &options.output_directory,
    )?;
    let account_id = options
        .account_root
        .as_deref()
        .and_then(|path| fs::canonicalize(path).ok())
        .map(|path| opaque_id(path.to_string_lossy().as_bytes()))
        .unwrap_or_else(|| opaque_id(catalog.manifest.source_fingerprint.as_bytes()));
    let self_username = options
        .account_root
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .map(wx_media::extract_wxid);
    let mut integrity = RestorationIntegrity {
        database_count: catalog.databases.len() as u64,
        ..Default::default()
    };
    let mut table_coverage = Vec::new();
    let mut all_table_coverage = Vec::new();
    let mut entity_seeds = EntitySeeds::default();

    for database in &catalog.databases {
        let connection =
            Connection::open_with_flags(&database.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.execute_batch("PRAGMA query_only = ON")?;
        let names = load_name_map(&connection).unwrap_or_default();

        for table in &database.tables {
            let columns = table_columns(&connection, table)?;
            let table_id = opaque_id(table.as_bytes());
            let (role, classification_reason) = classify_table(table, &columns);
            if role == TableCoverageRole::UnhandledMessageCandidate {
                integrity.message_candidate_gap_count += 1;
            }
            all_table_coverage.push(TableSchemaCoverage {
                source_set_id: database.source_set_id.clone(),
                source_logical_path: database.logical_path.clone(),
                source_table_id: table_id.clone(),
                source_table_name: table.clone(),
                columns: columns.clone(),
                role,
                classification_reason: classification_reason.to_string(),
            });
            if role != TableCoverageRole::Message {
                continue;
            }
            integrity.message_table_count += 1;
            let conversation = infer_conversation(table, &database.logical_path, names.values());
            let schema_identity = columns
                .iter()
                .map(|value| value.to_ascii_lowercase())
                .collect::<Vec<_>>()
                .join("\u{1f}");
            let schema_id = opaque_id(schema_identity.as_bytes());
            *integrity
                .message_schema_counts
                .entry(schema_id)
                .or_default() += 1;
            let quoted = quote_identifier(table);
            let count_sql = format!("SELECT count(*) FROM {quoted}");
            let row_count: i64 = connection.query_row(&count_sql, [], |row| row.get(0))?;
            integrity.source_row_count += row_count.max(0) as u64;
            table_coverage.push(MessageTableCoverage {
                source_set_id: database.source_set_id.clone(),
                source_logical_path: database.logical_path.clone(),
                source_table_id: table_id.clone(),
                source_table_name: table.clone(),
                source_row_count: row_count.max(0) as u64,
                columns: columns.clone(),
            });

            let select_sql = format!("SELECT rowid, * FROM {quoted} ORDER BY rowid");
            let mut statement = connection.prepare(&select_sql)?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let context = RowRestorationContext {
                    set_id: &database.source_set_id,
                    logical_path: &database.logical_path,
                    table_id: &table_id,
                    table_name: table,
                    account_id: &account_id,
                    conversation: &conversation,
                    names: &names,
                    self_username: self_username.as_deref(),
                };
                match restore_row(row, &columns, &context) {
                    Ok(mut message) => {
                        message.artifact_references =
                            artifact_resolver.resolve_message(&message)?;
                        entity_seeds.observe_message(&message);
                        let logical_key = message
                            .logical_type
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "missing".to_string());
                        *integrity
                            .logical_type_counts
                            .entry(logical_key)
                            .or_insert(0) += 1;
                        let sub_type_key = match (message.logical_type, message.sub_type) {
                            (Some(logical), Some(sub)) => format!("{logical}:{sub}"),
                            _ => "missing".to_string(),
                        };
                        *integrity
                            .logical_sub_type_counts
                            .entry(sub_type_key)
                            .or_default() += 1;
                        if let TypedPayload::Unknown { reason } = &message.typed_payload {
                            integrity.unknown_payload_count += 1;
                            *integrity
                                .unknown_payload_reason_counts
                                .entry(reason.clone())
                                .or_default() += 1;
                        }
                        if message.semantic_decode_state != SemanticDecodeState::Complete {
                            integrity.semantic_gap_count += 1;
                            let reason = message
                                .semantic_gap_reason
                                .clone()
                                .unwrap_or_else(|| "unspecified semantic coverage gap".to_string());
                            *integrity
                                .semantic_gap_reason_counts
                                .entry(reason)
                                .or_default() += 1;
                        }
                        let json = serde_json::to_vec(&message)?;
                        let inserted = staging.execute(
                            "INSERT OR IGNORE INTO staged_message(
                               canonical_id, conversation_id, sort_sequence, server_id,
                               created_at, local_id, source_logical_path, source_table_id,
                               source_row_id, message_json
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                            rusqlite::params![
                                message.canonical_id,
                                message.conversation_id,
                                message.sort_sequence,
                                message.server_id,
                                message.created_at_unix,
                                message.local_id,
                                message.source_logical_path,
                                message.source_table_id,
                                message.source_row_id,
                                json,
                            ],
                        )?;
                        if inserted != 1 {
                            integrity.duplicate_canonical_id_count += 1;
                            return Err(RestoreError::Integrity(
                                "canonical message identity collision".to_string(),
                            ));
                        }
                        integrity.restored_row_count += 1;
                    }
                    Err(rejection) => {
                        serde_json::to_writer(&mut rejections, &rejection)?;
                        rejections.write_all(b"\n")?;
                        integrity.rejected_row_count += 1;
                    }
                }
            }
        }
    }
    rejections.flush()?;

    if !integrity.row_equation_holds() {
        return Err(RestoreError::Manifest(
            "restoration row equation failed".to_string(),
        ));
    }

    let mut messages = owner_only_writer(&messages_path)?;
    let mut ordered = staging.prepare(
        "WITH conversation_basis AS (
           SELECT conversation_id,
             CASE
               WHEN COUNT(*) = COUNT(sort_sequence) THEN 1
               WHEN COUNT(*) = COUNT(server_id) THEN 2
               WHEN COUNT(*) = COUNT(created_at) THEN 3
               WHEN COUNT(*) = COUNT(local_id) THEN 4
               ELSE 5
             END AS basis
           FROM staged_message GROUP BY conversation_id
         )
         SELECT message_json, conversation_basis.basis
         FROM staged_message
         JOIN conversation_basis USING(conversation_id)
         ORDER BY staged_message.conversation_id,
           CASE conversation_basis.basis
             WHEN 1 THEN sort_sequence
             WHEN 2 THEN server_id
             WHEN 3 THEN created_at
             WHEN 4 THEN local_id
             ELSE COALESCE(sort_sequence, server_id, created_at, local_id, source_row_id)
           END,
           sort_sequence,
           server_id,
           created_at,
           local_id,
           source_logical_path,
           source_table_id,
           source_row_id",
    )?;
    let mut rows = ordered.query([])?;
    let mut previous_conversation: Option<String> = None;
    let mut conversation_ordinal = 0_u64;
    while let Some(row) = rows.next()? {
        let bytes: Vec<u8> = row.get(0)?;
        let basis: i64 = row.get(1)?;
        let mut message: CanonicalMessage = serde_json::from_slice(&bytes)?;
        if previous_conversation.as_deref() == Some(&message.conversation_id) {
            conversation_ordinal += 1;
        } else {
            previous_conversation = Some(message.conversation_id.clone());
            conversation_ordinal = 0;
        }
        message.conversation_ordinal = conversation_ordinal;
        message.ordering_basis = match basis {
            1 => MessageOrderingBasis::SortSequence,
            2 => MessageOrderingBasis::ServerId,
            3 => MessageOrderingBasis::CreatedAt,
            4 => MessageOrderingBasis::LocalId,
            _ => MessageOrderingBasis::HybridSourceFallback,
        };
        let ordering_key = match message.ordering_basis {
            MessageOrderingBasis::SortSequence => "sortSequence",
            MessageOrderingBasis::ServerId => "serverId",
            MessageOrderingBasis::CreatedAt => "createdAt",
            MessageOrderingBasis::LocalId => "localId",
            MessageOrderingBasis::HybridSourceFallback => "hybridSourceFallback",
        };
        *integrity
            .ordering_basis_counts
            .entry(ordering_key.to_string())
            .or_default() += 1;
        let direction_key = match message.direction {
            MessageDirection::Incoming => "incoming",
            MessageDirection::Outgoing => "outgoing",
            MessageDirection::Unknown => "unknown",
        };
        *integrity
            .direction_counts
            .entry(direction_key.to_string())
            .or_default() += 1;
        resolve_relationships(&staging, &mut message)?;
        integrity.artifact_reference_count += message.artifact_references.len() as u64;
        integrity.relationship_reference_count += message.relationships.len() as u64;
        for relationship in &message.relationships {
            match relationship.resolution_state {
                RelationshipResolutionState::Resolved => integrity.resolved_relationship_count += 1,
                RelationshipResolutionState::TargetNotPresentLocally => {
                    integrity.unresolved_relationship_count += 1;
                    integrity.absent_relationship_target_count += 1;
                }
                RelationshipResolutionState::ReferenceIdentifierMissing => {
                    integrity.unresolved_relationship_count += 1;
                    integrity.missing_relationship_identifier_count += 1;
                }
                RelationshipResolutionState::Ambiguous => {
                    integrity.unresolved_relationship_count += 1;
                    integrity.ambiguous_relationship_count += 1;
                }
                RelationshipResolutionState::Pending => {
                    integrity.unresolved_relationship_count += 1;
                }
            }
        }
        serde_json::to_writer(&mut messages, &message)?;
        messages.write_all(b"\n")?;
    }
    messages.flush()?;

    let mut artifacts = owner_only_writer(&artifacts_path)?;
    for artifact in artifact_resolver.artifacts() {
        integrity.unique_artifact_count += 1;
        match artifact.availability {
            ArtifactAvailability::Downloaded => integrity.downloaded_artifact_count += 1,
            ArtifactAvailability::MaterializedFromDatabase => {
                integrity.materialized_artifact_count += 1
            }
            ArtifactAvailability::NotDownloaded
            | ArtifactAvailability::RemoteOnly
            | ArtifactAvailability::Expired
            | ArtifactAvailability::Deleted
            | ArtifactAvailability::MetadataMissing => integrity.missing_artifact_count += 1,
            ArtifactAvailability::AccountRootUnavailable => {
                integrity.missing_artifact_count += 1;
                integrity.account_root_unavailable_artifact_count += 1;
            }
            ArtifactAvailability::Ambiguous => integrity.ambiguous_artifact_count += 1,
            ArtifactAvailability::Corrupt => integrity.corrupt_artifact_count += 1,
            ArtifactAvailability::UnsafePath => integrity.unsafe_artifact_count += 1,
        }
        if artifact.decode_state == ArtifactDecodeState::Decoded {
            integrity.decoded_artifact_count += 1;
        }
        if matches!(
            artifact.availability,
            ArtifactAvailability::Downloaded
                | ArtifactAvailability::MaterializedFromDatabase
                | ArtifactAvailability::Ambiguous
        ) && matches!(
            artifact.decode_state,
            ArtifactDecodeState::KeyUnavailable
                | ArtifactDecodeState::Unsupported
                | ArtifactDecodeState::Failed
        ) {
            integrity.artifact_decode_gap_count += 1;
        }
        serde_json::to_writer(&mut artifacts, artifact)?;
        artifacts.write_all(b"\n")?;
    }
    artifacts.flush()?;

    let entity_result = restore_entities(
        catalog,
        &account_id,
        entity_seeds,
        &options.output_directory,
    )?;
    integrity.conversation_count = entity_result.conversation_count;
    integrity.participant_count = entity_result.participant_count;
    integrity.group_member_count = entity_result.group_member_count;
    integrity.entity_source_row_count = entity_result.source_row_count;
    integrity.entity_decode_gap_count = entity_result.decode_gap_count;
    integrity.missing_local_profile_count = entity_result.missing_local_profile_count;
    integrity.unresolved_conversation_count = entity_result.unresolved_conversation_count;

    table_coverage.sort_by(|left, right| {
        (
            &left.source_logical_path,
            &left.source_table_name,
            &left.source_set_id,
        )
            .cmp(&(
                &right.source_logical_path,
                &right.source_table_name,
                &right.source_set_id,
            ))
    });
    all_table_coverage.sort_by(|left, right| {
        (
            &left.source_logical_path,
            &left.source_table_name,
            &left.source_set_id,
        )
            .cmp(&(
                &right.source_logical_path,
                &right.source_table_name,
                &right.source_set_id,
            ))
    });
    let coverage = RestorationCoverage {
        format_version: 2,
        decoder_name: "greenbubbles-restore".to_string(),
        decoder_version: env!("CARGO_PKG_VERSION").to_string(),
        snapshot_manifest_format_version: catalog.manifest.manifest_format_version,
        message_tables: table_coverage,
        all_tables: all_table_coverage,
        logical_type_counts: integrity.logical_type_counts.clone(),
        logical_sub_type_counts: integrity.logical_sub_type_counts.clone(),
        unknown_payload_reason_counts: integrity.unknown_payload_reason_counts.clone(),
        semantic_gap_reason_counts: integrity.semantic_gap_reason_counts.clone(),
    };
    write_owner_only_json(&coverage_path, &coverage)?;

    let client_build_compatibility = catalog.manifest.client_build_compatibility();
    let mut completion = RestorationCompletion::evaluate(&integrity);
    if catalog.manifest.manifest_format_version >= 2
        && !client_build_compatibility.production_compatible
    {
        completion.full_restoration_achieved = false;
    }
    let report = RestorationReport {
        format_version: 3,
        account_id,
        source_fingerprint: catalog.manifest.source_fingerprint.clone(),
        client_build_compatibility,
        messages_path: messages_path.display().to_string(),
        rejections_path: rejections_path.display().to_string(),
        artifacts_path: artifacts_path.display().to_string(),
        conversations_path: entity_result.conversations_path.display().to_string(),
        participants_path: entity_result.participants_path.display().to_string(),
        coverage_path: coverage_path.display().to_string(),
        report_path: report_path.display().to_string(),
        integrity,
        completion,
    };
    write_owner_only_json(&report_path, &report)?;
    Ok(report)
}

struct RowRestorationContext<'a> {
    set_id: &'a str,
    logical_path: &'a str,
    table_id: &'a str,
    table_name: &'a str,
    account_id: &'a str,
    conversation: &'a str,
    names: &'a HashMap<i64, String>,
    self_username: Option<&'a str>,
}

fn resolve_row_conversation(
    row: &Row<'_>,
    columns: &[String],
    context: &RowRestorationContext<'_>,
) -> Option<String> {
    let index = column_index(
        columns,
        &[
            "talker",
            "talker_name",
            "chat_name",
            "chat_username",
            "conversation_id",
            "session_id",
            "biz_username",
            "username",
            "user_name",
            "chat_id",
            "chat_name_id",
        ],
    )?;
    match row.get_ref(index).ok()? {
        ValueRef::Integer(value) => context.names.get(&value).cloned(),
        ValueRef::Real(value) => context.names.get(&(value as i64)).cloned(),
        ValueRef::Text(value) | ValueRef::Blob(value) => {
            let decoded = String::from_utf8(value.to_vec()).ok()?;
            if decoded.is_empty() {
                None
            } else if let Ok(identifier) = decoded.parse::<i64>() {
                context.names.get(&identifier).cloned().or(Some(decoded))
            } else {
                Some(decoded)
            }
        }
        ValueRef::Null => None,
    }
}

fn restore_row(
    row: &Row<'_>,
    columns: &[String],
    context: &RowRestorationContext<'_>,
) -> Result<CanonicalMessage, RejectedRow> {
    let source_row_id = get_i64(row, 0).unwrap_or(0);
    let field = |names: &[&str]| column_index(columns, names);
    let local_id = field(&["local_id", "message_local_id", "msg_local_id", "meslocalid"])
        .and_then(|index| get_i64(row, index));
    let server_id = field(&[
        "server_id",
        "svr_id",
        "message_svr_id",
        "msg_svr_id",
        "msg_server_id",
        "messvrid",
    ])
    .and_then(|index| get_i64(row, index));
    let sort_sequence =
        field(&["sort_seq", "sort_sequence", "sequence"]).and_then(|index| get_i64(row, index));
    let raw_type = field(&[
        "local_type",
        "message_local_type",
        "msg_type",
        "message_type",
        "type",
    ])
    .and_then(|index| get_i64(row, index));
    let sender_row_id = field(&["real_sender_id", "sender_id", "from_id", "from_user_id"])
        .and_then(|index| get_i64(row, index));
    let created_at = field(&[
        "create_time",
        "message_create_time",
        "msg_create_time",
        "create_timestamp",
        "timestamp",
    ])
    .and_then(|index| get_i64(row, index));
    let status = field(&["status", "message_status"]).and_then(|index| get_i64(row, index));
    let explicit_sender_flag =
        field(&["is_sender", "is_send", "is_sent_by_self"]).and_then(|index| get_i64(row, index));
    let content = field(&[
        "message_content",
        "msg_content",
        "content",
        "message_data",
        "msg_data",
    ])
    .and_then(|index| get_bytes(row, index));
    let packed = field(&["packed_info_data", "packed_info", "message_packed_info"])
        .and_then(|index| get_bytes(row, index));
    let compression_type = field(&[
        "WCDB_CT_message_content",
        "wcdb_ct_message_content",
        "compression_type",
    ])
    .and_then(|index| get_i64(row, index));
    let compressed =
        field(&["compress_content", "compressed_content"]).and_then(|index| get_bytes(row, index));
    let raw_columns = columns
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let value = raw_sqlite_value(row, index + 1).unwrap_or(RawSQLiteValue::Null);
            (name.clone(), value)
        })
        .collect();

    let (logical_type, sub_type) = raw_type
        .map(wx_db::split_local_type)
        .map(|(message_type, subtype)| (Some(message_type), Some(subtype)))
        .unwrap_or((None, None));
    let row_conversation = resolve_row_conversation(row, columns, context)
        .unwrap_or_else(|| context.conversation.to_string());
    let row_conversation_id = scoped_opaque_id(context.account_id, row_conversation.as_bytes());
    let fallback_sender = sender_row_id
        .and_then(|value| context.names.get(&value))
        .cloned()
        .or_else(|| {
            field(&["sender", "sender_name", "from_user", "from_username"])
                .and_then(|index| get_bytes(row, index))
                .filter(|value| !value.is_empty())
                .and_then(|value| String::from_utf8(value).ok())
        });
    let mut decoded_sender = fallback_sender.clone();
    let (typed_payload, semantic_decode_state, semantic_gap_reason) = match raw_type {
        Some(local_type) => match wx_db::decode_message_for_test(
            sort_sequence.unwrap_or_default(),
            server_id.unwrap_or_default(),
            local_type,
            fallback_sender.as_deref().unwrap_or(""),
            &row_conversation,
            created_at.unwrap_or_default(),
            content.as_deref().unwrap_or_default(),
            packed.as_deref(),
            status.unwrap_or_default() as i32,
            compression_type.map(|value| value as i32),
            compressed.as_deref(),
            row_conversation.ends_with("@chatroom"),
        ) {
            Ok(decoded) => {
                if !decoded.sender.is_empty() {
                    decoded_sender = Some(decoded.sender);
                }
                match decoded.content {
                    wx_db::MessageContent::Unknown { msg_type, .. } => {
                        let reason = format!("unsupported logical message type {msg_type}");
                        (
                            TypedPayload::Unknown {
                                reason: reason.clone(),
                            },
                            SemanticDecodeState::UnknownType,
                            Some(reason),
                        )
                    }
                    known => {
                        let partial_reason = match &known {
                            wx_db::MessageContent::AppGeneric { sub_type, .. } => Some(format!(
                                "app message subtype {sub_type} has only generic XML decoding"
                            )),
                            wx_db::MessageContent::MergedMessages { .. } => Some(
                                "merged-message children are retained in raw XML but not yet normalized"
                                    .to_string(),
                            ),
                            wx_db::MessageContent::ChannelVideo { sub_type, .. } => Some(format!(
                                "channel media subtype {sub_type} metadata is decoded but its nested media graph is not yet normalized"
                            )),
                            _ => None,
                        };
                        match serde_json::to_value(known) {
                            Ok(value) => (
                                TypedPayload::Decoded(value),
                                if partial_reason.is_some() {
                                    SemanticDecodeState::Partial
                                } else {
                                    SemanticDecodeState::Complete
                                },
                                partial_reason,
                            ),
                            Err(error) => {
                                let reason = format!("typed serialization failed: {error}");
                                (
                                    TypedPayload::Unknown {
                                        reason: reason.clone(),
                                    },
                                    SemanticDecodeState::Failed,
                                    Some(reason),
                                )
                            }
                        }
                    }
                }
            }
            Err(error) => {
                let reason = format!("typed decode failed: {error}");
                (
                    TypedPayload::Unknown {
                        reason: reason.clone(),
                    },
                    SemanticDecodeState::Failed,
                    Some(reason),
                )
            }
        },
        None => {
            let reason = "local_type column is absent or null".to_string();
            (
                TypedPayload::Unknown {
                    reason: reason.clone(),
                },
                SemanticDecodeState::MissingType,
                Some(reason),
            )
        }
    };

    let identity = format!("{}:{}:{source_row_id}", context.set_id, context.table_id);
    let relationships = extract_relationships(
        logical_type,
        sub_type,
        content.as_deref(),
        compressed.as_deref(),
    );
    let (direction, direction_evidence) = infer_direction(
        explicit_sender_flag,
        decoded_sender.as_deref(),
        &row_conversation,
        context.self_username,
    );
    Ok(CanonicalMessage {
        canonical_id: opaque_id(identity.as_bytes()),
        account_id: context.account_id.to_string(),
        source_set_id: context.set_id.to_string(),
        source_logical_path: context.logical_path.to_string(),
        source_table_id: context.table_id.to_string(),
        source_table_name: context.table_name.to_string(),
        source_row_id,
        conversation_id: row_conversation_id,
        conversation_source_identifier_base64: base64::engine::general_purpose::STANDARD
            .encode(row_conversation.as_bytes()),
        sender_id: decoded_sender
            .as_ref()
            .map(|value| scoped_opaque_id(context.account_id, value.as_bytes())),
        sender_source_identifier_base64: decoded_sender
            .map(|value| base64::engine::general_purpose::STANDARD.encode(value.as_bytes())),
        local_id,
        server_id,
        sort_sequence,
        created_at_unix: created_at,
        conversation_ordinal: 0,
        ordering_basis: MessageOrderingBasis::HybridSourceFallback,
        raw_type,
        logical_type,
        sub_type,
        status,
        direction,
        direction_evidence,
        content_base64: content
            .map(|value| base64::engine::general_purpose::STANDARD.encode(value)),
        packed_info_base64: packed
            .map(|value| base64::engine::general_purpose::STANDARD.encode(value)),
        compression_type,
        raw_columns,
        typed_payload,
        semantic_decode_state,
        semantic_gap_reason,
        relationships,
        artifact_references: Vec::new(),
    })
}

fn extract_relationships(
    logical_type: Option<u32>,
    sub_type: Option<u32>,
    content: Option<&[u8]>,
    compressed: Option<&[u8]>,
) -> Vec<MessageRelationship> {
    let kind = match (logical_type, sub_type) {
        (Some(49), Some(57)) => Some(MessageRelationshipKind::Quote),
        (Some(10002), _) => Some(MessageRelationshipKind::Recall),
        _ => None,
    };
    let Some(kind) = kind else {
        return Vec::new();
    };
    let raw = compressed.or(content).unwrap_or_default();
    let server_tags: &[&str] = match kind {
        MessageRelationshipKind::Recall => &["newmsgid", "svrid", "msgid"],
        _ => &["refermsgsvrid", "svrid", "newmsgid"],
    };
    let local_tags = &["refermsglocalid", "localid", "msglocalid"];
    vec![MessageRelationship {
        kind,
        target_canonical_id: None,
        target_server_id: extract_tagged_i64(raw, server_tags),
        target_local_id: extract_tagged_i64(raw, local_tags),
        resolved: false,
        resolution_state: RelationshipResolutionState::Pending,
        raw_reference_base64: (!raw.is_empty())
            .then(|| base64::engine::general_purpose::STANDARD.encode(raw)),
    }]
}

fn infer_direction(
    explicit_sender_flag: Option<i64>,
    sender: Option<&str>,
    conversation: &str,
    self_username: Option<&str>,
) -> (MessageDirection, DirectionEvidence) {
    if let Some(flag) = explicit_sender_flag {
        return if flag == 0 {
            (
                MessageDirection::Incoming,
                DirectionEvidence::ExplicitSourceColumn,
            )
        } else {
            (
                MessageDirection::Outgoing,
                DirectionEvidence::ExplicitSourceColumn,
            )
        };
    }
    let Some(sender) = sender.filter(|value| !value.is_empty()) else {
        return (MessageDirection::Unknown, DirectionEvidence::Unresolved);
    };
    if conversation.ends_with("@chatroom") {
        if let Some(account) = self_username.filter(|value| !value.is_empty()) {
            return if sender == account {
                (
                    MessageDirection::Outgoing,
                    DirectionEvidence::SenderMatchesAccount,
                )
            } else {
                (
                    MessageDirection::Incoming,
                    DirectionEvidence::SenderDiffersFromAccount,
                )
            };
        }
        return (MessageDirection::Unknown, DirectionEvidence::Unresolved);
    }
    if conversation.starts_with("unresolved:") {
        return (MessageDirection::Unknown, DirectionEvidence::Unresolved);
    }
    if sender == conversation {
        (
            MessageDirection::Incoming,
            DirectionEvidence::SenderMatchesConversation,
        )
    } else {
        (
            MessageDirection::Outgoing,
            DirectionEvidence::SenderDiffersFromConversation,
        )
    }
}

fn extract_tagged_i64(raw: &[u8], tags: &[&str]) -> Option<i64> {
    let value = String::from_utf8_lossy(raw).to_ascii_lowercase();
    for tag in tags {
        let open = format!("<{tag}>");
        if let Some(start) = value.find(&open) {
            let remainder = &value[start + open.len()..];
            let digits = remainder
                .trim_start()
                .chars()
                .take_while(|value| value.is_ascii_digit() || *value == '-')
                .collect::<String>();
            if let Ok(number) = digits.parse() {
                return Some(number);
            }
        }
        for quote in ['\'', '"'] {
            let attribute = format!("{tag}={quote}");
            if let Some(start) = value.find(&attribute) {
                let remainder = &value[start + attribute.len()..];
                let digits = remainder
                    .chars()
                    .take_while(|value| value.is_ascii_digit() || *value == '-')
                    .collect::<String>();
                if let Ok(number) = digits.parse() {
                    return Some(number);
                }
            }
        }
    }
    None
}

fn resolve_relationships(
    staging: &Connection,
    message: &mut CanonicalMessage,
) -> Result<(), RestoreError> {
    for relationship in &mut message.relationships {
        let targets = if let Some(server_id) = relationship.target_server_id {
            relationship_targets(staging, &message.conversation_id, "server_id", server_id)?
        } else if let Some(local_id) = relationship.target_local_id {
            relationship_targets(staging, &message.conversation_id, "local_id", local_id)?
        } else {
            Vec::new()
        };
        relationship.resolution_state =
            if relationship.target_server_id.is_none() && relationship.target_local_id.is_none() {
                RelationshipResolutionState::ReferenceIdentifierMissing
            } else {
                match targets.len() {
                    0 => RelationshipResolutionState::TargetNotPresentLocally,
                    1 => RelationshipResolutionState::Resolved,
                    _ => RelationshipResolutionState::Ambiguous,
                }
            };
        relationship.resolved =
            relationship.resolution_state == RelationshipResolutionState::Resolved;
        relationship.target_canonical_id = (targets.len() == 1).then(|| targets[0].clone());
    }
    Ok(())
}

fn relationship_targets(
    staging: &Connection,
    conversation_id: &str,
    column: &str,
    identifier: i64,
) -> Result<Vec<String>, RestoreError> {
    debug_assert!(matches!(column, "server_id" | "local_id"));
    let sql = format!(
        "SELECT canonical_id FROM staged_message
         WHERE conversation_id = ?1 AND {column} = ?2
         ORDER BY source_logical_path, source_table_id, source_row_id LIMIT 2"
    );
    let mut statement = staging.prepare(&sql)?;
    let values = statement
        .query_map(rusqlite::params![conversation_id, identifier], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(values)
}

fn column_index(columns: &[String], requested: &[&str]) -> Option<usize> {
    columns
        .iter()
        .position(|column| {
            requested
                .iter()
                .any(|requested| column.eq_ignore_ascii_case(requested))
        })
        .map(|index| index + 1)
}

fn table_columns(connection: &Connection, table: &str) -> Result<Vec<String>, RestoreError> {
    let sql = format!("PRAGMA table_info({})", quote_identifier(table));
    let mut statement = connection.prepare(&sql)?;
    let values = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(values)
}

fn load_name_map(connection: &Connection) -> Result<HashMap<i64, String>, RestoreError> {
    let table: Option<String> = connection
        .query_row(
            "SELECT name FROM sqlite_schema WHERE type='table' AND lower(name)='name2id' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(table) = table else {
        return Ok(HashMap::new());
    };
    let columns = table_columns(connection, &table)?;
    let Some(user_column) = columns.iter().find(|column| {
        ["user_name", "username", "name"]
            .iter()
            .any(|candidate| column.eq_ignore_ascii_case(candidate))
    }) else {
        return Ok(HashMap::new());
    };
    let sql = format!(
        "SELECT rowid, {} FROM {}",
        quote_identifier(user_column),
        quote_identifier(&table)
    );
    let mut statement = connection.prepare(&sql)?;
    let values = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<HashMap<_, _>, _>>()?;
    Ok(values)
}

fn infer_conversation<'a>(
    table: &str,
    logical_path: &str,
    names: impl Iterator<Item = &'a String>,
) -> String {
    let table_suffix = table
        .strip_prefix("Msg_")
        .or_else(|| table.strip_prefix("Chat_"))
        .unwrap_or(table);
    for name in names {
        if format!("{:x}", md5::compute(name.as_bytes())).eq_ignore_ascii_case(table_suffix) {
            return name.clone();
        }
    }
    format!("unresolved:{logical_path}:{table_suffix}")
}

fn is_message_table(name: &str, columns: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    let has_type = column_index(
        columns,
        &[
            "local_type",
            "message_local_type",
            "msg_type",
            "message_type",
            "type",
        ],
    )
    .is_some();
    let has_content = column_index(
        columns,
        &[
            "message_content",
            "msg_content",
            "content",
            "message_data",
            "msg_data",
            "compress_content",
        ],
    )
    .is_some();
    let has_identity = column_index(
        columns,
        &[
            "local_id",
            "message_local_id",
            "msg_local_id",
            "server_id",
            "svr_id",
            "message_svr_id",
            "msg_svr_id",
            "msg_server_id",
            "sort_seq",
            "create_time",
            "msg_create_time",
        ],
    )
    .is_some();
    let hashed_name = lower
        .strip_prefix("msg_")
        .or_else(|| lower.strip_prefix("chat_"))
        .is_some_and(|suffix| {
            suffix.len() == 32 && suffix.bytes().all(|value| value.is_ascii_hexdigit())
        });
    if hashed_name && (has_type || has_content) {
        return true;
    }
    has_type && has_content && has_identity
}

fn classify_table(name: &str, columns: &[String]) -> (TableCoverageRole, &'static str) {
    if is_message_table(name, columns) {
        return (
            TableCoverageRole::Message,
            "matched the supported message-table name or column signature",
        );
    }
    if is_known_auxiliary_table(name) {
        return (
            TableCoverageRole::KnownAuxiliary,
            "matched a known entity, resource, index, or metadata table family",
        );
    }
    if is_unhandled_message_candidate(name, columns) {
        return (
            TableCoverageRole::UnhandledMessageCandidate,
            "message-like name or columns did not satisfy the safe message adapter signature",
        );
    }
    (
        TableCoverageRole::Other,
        "no supported message signature or known auxiliary-table identity matched",
    )
}

fn is_known_auxiliary_table(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "name2id" | "sessiontable" | "contact" | "chat_room" | "messageresourceinfo" | "voiceinfo"
    ) || [
        "index",
        "metadata",
        "_meta",
        "resource",
        "media",
        "emoticon",
        "sticker",
        "attachment",
        "download",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn is_unhandled_message_candidate(name: &str, columns: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    let message_like_name = [
        "message",
        "msg",
        "chat",
        "conversation",
        "history",
        "inbox",
        "outbox",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    let has_type = column_index(
        columns,
        &[
            "local_type",
            "message_local_type",
            "msg_type",
            "message_type",
            "type",
        ],
    )
    .is_some();
    let has_content = column_index(
        columns,
        &[
            "message_content",
            "msg_content",
            "content",
            "message_data",
            "msg_data",
            "compress_content",
            "compressed_content",
        ],
    )
    .is_some();
    let has_identity = column_index(
        columns,
        &[
            "local_id",
            "message_local_id",
            "msg_local_id",
            "server_id",
            "svr_id",
            "message_svr_id",
            "msg_svr_id",
            "msg_server_id",
            "sort_seq",
            "sort_sequence",
            "create_time",
            "msg_create_time",
            "timestamp",
        ],
    )
    .is_some();
    let signature_count =
        usize::from(has_type) + usize::from(has_content) + usize::from(has_identity);
    message_like_name && (has_type || has_content || has_identity) || signature_count >= 2
}

fn get_i64(row: &Row<'_>, index: usize) -> Option<i64> {
    match row.get_ref(index).ok()? {
        ValueRef::Integer(value) => Some(value),
        ValueRef::Real(value) => Some(value as i64),
        ValueRef::Text(value) => std::str::from_utf8(value).ok()?.parse().ok(),
        _ => None,
    }
}

fn get_bytes(row: &Row<'_>, index: usize) -> Option<Vec<u8>> {
    match row.get_ref(index).ok()? {
        ValueRef::Blob(value) | ValueRef::Text(value) => Some(value.to_vec()),
        ValueRef::Null => None,
        ValueRef::Integer(value) => Some(value.to_string().into_bytes()),
        ValueRef::Real(value) => Some(value.to_string().into_bytes()),
    }
}

fn raw_sqlite_value(row: &Row<'_>, index: usize) -> Option<RawSQLiteValue> {
    Some(match row.get_ref(index).ok()? {
        ValueRef::Null => RawSQLiteValue::Null,
        ValueRef::Integer(value) => RawSQLiteValue::Integer(value),
        ValueRef::Real(value) => RawSQLiteValue::Real(value),
        ValueRef::Text(value) => {
            RawSQLiteValue::TextBase64(base64::engine::general_purpose::STANDARD.encode(value))
        }
        ValueRef::Blob(value) => {
            RawSQLiteValue::BlobBase64(base64::engine::general_purpose::STANDARD.encode(value))
        }
    })
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn opaque_id(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    hex::encode(digest)
}

pub(crate) fn scoped_opaque_id(scope: &str, value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(scope.as_bytes());
    hasher.update([0]);
    hasher.update(value);
    hex::encode(hasher.finalize())
}

fn owner_only_writer(path: &Path) -> Result<BufWriter<File>, RestoreError> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    Ok(BufWriter::new(file))
}

fn create_owner_only_directory(path: &Path) -> Result<(), RestoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn write_owner_only_json(path: &Path, value: &impl serde::Serialize) -> Result<(), RestoreError> {
    let mut writer = owner_only_writer(path)?;
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}
