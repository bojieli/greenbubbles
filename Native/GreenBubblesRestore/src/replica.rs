use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use rusqlite::backup::Backup;
use rusqlite::{named_params, params, Connection, OpenFlags, Transaction, TransactionBehavior};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::archive::{ensure_private_directory, ensure_private_regular_file, load_report};
use crate::{
    CanonicalArtifact, CanonicalConversation, CanonicalMessage, CanonicalParticipant, ReplicaKey,
    RestorationCoverage, RestorationReport, RestoreError, TypedPayload,
};

const CURRENT_SCHEMA_VERSION: u32 = 4;
const REPLICA_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaBootstrapReport {
    pub format_version: u32,
    pub schema_version: u32,
    pub account_id: String,
    pub source_fingerprint: String,
    pub cipher_version: String,
    pub encrypted_at_rest: bool,
    pub idempotent: bool,
    pub conversation_count: u64,
    pub participant_count: u64,
    pub message_count: u64,
    pub artifact_count: u64,
    pub cached_moment_count: u64,
    pub cached_moment_interaction_count: u64,
    pub relationship_count: u64,
    pub message_artifact_count: u64,
    pub pre_migration_backup_file_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaStatus {
    pub format_version: u32,
    pub schema_version: u32,
    pub replica_id: String,
    pub account_id: Option<String>,
    pub current_source_fingerprint: Option<String>,
    pub checkpoint_revision: Option<String>,
    pub client_build_compatibility: Option<crate::ClientBuildCompatibilityEvidence>,
    pub acquisition_mode: Option<crate::SnapshotAcquisitionMode>,
    pub media_phase: Option<crate::RestorationMediaPhase>,
    pub decoder_name: Option<String>,
    pub decoder_version: Option<String>,
    pub cipher_version: String,
    pub encrypted_at_rest: bool,
    pub conversation_count: u64,
    pub participant_count: u64,
    pub message_count: u64,
    pub artifact_count: u64,
    pub cached_moment_count: u64,
    pub cached_moment_interaction_count: u64,
    pub last_checkpoint_unix_nanoseconds: Option<u128>,
    pub checkpoint_age_seconds: Option<u64>,
    pub last_sync_kind: Option<String>,
    pub last_sync_started_unix_nanoseconds: Option<u128>,
    pub last_sync_duration_milliseconds: Option<u64>,
    pub last_integrity_scan_unix_nanoseconds: Option<u128>,
    pub integrity_scan_age_seconds: Option<u64>,
    pub restoration_complete: Option<bool>,
    pub health: ReplicaHealthState,
    pub source_row_count: Option<u64>,
    pub restored_row_count: Option<u64>,
    pub semantic_gap_count: Option<u64>,
    pub message_candidate_gap_count: Option<u64>,
    pub unavailable_artifact_count: Option<u64>,
    pub artifact_decode_gap_count: Option<u64>,
    pub entity_decode_gap_count: Option<u64>,
    pub semantic_decode_coverage_ratio: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReplicaHealthState {
    Uninitialized,
    CurrentComplete,
    CurrentWithCoverageGaps,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaSyncReport {
    pub format_version: u32,
    pub account_id: String,
    pub previous_source_fingerprint: String,
    pub current_source_fingerprint: String,
    pub idempotent: bool,
    pub added_count: u64,
    pub changed_count: u64,
    pub removed_count: u64,
    pub conversation_count: u64,
    pub participant_count: u64,
    pub message_count: u64,
    pub artifact_count: u64,
    pub cached_moment_count: u64,
    pub cached_moment_interaction_count: u64,
    pub committed_at_unix_nanoseconds: Option<u128>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaChangeCursor {
    pub format_version: u32,
    pub account_id: String,
    pub replica_id: String,
    pub after_sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaChange {
    pub sequence: u64,
    pub source_fingerprint: String,
    pub change_kind: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub conversation_id: Option<String>,
    pub record_sha256: Option<String>,
    pub observed_at_unix_nanoseconds: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaChangePage {
    pub account_id: String,
    pub items: Vec<ReplicaChange>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaMessageFilter {
    pub conversation_id: Option<String>,
    pub sender_id: Option<String>,
    pub direction: Option<crate::MessageDirection>,
    pub logical_type: Option<u32>,
    pub sub_type: Option<u32>,
    pub not_before_unix: Option<i64>,
    pub not_after_unix: Option<i64>,
    pub reply_target_canonical_id: Option<String>,
    pub has_attachment: Option<bool>,
    pub full_text_query: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaMessageCursor {
    pub format_version: u32,
    pub account_id: String,
    pub replica_id: String,
    pub source_fingerprint: String,
    pub checkpoint_revision: String,
    pub filter_sha256: String,
    pub after_sort_time: i64,
    pub after_conversation_id: String,
    pub after_conversation_ordinal: u64,
    pub after_canonical_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaMessagePage {
    pub account_id: String,
    pub source_fingerprint: String,
    pub checkpoint_revision: String,
    pub items: Vec<CanonicalMessage>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaConversationPage {
    pub account_id: String,
    pub items: Vec<CanonicalConversation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaCoverageView {
    pub account_id: String,
    pub source_fingerprint: String,
    pub coverage: RestorationCoverage,
    pub integrity: crate::RestorationIntegrity,
    pub completion: crate::RestorationCompletion,
    pub cached_surfaces: Option<crate::CachedSurfaceCoverage>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaCachedMomentFilter {
    pub author_id: Option<String>,
    pub not_before_unix: Option<i64>,
    pub not_after_unix: Option<i64>,
    pub content_type: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaCachedMomentCursor {
    pub format_version: u32,
    pub account_id: String,
    pub replica_id: String,
    pub source_fingerprint: String,
    pub checkpoint_revision: String,
    pub filter_sha256: String,
    pub after_created_at_unix: i64,
    pub after_canonical_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReplicaCachedSurfaceAvailability {
    Unavailable,
    AvailableEmpty,
    Available,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaCachedMomentPage {
    pub account_id: String,
    pub source_fingerprint: String,
    pub checkpoint_revision: String,
    pub availability: ReplicaCachedSurfaceAvailability,
    pub cache_completeness: Option<crate::CachedSurfaceCompleteness>,
    pub observed_at: Option<String>,
    pub items: Vec<crate::CanonicalCachedMoment>,
    pub next_cursor: Option<String>,
}

struct OpenedReplica {
    connection: Connection,
    cipher_version: String,
    pre_migration_backup_file_name: Option<String>,
}

#[derive(Default)]
struct ImportCounts {
    conversations: u64,
    participants: u64,
    messages: u64,
    artifacts: u64,
    relationships: u64,
    message_artifacts: u64,
    cached_moments: u64,
    cached_moment_interactions: u64,
}

#[derive(Default)]
struct SyncCounts {
    added: u64,
    changed: u64,
    removed: u64,
}

#[derive(Default)]
struct SyncHealth {
    last_kind: Option<String>,
    last_started: Option<u128>,
    last_duration_milliseconds: Option<u64>,
    last_integrity_scan: Option<u128>,
}

struct CachedArchiveInputs {
    moments_path: PathBuf,
    interactions_path: PathBuf,
    coverage: crate::CachedSurfaceCoverage,
}

pub fn bootstrap_replica(
    archive_directory: &Path,
    replica_path: &Path,
    key: &ReplicaKey,
) -> Result<ReplicaBootstrapReport, RestoreError> {
    ensure_private_directory(archive_directory)?;
    let report = load_report(archive_directory)?;
    require_authoritative_archive(&report)?;
    let mut opened = open_replica(replica_path, key)?;
    let existing_account: Option<String> = opened
        .connection
        .query_row(
            "SELECT account_id FROM replica_identity WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if existing_account
        .as_deref()
        .is_some_and(|account| account != report.account_id)
    {
        return Err(RestoreError::Integrity(
            "replica belongs to a different account".to_string(),
        ));
    }
    let existing_checkpoint: Option<String> = opened
        .connection
        .query_row(
            "SELECT source_fingerprint FROM source_checkpoint WHERE account_id = ?1",
            [&report.account_id],
            |row| row.get(0),
        )
        .optional()?;
    if existing_checkpoint.as_deref() == Some(&report.source_fingerprint) {
        return bootstrap_report(&opened, &report, true);
    }
    if existing_checkpoint.is_some() {
        return Err(RestoreError::Integrity(
            "replica is already bootstrapped from another checkpoint; use synchronization"
                .to_string(),
        ));
    }

    let counts =
        import_archive_transactionally(&mut opened.connection, archive_directory, &report)?;
    checkpoint_and_secure(&opened.connection, replica_path)?;
    Ok(ReplicaBootstrapReport {
        format_version: REPLICA_FORMAT_VERSION,
        schema_version: CURRENT_SCHEMA_VERSION,
        account_id: report.account_id,
        source_fingerprint: report.source_fingerprint,
        cipher_version: opened.cipher_version,
        encrypted_at_rest: true,
        idempotent: false,
        conversation_count: counts.conversations,
        participant_count: counts.participants,
        message_count: counts.messages,
        artifact_count: counts.artifacts,
        cached_moment_count: counts.cached_moments,
        cached_moment_interaction_count: counts.cached_moment_interactions,
        relationship_count: counts.relationships,
        message_artifact_count: counts.message_artifacts,
        pre_migration_backup_file_name: opened.pre_migration_backup_file_name,
    })
}

pub fn replica_status(
    replica_path: &Path,
    key: &ReplicaKey,
) -> Result<ReplicaStatus, RestoreError> {
    let opened = open_replica(replica_path, key)?;
    let identity = opened
        .connection
        .query_row(
            "SELECT account_id, current_source_fingerprint, restoration_complete,
                    updated_at_unix_nanoseconds
             FROM replica_identity WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<bool>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let checkpoint = if let Some((account, _, _, _)) = identity.as_ref() {
        let encoded = opened
            .connection
            .query_row(
                "SELECT committed_at_unix_nanoseconds FROM source_checkpoint
                 WHERE account_id = ?1",
                [account],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        encoded
            .map(|value| {
                value.parse::<u128>().map_err(|_| {
                    RestoreError::Integrity("replica checkpoint timestamp is invalid".to_string())
                })
            })
            .transpose()?
    } else {
        None
    };
    let stored_state = match identity.as_ref() {
        Some((account, _, _, _)) => load_coverage_state(&opened.connection, account)?,
        None => None,
    };
    let stored_report = stored_state.as_ref().map(|value| &value.0);
    let stored_coverage = stored_state.as_ref().map(|value| &value.1);
    let checkpoint_age_seconds = checkpoint
        .map(|timestamp| {
            unix_nanoseconds()
                .map(|now| now.saturating_sub(timestamp) / 1_000_000_000)
                .and_then(|seconds| {
                    u64::try_from(seconds).map_err(|_| {
                        RestoreError::Integrity(
                            "checkpoint age exceeds supported range".to_string(),
                        )
                    })
                })
        })
        .transpose()?;
    let semantic_decode_coverage_ratio = stored_report.and_then(|report| {
        (report.integrity.restored_row_count > 0).then(|| {
            let covered = report
                .integrity
                .restored_row_count
                .saturating_sub(report.integrity.semantic_gap_count);
            covered as f64 / report.integrity.restored_row_count as f64
        })
    });
    let health = match stored_report {
        None => ReplicaHealthState::Uninitialized,
        Some(report) if report.completion.full_restoration_achieved => {
            ReplicaHealthState::CurrentComplete
        }
        Some(_) => ReplicaHealthState::CurrentWithCoverageGaps,
    };
    let sync_health =
        load_sync_health(&opened.connection, identity.as_ref().map(|value| &value.0))?;
    let integrity_scan_age_seconds = sync_health
        .last_integrity_scan
        .map(age_seconds)
        .transpose()?;
    Ok(ReplicaStatus {
        format_version: REPLICA_FORMAT_VERSION,
        schema_version: CURRENT_SCHEMA_VERSION,
        replica_id: replica_id(&opened.connection)?,
        account_id: identity.as_ref().map(|value| value.0.clone()),
        current_source_fingerprint: identity.as_ref().and_then(|value| value.1.clone()),
        checkpoint_revision: identity.as_ref().map(|value| value.3.clone()),
        client_build_compatibility: stored_report
            .map(|report| report.client_build_compatibility.clone()),
        acquisition_mode: stored_report
            .and_then(|report| report.acquisition.as_ref().map(|value| value.mode)),
        media_phase: stored_report.map(|report| report.media_phase),
        decoder_name: stored_coverage.map(|coverage| coverage.decoder_name.clone()),
        decoder_version: stored_coverage.map(|coverage| coverage.decoder_version.clone()),
        cipher_version: opened.cipher_version,
        encrypted_at_rest: true,
        conversation_count: table_count(&opened.connection, "conversation")?,
        participant_count: table_count(&opened.connection, "participant")?,
        message_count: table_count(&opened.connection, "message")?,
        artifact_count: table_count(&opened.connection, "artifact")?,
        cached_moment_count: table_count(&opened.connection, "cached_moment")?,
        cached_moment_interaction_count: table_count(
            &opened.connection,
            "cached_moment_interaction",
        )?,
        last_checkpoint_unix_nanoseconds: checkpoint,
        checkpoint_age_seconds,
        last_sync_kind: sync_health.last_kind,
        last_sync_started_unix_nanoseconds: sync_health.last_started,
        last_sync_duration_milliseconds: sync_health.last_duration_milliseconds,
        last_integrity_scan_unix_nanoseconds: sync_health.last_integrity_scan,
        integrity_scan_age_seconds,
        restoration_complete: identity.and_then(|value| value.2),
        health,
        source_row_count: stored_report.map(|report| report.integrity.source_row_count),
        restored_row_count: stored_report.map(|report| report.integrity.restored_row_count),
        semantic_gap_count: stored_report.map(|report| report.integrity.semantic_gap_count),
        message_candidate_gap_count: stored_report
            .map(|report| report.integrity.message_candidate_gap_count),
        unavailable_artifact_count: stored_report
            .map(|report| report.integrity.missing_artifact_count),
        artifact_decode_gap_count: stored_report
            .map(|report| report.integrity.artifact_decode_gap_count),
        entity_decode_gap_count: stored_report
            .map(|report| report.integrity.entity_decode_gap_count),
        semantic_decode_coverage_ratio,
    })
}

pub fn synchronize_replica(
    archive_directory: &Path,
    replica_path: &Path,
    key: &ReplicaKey,
) -> Result<ReplicaSyncReport, RestoreError> {
    ensure_private_directory(archive_directory)?;
    let report = load_report(archive_directory)?;
    require_authoritative_archive(&report)?;
    let mut opened = open_replica(replica_path, key)?;
    let identity: Option<(String, Option<String>)> = opened
        .connection
        .query_row(
            "SELECT account_id, current_source_fingerprint
             FROM replica_identity WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((account_id, previous_fingerprint)) = identity else {
        return Err(RestoreError::Integrity(
            "replica must be bootstrapped before synchronization".to_string(),
        ));
    };
    require_account(&report.account_id, &account_id)?;
    let previous_fingerprint = previous_fingerprint.ok_or_else(|| {
        RestoreError::Integrity("replica has no authoritative source checkpoint".to_string())
    })?;
    let incoming_coverage = load_archive_coverage(archive_directory)?;
    let stored_state = load_coverage_state(&opened.connection, &account_id)?;
    let unchanged_revision =
        stored_state
            .as_ref()
            .is_some_and(|(stored_report, stored_coverage)| {
                archive_revision_digest(stored_report, stored_coverage)
                    == archive_revision_digest(&report, &incoming_coverage)
            });
    if previous_fingerprint == report.source_fingerprint && unchanged_revision {
        return sync_report(
            &opened.connection,
            &account_id,
            &previous_fingerprint,
            &report.source_fingerprint,
            true,
            SyncCounts::default(),
            None,
        );
    }
    let (counts, committed) = reconcile_archive_transactionally(
        &mut opened.connection,
        archive_directory,
        &report,
        &previous_fingerprint,
    )?;
    checkpoint_and_secure(&opened.connection, replica_path)?;
    sync_report(
        &opened.connection,
        &account_id,
        &previous_fingerprint,
        &report.source_fingerprint,
        false,
        counts,
        Some(committed),
    )
}

pub fn get_replica_changes(
    replica_path: &Path,
    key: &ReplicaKey,
    cursor: Option<&str>,
    requested_limit: usize,
) -> Result<ReplicaChangePage, RestoreError> {
    let opened = open_replica(replica_path, key)?;
    let account_id: String = opened
        .connection
        .query_row(
            "SELECT account_id FROM replica_identity WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => RestoreError::Integrity(
                "replica must be bootstrapped before reading changes".to_string(),
            ),
            other => other.into(),
        })?;
    let replica_id = replica_id(&opened.connection)?;
    let decoded = cursor.map(decode_change_cursor).transpose()?;
    if decoded.as_ref().is_some_and(|cursor| {
        cursor.format_version != 1
            || cursor.account_id != account_id
            || cursor.replica_id != replica_id
    }) {
        return Err(RestoreError::Integrity(
            "change cursor belongs to another account or format".to_string(),
        ));
    }
    let after = decoded.map(|cursor| cursor.after_sequence).unwrap_or(0);
    let limit = requested_limit.clamp(1, 1_000);
    let query_limit = checked_usize_i64(limit)?;
    let mut statement = opened.connection.prepare(
        "SELECT sequence, source_fingerprint, change_kind, entity_kind, entity_id,
                conversation_id, record_sha256, observed_at_unix_nanoseconds
         FROM change_log
         WHERE account_id = ?1 AND sequence > ?2
         ORDER BY sequence LIMIT ?3",
    )?;
    let values = statement
        .query_map(
            params![account_id, checked_i64(after)?, query_limit],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let mut items = Vec::with_capacity(values.len());
    for (sequence, source, kind, entity_kind, entity_id, conversation, digest, timestamp) in values
    {
        items.push(ReplicaChange {
            sequence: u64::try_from(sequence).map_err(|_| {
                RestoreError::Integrity(
                    "change sequence is outside the supported range".to_string(),
                )
            })?,
            source_fingerprint: source,
            change_kind: kind,
            entity_kind,
            entity_id,
            conversation_id: conversation,
            record_sha256: digest,
            observed_at_unix_nanoseconds: timestamp
                .parse()
                .map_err(|_| RestoreError::Integrity("change timestamp is invalid".to_string()))?,
        });
    }
    let next_cursor = items.last().map(|change| {
        encode_change_cursor(&ReplicaChangeCursor {
            format_version: 1,
            account_id: account_id.clone(),
            replica_id,
            after_sequence: change.sequence,
        })
    });
    Ok(ReplicaChangePage {
        account_id,
        items,
        next_cursor,
    })
}

pub fn search_replica_messages(
    replica_path: &Path,
    key: &ReplicaKey,
    filter: &ReplicaMessageFilter,
    cursor: Option<&str>,
    requested_limit: usize,
) -> Result<ReplicaMessagePage, RestoreError> {
    validate_message_filter(filter)?;
    let opened = open_replica(replica_path, key)?;
    let (account_id, source_fingerprint, checkpoint_revision) =
        current_replica_checkpoint(&opened.connection)?;
    let generation = replica_id(&opened.connection)?;
    let filter_sha256 = sha256(&serde_json::to_vec(filter)?);
    let decoded = cursor.map(decode_message_cursor).transpose()?;
    if decoded.as_ref().is_some_and(|cursor| {
        cursor.format_version != 2
            || cursor.account_id != account_id
            || cursor.replica_id != generation
            || cursor.source_fingerprint != source_fingerprint
            || cursor.checkpoint_revision != checkpoint_revision
            || cursor.filter_sha256 != filter_sha256
    }) {
        return Err(RestoreError::Integrity(
            "message cursor belongs to another replica checkpoint or query".to_string(),
        ));
    }
    let after_present = decoded.is_some();
    let after_sort_time = decoded
        .as_ref()
        .map(|cursor| cursor.after_sort_time)
        .unwrap_or(i64::MIN);
    let after_conversation = decoded
        .as_ref()
        .map(|cursor| cursor.after_conversation_id.as_str())
        .unwrap_or("");
    let after_ordinal = decoded
        .as_ref()
        .map(|cursor| checked_i64(cursor.after_conversation_ordinal))
        .transpose()?
        .unwrap_or(0);
    let after_canonical = decoded
        .as_ref()
        .map(|cursor| cursor.after_canonical_id.as_str())
        .unwrap_or("");
    let direction = filter.direction.as_ref().map(json_enum).transpose()?;
    let logical_type = filter.logical_type.map(i64::from);
    let sub_type = filter.sub_type.map(i64::from);
    let limit = requested_limit.clamp(1, 1_000);
    let query_limit = checked_usize_i64(limit.saturating_add(1))?;
    let mut statement = opened.connection.prepare(
        "SELECT m.record_json
         FROM message AS m
         WHERE m.account_id = :account
           AND (:conversation IS NULL OR m.conversation_id = :conversation)
           AND (:sender IS NULL OR m.sender_id = :sender)
           AND (:direction IS NULL OR m.direction = :direction)
           AND (:logical_type IS NULL OR m.logical_type = :logical_type)
           AND (:sub_type IS NULL OR m.sub_type = :sub_type)
           AND (:not_before IS NULL OR m.created_at_unix >= :not_before)
           AND (:not_after IS NULL OR m.created_at_unix <= :not_after)
           AND (:reply_target IS NULL OR EXISTS(
             SELECT 1 FROM message_relationship AS r
             WHERE r.account_id = m.account_id
               AND r.source_canonical_id = m.canonical_id
               AND r.target_canonical_id = :reply_target
           ))
           AND (
             :has_attachment IS NULL
             OR (:has_attachment = 1 AND EXISTS(
               SELECT 1 FROM message_artifact AS a
               WHERE a.account_id = m.account_id AND a.canonical_id = m.canonical_id
             ))
             OR (:has_attachment = 0 AND NOT EXISTS(
               SELECT 1 FROM message_artifact AS a
               WHERE a.account_id = m.account_id AND a.canonical_id = m.canonical_id
             ))
           )
           AND (:full_text IS NULL OR EXISTS(
             SELECT 1 FROM message_fts
             WHERE message_fts.account_id = m.account_id
               AND message_fts.canonical_id = m.canonical_id
               AND message_fts MATCH :full_text
           ))
           AND (
             :after_present = 0
             OR COALESCE(m.created_at_unix, -9223372036854775808) > :after_time
             OR (
               COALESCE(m.created_at_unix, -9223372036854775808) = :after_time
               AND m.conversation_id > :after_conversation
             )
             OR (
               COALESCE(m.created_at_unix, -9223372036854775808) = :after_time
               AND m.conversation_id = :after_conversation
               AND m.conversation_ordinal > :after_ordinal
             )
             OR (
               COALESCE(m.created_at_unix, -9223372036854775808) = :after_time
               AND m.conversation_id = :after_conversation
               AND m.conversation_ordinal = :after_ordinal
               AND m.canonical_id > :after_canonical
             )
           )
         ORDER BY COALESCE(m.created_at_unix, -9223372036854775808),
                  m.conversation_id, m.conversation_ordinal, m.canonical_id
         LIMIT :limit",
    )?;
    let rows = statement.query_map(
        named_params! {
            ":account": account_id,
            ":conversation": filter.conversation_id,
            ":sender": filter.sender_id,
            ":direction": direction,
            ":logical_type": logical_type,
            ":sub_type": sub_type,
            ":not_before": filter.not_before_unix,
            ":not_after": filter.not_after_unix,
            ":reply_target": filter.reply_target_canonical_id,
            ":has_attachment": filter.has_attachment,
            ":full_text": filter.full_text_query,
            ":after_present": after_present,
            ":after_time": after_sort_time,
            ":after_conversation": after_conversation,
            ":after_ordinal": after_ordinal,
            ":after_canonical": after_canonical,
            ":limit": query_limit,
        },
        |row| row.get::<_, Vec<u8>>(0),
    )?;
    let mut items = rows
        .map(|row| -> Result<CanonicalMessage, RestoreError> {
            let message: CanonicalMessage = serde_json::from_slice(&row?)?;
            require_account(&message.account_id, &account_id)?;
            Ok(message)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if has_more {
        items.last().map(|message| {
            encode_message_cursor(&ReplicaMessageCursor {
                format_version: 2,
                account_id: account_id.clone(),
                replica_id: generation,
                source_fingerprint: source_fingerprint.clone(),
                checkpoint_revision: checkpoint_revision.clone(),
                filter_sha256,
                after_sort_time: message.created_at_unix.unwrap_or(i64::MIN),
                after_conversation_id: message.conversation_id.clone(),
                after_conversation_ordinal: message.conversation_ordinal,
                after_canonical_id: message.canonical_id.clone(),
            })
        })
    } else {
        None
    };
    Ok(ReplicaMessagePage {
        account_id,
        source_fingerprint,
        checkpoint_revision,
        items,
        next_cursor,
    })
}

pub fn search_replica_cached_moments(
    replica_path: &Path,
    key: &ReplicaKey,
    filter: &ReplicaCachedMomentFilter,
    cursor: Option<&str>,
    requested_limit: usize,
) -> Result<ReplicaCachedMomentPage, RestoreError> {
    validate_cached_moment_filter(filter)?;
    let opened = open_replica(replica_path, key)?;
    let (account_id, source_fingerprint, checkpoint_revision) =
        current_replica_checkpoint(&opened.connection)?;
    let generation = replica_id(&opened.connection)?;
    let filter_sha256 = sha256(&serde_json::to_vec(filter)?);
    let decoded = cursor.map(decode_cached_moment_cursor).transpose()?;
    if decoded.as_ref().is_some_and(|cursor| {
        cursor.format_version != 1
            || cursor.account_id != account_id
            || cursor.replica_id != generation
            || cursor.source_fingerprint != source_fingerprint
            || cursor.checkpoint_revision != checkpoint_revision
            || cursor.filter_sha256 != filter_sha256
    }) {
        return Err(RestoreError::Integrity(
            "cached-moment cursor belongs to another replica checkpoint or query".to_string(),
        ));
    }

    let coverage: Option<Vec<u8>> = opened
        .connection
        .query_row(
            "SELECT coverage_json FROM cached_surface_state WHERE account_id = ?1",
            [&account_id],
            |row| row.get(0),
        )
        .optional()?;
    let coverage: Option<crate::CachedSurfaceCoverage> = coverage
        .map(|bytes| serde_json::from_slice(&bytes))
        .transpose()?;
    let availability = match coverage.as_ref() {
        None => ReplicaCachedSurfaceAvailability::Unavailable,
        Some(coverage) if !coverage.source_database_present => {
            ReplicaCachedSurfaceAvailability::Unavailable
        }
        Some(coverage) if coverage.moment_count == 0 => {
            ReplicaCachedSurfaceAvailability::AvailableEmpty
        }
        Some(_) => ReplicaCachedSurfaceAvailability::Available,
    };
    if availability == ReplicaCachedSurfaceAvailability::Unavailable {
        if decoded.is_some() {
            return Err(RestoreError::Integrity(
                "cached-moment cursor cannot be resumed because the cached surface is unavailable"
                    .to_string(),
            ));
        }
        return Ok(ReplicaCachedMomentPage {
            account_id,
            source_fingerprint,
            checkpoint_revision,
            availability,
            cache_completeness: coverage
                .as_ref()
                .map(|coverage| coverage.cache_completeness),
            observed_at: coverage.map(|coverage| coverage.observed_at),
            items: Vec::new(),
            next_cursor: None,
        });
    }

    let after_present = decoded.is_some();
    let after_created_at = decoded
        .as_ref()
        .map(|cursor| cursor.after_created_at_unix)
        .unwrap_or(i64::MIN);
    let after_canonical_id = decoded
        .as_ref()
        .map(|cursor| cursor.after_canonical_id.as_str())
        .unwrap_or("");
    let limit = requested_limit.clamp(1, 1_000);
    let query_limit = checked_usize_i64(limit.saturating_add(1))?;
    let mut statement = opened.connection.prepare(
        "SELECT record_json FROM cached_moment
         WHERE account_id = :account
           AND (:author IS NULL OR author_id = :author)
           AND (:not_before IS NULL OR created_at_unix >= :not_before)
           AND (:not_after IS NULL OR created_at_unix <= :not_after)
           AND (:content_type IS NULL OR content_type = :content_type)
           AND (
             :after_present = 0
             OR COALESCE(created_at_unix, -9223372036854775808) > :after_created_at
             OR (
               COALESCE(created_at_unix, -9223372036854775808) = :after_created_at
               AND canonical_id > :after_canonical_id
             )
           )
         ORDER BY COALESCE(created_at_unix, -9223372036854775808), canonical_id
         LIMIT :limit",
    )?;
    let rows = statement.query_map(
        named_params! {
            ":account": account_id,
            ":author": filter.author_id,
            ":not_before": filter.not_before_unix,
            ":not_after": filter.not_after_unix,
            ":content_type": filter.content_type,
            ":after_present": after_present,
            ":after_created_at": after_created_at,
            ":after_canonical_id": after_canonical_id,
            ":limit": query_limit,
        },
        |row| row.get::<_, Vec<u8>>(0),
    )?;
    let mut items = rows
        .map(
            |row| -> Result<crate::CanonicalCachedMoment, RestoreError> {
                let moment: crate::CanonicalCachedMoment = serde_json::from_slice(&row?)?;
                require_account(&moment.account_id, &account_id)?;
                Ok(moment)
            },
        )
        .collect::<Result<Vec<_>, _>>()?;
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if has_more {
        items.last().map(|moment| {
            encode_cached_moment_cursor(&ReplicaCachedMomentCursor {
                format_version: 1,
                account_id: account_id.clone(),
                replica_id: generation,
                source_fingerprint: source_fingerprint.clone(),
                checkpoint_revision: checkpoint_revision.clone(),
                filter_sha256,
                after_created_at_unix: moment.created_at_unix.unwrap_or(i64::MIN),
                after_canonical_id: moment.canonical_id.clone(),
            })
        })
    } else {
        None
    };
    Ok(ReplicaCachedMomentPage {
        account_id,
        source_fingerprint,
        checkpoint_revision,
        availability,
        cache_completeness: coverage
            .as_ref()
            .map(|coverage| coverage.cache_completeness),
        observed_at: coverage.map(|coverage| coverage.observed_at),
        items,
        next_cursor,
    })
}

pub fn load_replica_message_filter(path: &Path) -> Result<ReplicaMessageFilter, RestoreError> {
    ensure_private_regular_file(path)?;
    let filter: ReplicaMessageFilter = serde_json::from_slice(&fs::read(path)?)?;
    validate_message_filter(&filter)?;
    Ok(filter)
}

pub fn get_replica_message(
    replica_path: &Path,
    key: &ReplicaKey,
    canonical_id: &str,
) -> Result<Option<CanonicalMessage>, RestoreError> {
    if canonical_id.is_empty() {
        return Err(RestoreError::Integrity(
            "canonical message ID cannot be empty".to_string(),
        ));
    }
    let opened = open_replica(replica_path, key)?;
    let (account_id, _) = current_replica_identity(&opened.connection)?;
    let bytes: Option<Vec<u8>> = opened
        .connection
        .query_row(
            "SELECT record_json FROM message
             WHERE account_id = ?1 AND canonical_id = ?2",
            params![account_id, canonical_id],
            |row| row.get(0),
        )
        .optional()?;
    bytes
        .map(|bytes| {
            let message: CanonicalMessage = serde_json::from_slice(&bytes)?;
            require_account(&message.account_id, &account_id)?;
            Ok(message)
        })
        .transpose()
}

pub fn get_replica_recent_messages(
    replica_path: &Path,
    key: &ReplicaKey,
    conversation_id: &str,
    not_before_unix: Option<i64>,
    not_after_unix: Option<i64>,
    requested_limit: usize,
) -> Result<Vec<CanonicalMessage>, RestoreError> {
    if conversation_id.is_empty() {
        return Err(RestoreError::Integrity(
            "conversation ID cannot be empty".to_string(),
        ));
    }
    let opened = open_replica(replica_path, key)?;
    let (account_id, _) = current_replica_identity(&opened.connection)?;
    let limit = checked_usize_i64(requested_limit.clamp(1, 1_000))?;
    let mut statement = opened.connection.prepare(
        "SELECT record_json FROM message
         WHERE account_id = ?1 AND conversation_id = ?2
           AND (?3 IS NULL OR created_at_unix >= ?3)
           AND (?4 IS NULL OR created_at_unix <= ?4)
         ORDER BY conversation_ordinal DESC, canonical_id DESC LIMIT ?5",
    )?;
    let rows = statement.query_map(
        params![
            account_id,
            conversation_id,
            not_before_unix,
            not_after_unix,
            limit
        ],
        |row| row.get::<_, Vec<u8>>(0),
    )?;
    let mut messages = rows
        .map(|row| -> Result<CanonicalMessage, RestoreError> {
            let message: CanonicalMessage = serde_json::from_slice(&row?)?;
            require_account(&message.account_id, &account_id)?;
            Ok(message)
        })
        .collect::<Result<Vec<_>, _>>()?;
    messages.reverse();
    Ok(messages)
}

pub fn get_replica_conversation(
    replica_path: &Path,
    key: &ReplicaKey,
    conversation_id: &str,
) -> Result<Option<CanonicalConversation>, RestoreError> {
    get_replica_record(
        replica_path,
        key,
        "conversation",
        "conversation_id",
        conversation_id,
    )
}

pub fn get_replica_participant(
    replica_path: &Path,
    key: &ReplicaKey,
    participant_id: &str,
) -> Result<Option<CanonicalParticipant>, RestoreError> {
    get_replica_record(
        replica_path,
        key,
        "participant",
        "participant_id",
        participant_id,
    )
}

pub fn get_replica_artifact(
    replica_path: &Path,
    key: &ReplicaKey,
    artifact_id: &str,
) -> Result<Option<CanonicalArtifact>, RestoreError> {
    get_replica_record(replica_path, key, "artifact", "artifact_id", artifact_id)
}

pub fn replica_conversation_references_artifact(
    replica_path: &Path,
    key: &ReplicaKey,
    conversation_id: &str,
    artifact_id: &str,
) -> Result<bool, RestoreError> {
    if conversation_id.is_empty() || artifact_id.is_empty() {
        return Err(RestoreError::Integrity(
            "conversation and artifact IDs cannot be empty".to_string(),
        ));
    }
    let opened = open_replica(replica_path, key)?;
    let (account_id, _) = current_replica_identity(&opened.connection)?;
    let exists: bool = opened.connection.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM message_artifact AS a
           JOIN message AS m
             ON m.account_id = a.account_id AND m.canonical_id = a.canonical_id
           WHERE a.account_id = ?1 AND m.conversation_id = ?2 AND a.artifact_id = ?3
         )",
        params![account_id, conversation_id, artifact_id],
        |row| row.get(0),
    )?;
    Ok(exists)
}

fn get_replica_record<T: DeserializeOwned>(
    replica_path: &Path,
    key: &ReplicaKey,
    table: &str,
    identifier_column: &str,
    identifier: &str,
) -> Result<Option<T>, RestoreError> {
    if identifier.is_empty() {
        return Err(RestoreError::Integrity(
            "replica record identifier cannot be empty".to_string(),
        ));
    }
    debug_assert!(matches!(table, "conversation" | "participant" | "artifact"));
    debug_assert!(matches!(
        identifier_column,
        "conversation_id" | "participant_id" | "artifact_id"
    ));
    let opened = open_replica(replica_path, key)?;
    let (account_id, _) = current_replica_identity(&opened.connection)?;
    let query = format!(
        "SELECT record_json FROM {table}
         WHERE account_id = ?1 AND {identifier_column} = ?2"
    );
    let bytes: Option<Vec<u8>> = opened
        .connection
        .query_row(&query, params![account_id, identifier], |row| row.get(0))
        .optional()?;
    bytes
        .map(|bytes| Ok(serde_json::from_slice(&bytes)?))
        .transpose()
}

pub fn list_replica_conversations(
    replica_path: &Path,
    key: &ReplicaKey,
    requested_limit: usize,
) -> Result<ReplicaConversationPage, RestoreError> {
    let opened = open_replica(replica_path, key)?;
    let (account_id, _) = current_replica_identity(&opened.connection)?;
    let limit = checked_usize_i64(requested_limit.clamp(1, 1_000))?;
    let mut statement = opened.connection.prepare(
        "SELECT record_json FROM conversation
         WHERE account_id = ?1 ORDER BY conversation_id LIMIT ?2",
    )?;
    let rows = statement.query_map(params![account_id, limit], |row| row.get::<_, Vec<u8>>(0))?;
    let items = rows
        .map(|row| -> Result<CanonicalConversation, RestoreError> {
            let conversation: CanonicalConversation = serde_json::from_slice(&row?)?;
            require_account(&conversation.account_id, &account_id)?;
            Ok(conversation)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ReplicaConversationPage { account_id, items })
}

pub fn replica_coverage(
    replica_path: &Path,
    key: &ReplicaKey,
) -> Result<ReplicaCoverageView, RestoreError> {
    let opened = open_replica(replica_path, key)?;
    let (account_id, source_fingerprint) = current_replica_identity(&opened.connection)?;
    let (coverage, report): (Vec<u8>, Vec<u8>) = opened.connection.query_row(
        "SELECT coverage_json, report_json FROM coverage_state WHERE account_id = ?1",
        [&account_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let coverage: RestorationCoverage = serde_json::from_slice(&coverage)?;
    let report: RestorationReport = serde_json::from_slice(&report)?;
    let cached_surfaces: Option<Vec<u8>> = opened
        .connection
        .query_row(
            "SELECT coverage_json FROM cached_surface_state WHERE account_id = ?1",
            [&account_id],
            |row| row.get(0),
        )
        .optional()?;
    let cached_surfaces = cached_surfaces
        .map(|bytes| serde_json::from_slice(&bytes))
        .transpose()?;
    require_account(&report.account_id, &account_id)?;
    Ok(ReplicaCoverageView {
        account_id,
        source_fingerprint,
        coverage,
        integrity: report.integrity,
        completion: report.completion,
        cached_surfaces,
    })
}

fn open_replica(path: &Path, key: &ReplicaKey) -> Result<OpenedReplica, RestoreError> {
    let existed = path.try_exists()?;
    if existed {
        ensure_private_regular_file(path)?;
    } else {
        let parent = path
            .parent()
            .ok_or_else(|| RestoreError::UnsafePath("replica has no parent".to_string()))?;
        ensure_private_directory(parent)?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)?;
    }
    let result = (|| {
        let mut connection = open_keyed_connection(path, key)?;
        let version = schema_version(&connection)?;
        if version > CURRENT_SCHEMA_VERSION {
            return Err(RestoreError::Integrity(format!(
                "replica schema version {version} is newer than supported version {CURRENT_SCHEMA_VERSION}"
            )));
        }
        let pre_migration_backup_file_name = if version > 0 && version < CURRENT_SCHEMA_VERSION {
            Some(create_pre_migration_backup(
                &connection,
                path,
                key,
                version,
            )?)
        } else {
            None
        };
        apply_migrations(&mut connection, version)?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             PRAGMA wal_autocheckpoint = 1000;",
        )?;
        let cipher_version =
            connection.pragma_query_value(None, "cipher_version", |row| row.get::<_, String>(0))?;
        if cipher_version.is_empty() {
            return Err(RestoreError::Integrity(
                "replica SQLite build does not provide SQLCipher".to_string(),
            ));
        }
        secure_replica_files(path)?;
        Ok(OpenedReplica {
            connection,
            cipher_version,
            pre_migration_backup_file_name,
        })
    })();
    if result.is_err() && !existed {
        remove_failed_replica_files(path);
    }
    result
}

fn open_keyed_connection(path: &Path, key: &ReplicaKey) -> Result<Connection, RestoreError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut key_hex = hex::encode(key.expose_for_replica_operation());
    let key_statement = Zeroizing::new(format!("PRAGMA key = \"x'{key_hex}'\";"));
    key_hex.zeroize();
    connection.execute_batch(
        "PRAGMA cipher_compatibility = 4;
         PRAGMA cipher_memory_security = ON;",
    )?;
    connection.execute_batch(&key_statement)?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA trusted_schema = OFF;
         PRAGMA temp_store = MEMORY;
         PRAGMA secure_delete = ON;",
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))?;
    Ok(connection)
}

fn schema_version(connection: &Connection) -> Result<u32, RestoreError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'replica_schema'
         )",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(0);
    }
    let version: i64 = connection.query_row(
        "SELECT schema_version FROM replica_schema WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    u32::try_from(version).map_err(|_| {
        RestoreError::Integrity("replica schema version is outside the supported range".to_string())
    })
}

fn apply_migrations(connection: &mut Connection, from: u32) -> Result<(), RestoreError> {
    for version in (from + 1)..=CURRENT_SCHEMA_VERSION {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        match version {
            1 => migration_1(&transaction)?,
            2 => migration_2(&transaction)?,
            3 => migration_3(&transaction)?,
            4 => migration_4(&transaction)?,
            _ => unreachable!("all replica migrations are enumerated"),
        }
        transaction.commit()?;
    }
    Ok(())
}

fn migration_1(transaction: &Transaction<'_>) -> Result<(), RestoreError> {
    transaction.execute_batch(
        "CREATE TABLE replica_schema(
           singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
           schema_version INTEGER NOT NULL,
           replica_format_version INTEGER NOT NULL
         );
         INSERT INTO replica_schema VALUES (1, 1, 1);
         CREATE TABLE migration_history(
           schema_version INTEGER PRIMARY KEY,
           applied_at_unix_nanoseconds TEXT NOT NULL,
           migration_sha256 TEXT NOT NULL
         );
         CREATE TABLE replica_identity(
           singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
           account_id TEXT NOT NULL UNIQUE,
           current_source_fingerprint TEXT,
           restoration_complete INTEGER,
           created_at_unix_nanoseconds TEXT NOT NULL,
           updated_at_unix_nanoseconds TEXT NOT NULL
         );
         CREATE TABLE conversation(
           account_id TEXT NOT NULL,
           conversation_id TEXT NOT NULL,
           kind TEXT NOT NULL,
           entity_decode_state TEXT NOT NULL,
           participant_count INTEGER NOT NULL,
           record_sha256 TEXT NOT NULL,
           record_json BLOB NOT NULL,
           PRIMARY KEY(account_id, conversation_id),
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE participant(
           account_id TEXT NOT NULL,
           participant_id TEXT NOT NULL,
           local_profile_state TEXT NOT NULL,
           record_sha256 TEXT NOT NULL,
           record_json BLOB NOT NULL,
           PRIMARY KEY(account_id, participant_id),
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE conversation_participant(
           account_id TEXT NOT NULL,
           conversation_id TEXT NOT NULL,
           participant_id TEXT NOT NULL,
           membership_role TEXT NOT NULL,
           display_name_base64 TEXT,
           PRIMARY KEY(account_id, conversation_id, participant_id, membership_role),
           FOREIGN KEY(account_id, conversation_id)
             REFERENCES conversation(account_id, conversation_id) ON DELETE CASCADE,
           FOREIGN KEY(account_id, participant_id)
             REFERENCES participant(account_id, participant_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE artifact(
           account_id TEXT NOT NULL,
           artifact_id TEXT NOT NULL,
           kind TEXT NOT NULL,
           role TEXT NOT NULL,
           availability TEXT NOT NULL,
           source_sha256 TEXT,
           decoded_sha256 TEXT,
           record_sha256 TEXT NOT NULL,
           record_json BLOB NOT NULL,
           PRIMARY KEY(account_id, artifact_id),
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE message(
           account_id TEXT NOT NULL,
           canonical_id TEXT NOT NULL,
           conversation_id TEXT NOT NULL,
           sender_id TEXT,
           conversation_ordinal INTEGER NOT NULL,
           created_at_unix INTEGER,
           direction TEXT NOT NULL,
           logical_type INTEGER,
           sub_type INTEGER,
           semantic_decode_state TEXT NOT NULL,
           search_text TEXT NOT NULL,
           record_sha256 TEXT NOT NULL,
           record_json BLOB NOT NULL,
           PRIMARY KEY(account_id, canonical_id),
           UNIQUE(account_id, conversation_id, conversation_ordinal),
           FOREIGN KEY(account_id, conversation_id)
             REFERENCES conversation(account_id, conversation_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE message_relationship(
           account_id TEXT NOT NULL,
           source_canonical_id TEXT NOT NULL,
           relationship_ordinal INTEGER NOT NULL,
           kind TEXT NOT NULL,
           target_canonical_id TEXT,
           resolved INTEGER NOT NULL,
           record_json BLOB NOT NULL,
           PRIMARY KEY(account_id, source_canonical_id, relationship_ordinal),
           FOREIGN KEY(account_id, source_canonical_id)
             REFERENCES message(account_id, canonical_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE message_artifact(
           account_id TEXT NOT NULL,
           canonical_id TEXT NOT NULL,
           artifact_ordinal INTEGER NOT NULL,
           artifact_id TEXT NOT NULL,
           role TEXT NOT NULL,
           preferred INTEGER NOT NULL,
           PRIMARY KEY(account_id, canonical_id, artifact_ordinal),
           FOREIGN KEY(account_id, canonical_id)
             REFERENCES message(account_id, canonical_id) ON DELETE CASCADE,
           FOREIGN KEY(account_id, artifact_id)
             REFERENCES artifact(account_id, artifact_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE coverage_state(
           account_id TEXT PRIMARY KEY,
           source_fingerprint TEXT NOT NULL,
           coverage_json BLOB NOT NULL,
           report_json BLOB NOT NULL,
           full_restoration_achieved INTEGER NOT NULL,
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         ) WITHOUT ROWID;",
    )?;
    record_migration(transaction, 1, "canonical replica base schema")?;
    Ok(())
}

fn migration_2(transaction: &Transaction<'_>) -> Result<(), RestoreError> {
    transaction.execute_batch(
        "CREATE TABLE source_checkpoint(
           account_id TEXT PRIMARY KEY,
           source_fingerprint TEXT NOT NULL UNIQUE,
           committed_at_unix_nanoseconds TEXT NOT NULL,
           conversation_count INTEGER NOT NULL,
           participant_count INTEGER NOT NULL,
           message_count INTEGER NOT NULL,
           artifact_count INTEGER NOT NULL,
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE sync_run(
           run_id TEXT PRIMARY KEY,
           account_id TEXT NOT NULL,
           mode TEXT NOT NULL,
           source_fingerprint TEXT NOT NULL,
           started_at_unix_nanoseconds TEXT NOT NULL,
           committed_at_unix_nanoseconds TEXT NOT NULL,
           changed_record_count INTEGER NOT NULL,
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE change_log(
           sequence INTEGER PRIMARY KEY AUTOINCREMENT,
           account_id TEXT NOT NULL,
           source_fingerprint TEXT NOT NULL,
           change_kind TEXT NOT NULL,
           entity_kind TEXT NOT NULL,
           entity_id TEXT NOT NULL,
           conversation_id TEXT,
           record_sha256 TEXT,
           observed_at_unix_nanoseconds TEXT NOT NULL,
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         );
         CREATE INDEX message_by_conversation_time
           ON message(account_id, conversation_id, created_at_unix, conversation_ordinal);
         CREATE INDEX message_by_sender
           ON message(account_id, sender_id, created_at_unix);
         CREATE INDEX message_by_type
           ON message(account_id, logical_type, sub_type, created_at_unix);
         CREATE INDEX relationship_by_target
           ON message_relationship(account_id, target_canonical_id);
         CREATE INDEX change_by_account_sequence
           ON change_log(account_id, sequence);
         CREATE VIRTUAL TABLE message_fts USING fts5(
           account_id UNINDEXED,
           canonical_id UNINDEXED,
           conversation_id UNINDEXED,
           search_text,
           tokenize = 'unicode61'
         );
         UPDATE replica_schema SET schema_version = 2 WHERE singleton = 1;",
    )?;
    record_migration(transaction, 2, "checkpoints change stream and exact FTS")?;
    Ok(())
}

fn migration_3(transaction: &Transaction<'_>) -> Result<(), RestoreError> {
    transaction.execute_batch(
        "CREATE TABLE replica_generation(
           singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
           replica_id TEXT NOT NULL UNIQUE
         );
         INSERT INTO replica_generation VALUES (1, lower(hex(randomblob(16))));
         CREATE TABLE sync_seen(
           entity_kind TEXT NOT NULL,
           entity_id TEXT NOT NULL,
           PRIMARY KEY(entity_kind, entity_id)
         ) WITHOUT ROWID;
         UPDATE replica_schema SET schema_version = 3 WHERE singleton = 1;",
    )?;
    record_migration(
        transaction,
        3,
        "encrypted reconciliation staging and resumable change stream",
    )?;
    Ok(())
}

fn migration_4(transaction: &Transaction<'_>) -> Result<(), RestoreError> {
    transaction.execute_batch(
        "CREATE TABLE cached_moment(
           account_id TEXT NOT NULL,
           canonical_id TEXT NOT NULL,
           author_id TEXT,
           created_at_unix INTEGER,
           content_type INTEGER,
           record_sha256 TEXT NOT NULL,
           record_json BLOB NOT NULL,
           PRIMARY KEY(account_id, canonical_id),
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE cached_moment_interaction(
           account_id TEXT NOT NULL,
           canonical_id TEXT NOT NULL,
           created_at_unix INTEGER,
           interaction_kind TEXT NOT NULL,
           from_participant_id TEXT,
           to_participant_id TEXT,
           record_sha256 TEXT NOT NULL,
           record_json BLOB NOT NULL,
           PRIMARY KEY(account_id, canonical_id),
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE TABLE cached_surface_state(
           account_id TEXT PRIMARY KEY,
           observed_at TEXT NOT NULL,
           coverage_json BLOB NOT NULL,
           FOREIGN KEY(account_id) REFERENCES replica_identity(account_id) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE INDEX cached_moment_by_time
           ON cached_moment(account_id, created_at_unix, canonical_id);
         CREATE INDEX cached_moment_by_author_time
           ON cached_moment(account_id, author_id, created_at_unix, canonical_id);
         CREATE INDEX cached_moment_by_type_time
           ON cached_moment(account_id, content_type, created_at_unix, canonical_id);
         CREATE INDEX cached_interaction_by_time
           ON cached_moment_interaction(account_id, created_at_unix, canonical_id);
         UPDATE replica_schema SET schema_version = 4 WHERE singleton = 1;",
    )?;
    record_migration(
        transaction,
        4,
        "passive cached moments interactions and coverage",
    )?;
    Ok(())
}

fn record_migration(
    transaction: &Transaction<'_>,
    version: u32,
    identity: &str,
) -> Result<(), RestoreError> {
    transaction.execute(
        "INSERT INTO migration_history VALUES (?1, ?2, ?3)",
        params![
            version,
            unix_nanoseconds()?.to_string(),
            hex::encode(Sha256::digest(identity.as_bytes()))
        ],
    )?;
    Ok(())
}

fn create_pre_migration_backup(
    source: &Connection,
    replica_path: &Path,
    key: &ReplicaKey,
    version: u32,
) -> Result<String, RestoreError> {
    let parent = replica_path
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath("replica has no parent".to_string()))?;
    ensure_private_directory(parent)?;
    let base = replica_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("replica.db");
    let file_name = format!(
        ".{base}.pre-migration-v{version}-{}.db",
        unix_nanoseconds()?
    );
    let path = parent.join(&file_name);
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)?;
    let result = (|| {
        let mut destination = open_keyed_connection(&path, key)?;
        let backup = Backup::new(source, &mut destination)?;
        backup.run_to_completion(128, Duration::from_millis(2), None)?;
        drop(backup);
        destination.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        secure_replica_files(&path)?;
        Ok(())
    })();
    if result.is_err() {
        remove_failed_replica_files(&path);
    }
    result.map(|()| file_name)
}

fn import_archive_transactionally(
    connection: &mut Connection,
    archive_directory: &Path,
    report: &RestorationReport,
) -> Result<ImportCounts, RestoreError> {
    let conversations_path = archive_directory.join("conversations.ndjson");
    let participants_path = archive_directory.join("participants.ndjson");
    let messages_path = archive_directory.join("messages.ndjson");
    let artifacts_path = archive_directory.join("artifacts.ndjson");
    let coverage_path = archive_directory.join("coverage.json");
    for path in [
        &conversations_path,
        &participants_path,
        &messages_path,
        &artifacts_path,
        &coverage_path,
    ] {
        ensure_private_regular_file(path)?;
    }
    let coverage = load_archive_coverage(archive_directory)?;
    let cached = load_cached_archive_inputs(archive_directory)?;
    let started = unix_nanoseconds()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO replica_identity(
           singleton, account_id, current_source_fingerprint, restoration_complete,
           created_at_unix_nanoseconds, updated_at_unix_nanoseconds
         ) VALUES (1, ?1, NULL, NULL, ?2, ?2)",
        params![report.account_id, started.to_string()],
    )?;
    let mut counts = ImportCounts::default();

    for_each_ndjson::<CanonicalConversation>(&conversations_path, |conversation, bytes| {
        require_account(&conversation.account_id, &report.account_id)?;
        transaction.execute(
            "INSERT INTO conversation VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                report.account_id,
                conversation.conversation_id,
                json_enum(&conversation.kind)?,
                json_enum(&conversation.entity_decode_state)?,
                checked_usize_i64(conversation.participant_ids.len())?,
                sha256(&bytes),
                bytes,
            ],
        )?;
        counts.conversations += 1;
        Ok(())
    })?;
    for_each_ndjson::<CanonicalParticipant>(&participants_path, |participant, bytes| {
        require_account(&participant.account_id, &report.account_id)?;
        transaction.execute(
            "INSERT INTO participant VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                report.account_id,
                participant.participant_id,
                json_enum(&participant.local_profile_state)?,
                sha256(&bytes),
                bytes,
            ],
        )?;
        counts.participants += 1;
        Ok(())
    })?;
    for_each_ndjson::<CanonicalConversation>(&conversations_path, |conversation, _| {
        for membership in conversation.memberships {
            transaction.execute(
                "INSERT INTO conversation_participant VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    report.account_id,
                    conversation.conversation_id,
                    membership.participant_id,
                    json_enum(&membership.role)?,
                    membership.display_name_base64,
                ],
            )?;
        }
        Ok(())
    })?;
    for_each_ndjson::<CanonicalArtifact>(&artifacts_path, |artifact, bytes| {
        transaction.execute(
            "INSERT INTO artifact VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                report.account_id,
                artifact.artifact_id,
                json_enum(&artifact.kind)?,
                json_enum(&artifact.role)?,
                json_enum(&artifact.availability)?,
                artifact.source_sha256,
                artifact.decoded_sha256,
                sha256(&bytes),
                bytes,
            ],
        )?;
        counts.artifacts += 1;
        Ok(())
    })?;
    for_each_ndjson::<CanonicalMessage>(&messages_path, |message, bytes| {
        require_account(&message.account_id, &report.account_id)?;
        let search_text = message_search_text(&message);
        let record_sha = sha256(&bytes);
        transaction.execute(
            "INSERT INTO message VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13
             )",
            params![
                report.account_id,
                message.canonical_id,
                message.conversation_id,
                message.sender_id,
                checked_i64(message.conversation_ordinal)?,
                message.created_at_unix,
                json_enum(&message.direction)?,
                message.logical_type,
                message.sub_type,
                json_enum(&message.semantic_decode_state)?,
                search_text,
                record_sha,
                bytes,
            ],
        )?;
        transaction.execute(
            "INSERT INTO message_fts(account_id, canonical_id, conversation_id, search_text)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                report.account_id,
                message.canonical_id,
                message.conversation_id,
                search_text,
            ],
        )?;
        for (ordinal, relationship) in message.relationships.into_iter().enumerate() {
            transaction.execute(
                "INSERT INTO message_relationship VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    report.account_id,
                    message.canonical_id,
                    checked_usize_i64(ordinal)?,
                    json_enum(&relationship.kind)?,
                    relationship.target_canonical_id,
                    relationship.resolved,
                    serde_json::to_vec(&relationship)?,
                ],
            )?;
            counts.relationships += 1;
        }
        for (ordinal, reference) in message.artifact_references.into_iter().enumerate() {
            transaction.execute(
                "INSERT INTO message_artifact VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    report.account_id,
                    message.canonical_id,
                    checked_usize_i64(ordinal)?,
                    reference.artifact_id,
                    json_enum(&reference.role)?,
                    reference.preferred,
                ],
            )?;
            counts.message_artifacts += 1;
        }
        counts.messages += 1;
        Ok(())
    })?;

    if let Some(cached) = cached.as_ref() {
        for_each_ndjson::<crate::CanonicalCachedMoment>(&cached.moments_path, |moment, bytes| {
            require_account(&moment.account_id, &report.account_id)?;
            transaction.execute(
                "INSERT INTO cached_moment VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    report.account_id,
                    moment.canonical_id,
                    moment.author_id,
                    moment.created_at_unix,
                    moment.content_type,
                    sha256(&bytes),
                    bytes,
                ],
            )?;
            counts.cached_moments += 1;
            Ok(())
        })?;
        for_each_ndjson::<crate::CanonicalCachedMomentInteraction>(
            &cached.interactions_path,
            |interaction, bytes| {
                require_account(&interaction.account_id, &report.account_id)?;
                transaction.execute(
                    "INSERT INTO cached_moment_interaction VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        report.account_id,
                        interaction.canonical_id,
                        interaction.created_at_unix,
                        json_enum(&interaction.kind)?,
                        interaction.from_participant_id,
                        interaction.to_participant_id,
                        sha256(&bytes),
                        bytes,
                    ],
                )?;
                counts.cached_moment_interactions += 1;
                Ok(())
            },
        )?;
        transaction.execute(
            "INSERT INTO cached_surface_state VALUES (?1, ?2, ?3)",
            params![
                report.account_id,
                cached.coverage.observed_at,
                serde_json::to_vec(&cached.coverage)?,
            ],
        )?;
    }

    let committed = unix_nanoseconds()?;
    transaction.execute(
        "INSERT INTO coverage_state VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            report.account_id,
            report.source_fingerprint,
            serde_json::to_vec(&coverage)?,
            serde_json::to_vec(report)?,
            report.completion.full_restoration_achieved,
        ],
    )?;
    transaction.execute(
        "INSERT INTO source_checkpoint VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            report.account_id,
            report.source_fingerprint,
            committed.to_string(),
            checked_i64(counts.conversations)?,
            checked_i64(counts.participants)?,
            checked_i64(counts.messages)?,
            checked_i64(counts.artifacts)?,
        ],
    )?;
    let run_id = sha256(
        format!(
            "{}:{}:{started}",
            report.account_id, report.source_fingerprint
        )
        .as_bytes(),
    );
    transaction.execute(
        "INSERT INTO sync_run VALUES (?1, ?2, 'bootstrap', ?3, ?4, ?5, ?6)",
        params![
            run_id,
            report.account_id,
            report.source_fingerprint,
            started.to_string(),
            committed.to_string(),
            checked_i64(
                counts.conversations
                    + counts.participants
                    + counts.messages
                    + counts.artifacts
                    + counts.cached_moments
                    + counts.cached_moment_interactions
            )?,
        ],
    )?;
    transaction.execute(
        "INSERT INTO change_log(
           account_id, source_fingerprint, change_kind, entity_kind, entity_id,
           conversation_id, record_sha256, observed_at_unix_nanoseconds
         ) VALUES (?1, ?2, 'bootstrap', 'checkpoint', ?2, NULL, NULL, ?3)",
        params![
            report.account_id,
            report.source_fingerprint,
            committed.to_string()
        ],
    )?;
    transaction.execute(
        "UPDATE replica_identity SET
           current_source_fingerprint = ?2,
           restoration_complete = ?3,
           updated_at_unix_nanoseconds = ?4
         WHERE account_id = ?1",
        params![
            report.account_id,
            report.source_fingerprint,
            report.completion.full_restoration_achieved,
            committed.to_string(),
        ],
    )?;
    transaction.commit()?;
    Ok(counts)
}

fn require_authoritative_archive(report: &RestorationReport) -> Result<(), RestoreError> {
    if report.archive_scope != crate::RestorationArchiveScope::Authoritative {
        return Err(RestoreError::Integrity(
            "incremental acquisition must be merged with its prior authoritative state before replica mutation"
                .to_string(),
        ));
    }
    Ok(())
}

fn reconcile_archive_transactionally(
    connection: &mut Connection,
    archive_directory: &Path,
    report: &RestorationReport,
    previous_fingerprint: &str,
) -> Result<(SyncCounts, u128), RestoreError> {
    let conversations_path = archive_directory.join("conversations.ndjson");
    let participants_path = archive_directory.join("participants.ndjson");
    let messages_path = archive_directory.join("messages.ndjson");
    let artifacts_path = archive_directory.join("artifacts.ndjson");
    let coverage_path = archive_directory.join("coverage.json");
    for path in [
        &conversations_path,
        &participants_path,
        &messages_path,
        &artifacts_path,
        &coverage_path,
    ] {
        ensure_private_regular_file(path)?;
    }
    let coverage = load_archive_coverage(archive_directory)?;
    let cached = load_cached_archive_inputs(archive_directory)?;
    let started = unix_nanoseconds()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute("DELETE FROM sync_seen", [])?;
    let mut counts = SyncCounts::default();
    let mut changed_conversations = HashSet::new();

    for_each_ndjson::<CanonicalConversation>(&conversations_path, |conversation, bytes| {
        require_account(&conversation.account_id, &report.account_id)?;
        mark_seen(&transaction, "conversation", &conversation.conversation_id)?;
        let digest = sha256(&bytes);
        let existing: Option<String> = transaction
            .query_row(
                "SELECT record_sha256 FROM conversation
                 WHERE account_id = ?1 AND conversation_id = ?2",
                params![report.account_id, conversation.conversation_id],
                |row| row.get(0),
            )
            .optional()?;
        let change_kind = match existing.as_deref() {
            None => {
                transaction.execute(
                    "INSERT INTO conversation VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        report.account_id,
                        conversation.conversation_id,
                        json_enum(&conversation.kind)?,
                        json_enum(&conversation.entity_decode_state)?,
                        checked_usize_i64(conversation.participant_ids.len())?,
                        digest,
                        bytes,
                    ],
                )?;
                Some("added")
            }
            Some(value) if value != digest => {
                transaction.execute(
                    "UPDATE conversation SET
                       kind = ?3, entity_decode_state = ?4, participant_count = ?5,
                       record_sha256 = ?6, record_json = ?7
                     WHERE account_id = ?1 AND conversation_id = ?2",
                    params![
                        report.account_id,
                        conversation.conversation_id,
                        json_enum(&conversation.kind)?,
                        json_enum(&conversation.entity_decode_state)?,
                        checked_usize_i64(conversation.participant_ids.len())?,
                        digest,
                        bytes,
                    ],
                )?;
                Some("changed")
            }
            Some(_) => None,
        };
        if let Some(kind) = change_kind {
            changed_conversations.insert(conversation.conversation_id.clone());
            record_change(
                &transaction,
                &mut counts,
                &report.account_id,
                &report.source_fingerprint,
                kind,
                "conversation",
                &conversation.conversation_id,
                Some(&conversation.conversation_id),
                Some(&digest),
                started,
            )?;
        }
        Ok(())
    })?;

    for_each_ndjson::<CanonicalParticipant>(&participants_path, |participant, bytes| {
        require_account(&participant.account_id, &report.account_id)?;
        mark_seen(&transaction, "participant", &participant.participant_id)?;
        let digest = sha256(&bytes);
        let existing: Option<String> = transaction
            .query_row(
                "SELECT record_sha256 FROM participant
                 WHERE account_id = ?1 AND participant_id = ?2",
                params![report.account_id, participant.participant_id],
                |row| row.get(0),
            )
            .optional()?;
        let change_kind = match existing.as_deref() {
            None => {
                transaction.execute(
                    "INSERT INTO participant VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        report.account_id,
                        participant.participant_id,
                        json_enum(&participant.local_profile_state)?,
                        digest,
                        bytes,
                    ],
                )?;
                Some("added")
            }
            Some(value) if value != digest => {
                transaction.execute(
                    "UPDATE participant SET local_profile_state = ?3,
                       record_sha256 = ?4, record_json = ?5
                     WHERE account_id = ?1 AND participant_id = ?2",
                    params![
                        report.account_id,
                        participant.participant_id,
                        json_enum(&participant.local_profile_state)?,
                        digest,
                        bytes,
                    ],
                )?;
                Some("changed")
            }
            Some(_) => None,
        };
        if let Some(kind) = change_kind {
            record_change(
                &transaction,
                &mut counts,
                &report.account_id,
                &report.source_fingerprint,
                kind,
                "participant",
                &participant.participant_id,
                None,
                Some(&digest),
                started,
            )?;
        }
        Ok(())
    })?;

    for_each_ndjson::<CanonicalConversation>(&conversations_path, |conversation, _| {
        if !changed_conversations.contains(&conversation.conversation_id) {
            return Ok(());
        }
        transaction.execute(
            "DELETE FROM conversation_participant
             WHERE account_id = ?1 AND conversation_id = ?2",
            params![report.account_id, conversation.conversation_id],
        )?;
        for membership in conversation.memberships {
            transaction.execute(
                "INSERT INTO conversation_participant VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    report.account_id,
                    conversation.conversation_id,
                    membership.participant_id,
                    json_enum(&membership.role)?,
                    membership.display_name_base64,
                ],
            )?;
        }
        Ok(())
    })?;

    for_each_ndjson::<CanonicalArtifact>(&artifacts_path, |artifact, bytes| {
        mark_seen(&transaction, "artifact", &artifact.artifact_id)?;
        let digest = sha256(&bytes);
        let existing: Option<String> = transaction
            .query_row(
                "SELECT record_sha256 FROM artifact
                 WHERE account_id = ?1 AND artifact_id = ?2",
                params![report.account_id, artifact.artifact_id],
                |row| row.get(0),
            )
            .optional()?;
        let change_kind = match existing.as_deref() {
            None => {
                transaction.execute(
                    "INSERT INTO artifact VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        report.account_id,
                        artifact.artifact_id,
                        json_enum(&artifact.kind)?,
                        json_enum(&artifact.role)?,
                        json_enum(&artifact.availability)?,
                        artifact.source_sha256,
                        artifact.decoded_sha256,
                        digest,
                        bytes,
                    ],
                )?;
                Some("added")
            }
            Some(value) if value != digest => {
                transaction.execute(
                    "UPDATE artifact SET kind = ?3, role = ?4, availability = ?5,
                       source_sha256 = ?6, decoded_sha256 = ?7,
                       record_sha256 = ?8, record_json = ?9
                     WHERE account_id = ?1 AND artifact_id = ?2",
                    params![
                        report.account_id,
                        artifact.artifact_id,
                        json_enum(&artifact.kind)?,
                        json_enum(&artifact.role)?,
                        json_enum(&artifact.availability)?,
                        artifact.source_sha256,
                        artifact.decoded_sha256,
                        digest,
                        bytes,
                    ],
                )?;
                Some("changed")
            }
            Some(_) => None,
        };
        if let Some(kind) = change_kind {
            record_change(
                &transaction,
                &mut counts,
                &report.account_id,
                &report.source_fingerprint,
                kind,
                "artifact",
                &artifact.artifact_id,
                None,
                Some(&digest),
                started,
            )?;
        }
        Ok(())
    })?;

    for_each_ndjson::<CanonicalMessage>(&messages_path, |message, bytes| {
        require_account(&message.account_id, &report.account_id)?;
        mark_seen(&transaction, "message", &message.canonical_id)?;
        let digest = sha256(&bytes);
        let existing: Option<String> = transaction
            .query_row(
                "SELECT record_sha256 FROM message
                 WHERE account_id = ?1 AND canonical_id = ?2",
                params![report.account_id, message.canonical_id],
                |row| row.get(0),
            )
            .optional()?;
        let change_kind = match existing.as_deref() {
            None => {
                insert_message(&transaction, report, &message, &bytes, &digest)?;
                Some("added")
            }
            Some(value) if value != digest => {
                update_message(&transaction, report, &message, &bytes, &digest)?;
                Some("changed")
            }
            Some(_) => None,
        };
        if let Some(kind) = change_kind {
            replace_message_links(&transaction, report, &message)?;
            record_change(
                &transaction,
                &mut counts,
                &report.account_id,
                &report.source_fingerprint,
                kind,
                "message",
                &message.canonical_id,
                Some(&message.conversation_id),
                Some(&digest),
                started,
            )?;
        }
        Ok(())
    })?;

    reconcile_cached_surfaces(&transaction, report, cached.as_ref(), &mut counts, started)?;

    remove_missing_messages(&transaction, report, &mut counts, started)?;
    remove_missing_entities(
        &transaction,
        report,
        &mut counts,
        started,
        "artifact",
        "artifact_id",
    )?;
    remove_missing_entities(
        &transaction,
        report,
        &mut counts,
        started,
        "conversation",
        "conversation_id",
    )?;
    remove_missing_entities(
        &transaction,
        report,
        &mut counts,
        started,
        "participant",
        "participant_id",
    )?;
    remove_missing_cached(
        &transaction,
        report,
        &mut counts,
        started,
        "cached_moment",
        "cachedMoment",
    )?;
    remove_missing_cached(
        &transaction,
        report,
        &mut counts,
        started,
        "cached_moment_interaction",
        "cachedMomentInteraction",
    )?;
    match cached.as_ref() {
        Some(cached) => {
            transaction.execute(
                "INSERT INTO cached_surface_state VALUES (?1, ?2, ?3)
                 ON CONFLICT(account_id) DO UPDATE SET
                   observed_at = excluded.observed_at,
                   coverage_json = excluded.coverage_json",
                params![
                    report.account_id,
                    cached.coverage.observed_at,
                    serde_json::to_vec(&cached.coverage)?,
                ],
            )?;
        }
        None => {
            transaction.execute(
                "DELETE FROM cached_surface_state WHERE account_id = ?1",
                [&report.account_id],
            )?;
        }
    }

    let committed = next_checkpoint_revision(&transaction, &report.account_id)?;
    transaction.execute(
        "INSERT INTO coverage_state VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(account_id) DO UPDATE SET
           source_fingerprint = excluded.source_fingerprint,
           coverage_json = excluded.coverage_json,
           report_json = excluded.report_json,
           full_restoration_achieved = excluded.full_restoration_achieved",
        params![
            report.account_id,
            report.source_fingerprint,
            serde_json::to_vec(&coverage)?,
            serde_json::to_vec(report)?,
            report.completion.full_restoration_achieved,
        ],
    )?;
    let conversation_count = table_account_count(&transaction, "conversation", &report.account_id)?;
    let participant_count = table_account_count(&transaction, "participant", &report.account_id)?;
    let message_count = table_account_count(&transaction, "message", &report.account_id)?;
    let artifact_count = table_account_count(&transaction, "artifact", &report.account_id)?;
    transaction.execute(
        "INSERT INTO source_checkpoint VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(account_id) DO UPDATE SET
           source_fingerprint = excluded.source_fingerprint,
           committed_at_unix_nanoseconds = excluded.committed_at_unix_nanoseconds,
           conversation_count = excluded.conversation_count,
           participant_count = excluded.participant_count,
           message_count = excluded.message_count,
           artifact_count = excluded.artifact_count",
        params![
            report.account_id,
            report.source_fingerprint,
            committed.to_string(),
            checked_i64(conversation_count)?,
            checked_i64(participant_count)?,
            checked_i64(message_count)?,
            checked_i64(artifact_count)?,
        ],
    )?;
    let run_id = sha256(
        format!(
            "{}:{}:{}:{started}",
            report.account_id, previous_fingerprint, report.source_fingerprint
        )
        .as_bytes(),
    );
    let sync_kind = synchronization_kind(report);
    transaction.execute(
        "INSERT INTO sync_run VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            run_id,
            report.account_id,
            sync_kind,
            report.source_fingerprint,
            started.to_string(),
            committed.to_string(),
            checked_i64(counts.added + counts.changed + counts.removed)?,
        ],
    )?;
    transaction.execute(
        "UPDATE replica_identity SET
           current_source_fingerprint = ?2,
           restoration_complete = ?3,
           updated_at_unix_nanoseconds = ?4
         WHERE account_id = ?1",
        params![
            report.account_id,
            report.source_fingerprint,
            report.completion.full_restoration_achieved,
            committed.to_string(),
        ],
    )?;
    transaction.execute("DELETE FROM sync_seen", [])?;
    transaction.commit()?;
    Ok((counts, committed))
}

fn insert_message(
    transaction: &Transaction<'_>,
    report: &RestorationReport,
    message: &CanonicalMessage,
    bytes: &[u8],
    digest: &str,
) -> Result<(), RestoreError> {
    let search_text = message_search_text(message);
    transaction.execute(
        "INSERT INTO message VALUES (
           ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13
         )",
        params![
            report.account_id,
            message.canonical_id,
            message.conversation_id,
            message.sender_id,
            checked_i64(message.conversation_ordinal)?,
            message.created_at_unix,
            json_enum(&message.direction)?,
            message.logical_type,
            message.sub_type,
            json_enum(&message.semantic_decode_state)?,
            search_text,
            digest,
            bytes,
        ],
    )?;
    transaction.execute(
        "INSERT INTO message_fts(account_id, canonical_id, conversation_id, search_text)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            report.account_id,
            message.canonical_id,
            message.conversation_id,
            search_text,
        ],
    )?;
    Ok(())
}

fn update_message(
    transaction: &Transaction<'_>,
    report: &RestorationReport,
    message: &CanonicalMessage,
    bytes: &[u8],
    digest: &str,
) -> Result<(), RestoreError> {
    let search_text = message_search_text(message);
    transaction.execute(
        "UPDATE message SET
           conversation_id = ?3, sender_id = ?4, conversation_ordinal = ?5,
           created_at_unix = ?6, direction = ?7, logical_type = ?8, sub_type = ?9,
           semantic_decode_state = ?10, search_text = ?11,
           record_sha256 = ?12, record_json = ?13
         WHERE account_id = ?1 AND canonical_id = ?2",
        params![
            report.account_id,
            message.canonical_id,
            message.conversation_id,
            message.sender_id,
            checked_i64(message.conversation_ordinal)?,
            message.created_at_unix,
            json_enum(&message.direction)?,
            message.logical_type,
            message.sub_type,
            json_enum(&message.semantic_decode_state)?,
            search_text,
            digest,
            bytes,
        ],
    )?;
    transaction.execute(
        "DELETE FROM message_fts WHERE account_id = ?1 AND canonical_id = ?2",
        params![report.account_id, message.canonical_id],
    )?;
    transaction.execute(
        "INSERT INTO message_fts(account_id, canonical_id, conversation_id, search_text)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            report.account_id,
            message.canonical_id,
            message.conversation_id,
            search_text,
        ],
    )?;
    Ok(())
}

fn replace_message_links(
    transaction: &Transaction<'_>,
    report: &RestorationReport,
    message: &CanonicalMessage,
) -> Result<(), RestoreError> {
    transaction.execute(
        "DELETE FROM message_relationship WHERE account_id = ?1 AND source_canonical_id = ?2",
        params![report.account_id, message.canonical_id],
    )?;
    transaction.execute(
        "DELETE FROM message_artifact WHERE account_id = ?1 AND canonical_id = ?2",
        params![report.account_id, message.canonical_id],
    )?;
    for (ordinal, relationship) in message.relationships.iter().enumerate() {
        transaction.execute(
            "INSERT INTO message_relationship VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                report.account_id,
                message.canonical_id,
                checked_usize_i64(ordinal)?,
                json_enum(&relationship.kind)?,
                relationship.target_canonical_id,
                relationship.resolved,
                serde_json::to_vec(relationship)?,
            ],
        )?;
    }
    for (ordinal, reference) in message.artifact_references.iter().enumerate() {
        transaction.execute(
            "INSERT INTO message_artifact VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                report.account_id,
                message.canonical_id,
                checked_usize_i64(ordinal)?,
                reference.artifact_id,
                json_enum(&reference.role)?,
                reference.preferred,
            ],
        )?;
    }
    Ok(())
}

fn reconcile_cached_surfaces(
    transaction: &Transaction<'_>,
    report: &RestorationReport,
    cached: Option<&CachedArchiveInputs>,
    counts: &mut SyncCounts,
    observed_at: u128,
) -> Result<(), RestoreError> {
    let Some(cached) = cached else {
        return Ok(());
    };
    for_each_ndjson::<crate::CanonicalCachedMoment>(&cached.moments_path, |moment, bytes| {
        require_account(&moment.account_id, &report.account_id)?;
        mark_seen(transaction, "cachedMoment", &moment.canonical_id)?;
        let digest = sha256(&bytes);
        let existing: Option<String> = transaction
            .query_row(
                "SELECT record_sha256 FROM cached_moment
                 WHERE account_id = ?1 AND canonical_id = ?2",
                params![report.account_id, moment.canonical_id],
                |row| row.get(0),
            )
            .optional()?;
        let kind = match existing.as_deref() {
            None => {
                transaction.execute(
                    "INSERT INTO cached_moment VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        report.account_id,
                        moment.canonical_id,
                        moment.author_id,
                        moment.created_at_unix,
                        moment.content_type,
                        digest,
                        bytes,
                    ],
                )?;
                Some("added")
            }
            Some(value) if value != digest => {
                transaction.execute(
                    "UPDATE cached_moment SET author_id = ?3, created_at_unix = ?4,
                       content_type = ?5, record_sha256 = ?6, record_json = ?7
                     WHERE account_id = ?1 AND canonical_id = ?2",
                    params![
                        report.account_id,
                        moment.canonical_id,
                        moment.author_id,
                        moment.created_at_unix,
                        moment.content_type,
                        digest,
                        bytes,
                    ],
                )?;
                Some("changed")
            }
            Some(_) => None,
        };
        if let Some(kind) = kind {
            record_change(
                transaction,
                counts,
                &report.account_id,
                &report.source_fingerprint,
                kind,
                "cachedMoment",
                &moment.canonical_id,
                None,
                Some(&digest),
                observed_at,
            )?;
        }
        Ok(())
    })?;
    for_each_ndjson::<crate::CanonicalCachedMomentInteraction>(
        &cached.interactions_path,
        |interaction, bytes| {
            require_account(&interaction.account_id, &report.account_id)?;
            mark_seen(
                transaction,
                "cachedMomentInteraction",
                &interaction.canonical_id,
            )?;
            let digest = sha256(&bytes);
            let existing: Option<String> = transaction
                .query_row(
                    "SELECT record_sha256 FROM cached_moment_interaction
                     WHERE account_id = ?1 AND canonical_id = ?2",
                    params![report.account_id, interaction.canonical_id],
                    |row| row.get(0),
                )
                .optional()?;
            let kind = match existing.as_deref() {
                None => {
                    transaction.execute(
                        "INSERT INTO cached_moment_interaction VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            report.account_id,
                            interaction.canonical_id,
                            interaction.created_at_unix,
                            json_enum(&interaction.kind)?,
                            interaction.from_participant_id,
                            interaction.to_participant_id,
                            digest,
                            bytes,
                        ],
                    )?;
                    Some("added")
                }
                Some(value) if value != digest => {
                    transaction.execute(
                        "UPDATE cached_moment_interaction SET
                           created_at_unix = ?3, interaction_kind = ?4,
                           from_participant_id = ?5, to_participant_id = ?6,
                           record_sha256 = ?7, record_json = ?8
                         WHERE account_id = ?1 AND canonical_id = ?2",
                        params![
                            report.account_id,
                            interaction.canonical_id,
                            interaction.created_at_unix,
                            json_enum(&interaction.kind)?,
                            interaction.from_participant_id,
                            interaction.to_participant_id,
                            digest,
                            bytes,
                        ],
                    )?;
                    Some("changed")
                }
                Some(_) => None,
            };
            if let Some(kind) = kind {
                record_change(
                    transaction,
                    counts,
                    &report.account_id,
                    &report.source_fingerprint,
                    kind,
                    "cachedMomentInteraction",
                    &interaction.canonical_id,
                    None,
                    Some(&digest),
                    observed_at,
                )?;
            }
            Ok(())
        },
    )?;
    Ok(())
}

fn remove_missing_cached(
    transaction: &Transaction<'_>,
    report: &RestorationReport,
    counts: &mut SyncCounts,
    observed_at: u128,
    table: &str,
    entity_kind: &str,
) -> Result<(), RestoreError> {
    debug_assert!(matches!(
        table,
        "cached_moment" | "cached_moment_interaction"
    ));
    debug_assert!(matches!(
        entity_kind,
        "cachedMoment" | "cachedMomentInteraction"
    ));
    let query = format!(
        "SELECT canonical_id, record_sha256 FROM {table}
         WHERE account_id = ?1 AND NOT EXISTS(
           SELECT 1 FROM sync_seen
           WHERE entity_kind = ?2 AND entity_id = canonical_id
         ) ORDER BY canonical_id"
    );
    let values = {
        let mut statement = transaction.prepare(&query)?;
        let rows = statement.query_map(params![report.account_id, entity_kind], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for (identifier, digest) in values {
        record_change(
            transaction,
            counts,
            &report.account_id,
            &report.source_fingerprint,
            "removed",
            entity_kind,
            &identifier,
            None,
            Some(&digest),
            observed_at,
        )?;
        let delete = format!("DELETE FROM {table} WHERE account_id = ?1 AND canonical_id = ?2");
        transaction.execute(&delete, params![report.account_id, identifier])?;
    }
    Ok(())
}

fn mark_seen(
    transaction: &Transaction<'_>,
    entity_kind: &str,
    entity_id: &str,
) -> Result<(), RestoreError> {
    transaction.execute(
        "INSERT INTO sync_seen VALUES (?1, ?2)",
        params![entity_kind, entity_id],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn record_change(
    transaction: &Transaction<'_>,
    counts: &mut SyncCounts,
    account_id: &str,
    source_fingerprint: &str,
    change_kind: &str,
    entity_kind: &str,
    entity_id: &str,
    conversation_id: Option<&str>,
    record_sha256: Option<&str>,
    observed_at: u128,
) -> Result<(), RestoreError> {
    match change_kind {
        "added" => counts.added += 1,
        "changed" => counts.changed += 1,
        "removed" => counts.removed += 1,
        _ => {
            return Err(RestoreError::Integrity(
                "unsupported replica change kind".to_string(),
            ))
        }
    }
    transaction.execute(
        "INSERT INTO change_log(
           account_id, source_fingerprint, change_kind, entity_kind, entity_id,
           conversation_id, record_sha256, observed_at_unix_nanoseconds
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            account_id,
            source_fingerprint,
            change_kind,
            entity_kind,
            entity_id,
            conversation_id,
            record_sha256,
            observed_at.to_string(),
        ],
    )?;
    Ok(())
}

fn remove_missing_messages(
    transaction: &Transaction<'_>,
    report: &RestorationReport,
    counts: &mut SyncCounts,
    observed_at: u128,
) -> Result<(), RestoreError> {
    let values = {
        let mut statement = transaction.prepare(
            "SELECT canonical_id, conversation_id, record_sha256 FROM message
             WHERE account_id = ?1 AND NOT EXISTS(
               SELECT 1 FROM sync_seen
               WHERE entity_kind = 'message' AND entity_id = canonical_id
             ) ORDER BY canonical_id",
        )?;
        let rows = statement.query_map([&report.account_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for (identifier, conversation, digest) in values {
        record_change(
            transaction,
            counts,
            &report.account_id,
            &report.source_fingerprint,
            "removed",
            "message",
            &identifier,
            Some(&conversation),
            Some(&digest),
            observed_at,
        )?;
        transaction.execute(
            "DELETE FROM message_fts WHERE account_id = ?1 AND canonical_id = ?2",
            params![report.account_id, identifier],
        )?;
        transaction.execute(
            "DELETE FROM message WHERE account_id = ?1 AND canonical_id = ?2",
            params![report.account_id, identifier],
        )?;
    }
    Ok(())
}

fn remove_missing_entities(
    transaction: &Transaction<'_>,
    report: &RestorationReport,
    counts: &mut SyncCounts,
    observed_at: u128,
    table: &str,
    id_column: &str,
) -> Result<(), RestoreError> {
    debug_assert!(matches!(table, "artifact" | "conversation" | "participant"));
    debug_assert!(matches!(
        id_column,
        "artifact_id" | "conversation_id" | "participant_id"
    ));
    let query = format!(
        "SELECT {id_column}, record_sha256 FROM {table}
         WHERE account_id = ?1 AND NOT EXISTS(
           SELECT 1 FROM sync_seen
           WHERE entity_kind = ?2 AND entity_id = {id_column}
         ) ORDER BY {id_column}"
    );
    let values = {
        let mut statement = transaction.prepare(&query)?;
        let rows = statement.query_map(params![report.account_id, table], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for (identifier, digest) in values {
        let conversation = (table == "conversation").then_some(identifier.as_str());
        record_change(
            transaction,
            counts,
            &report.account_id,
            &report.source_fingerprint,
            "removed",
            table,
            &identifier,
            conversation,
            Some(&digest),
            observed_at,
        )?;
        let delete = format!("DELETE FROM {table} WHERE account_id = ?1 AND {id_column} = ?2");
        transaction.execute(&delete, params![report.account_id, identifier])?;
    }
    Ok(())
}

fn table_account_count(
    transaction: &Transaction<'_>,
    table: &str,
    account_id: &str,
) -> Result<u64, RestoreError> {
    debug_assert!(matches!(
        table,
        "conversation" | "participant" | "message" | "artifact"
    ));
    let sql = format!("SELECT count(*) FROM {table} WHERE account_id = ?1");
    let count: i64 = transaction.query_row(&sql, [account_id], |row| row.get(0))?;
    Ok(count.max(0) as u64)
}

fn sync_report(
    connection: &Connection,
    account_id: &str,
    previous_source_fingerprint: &str,
    current_source_fingerprint: &str,
    idempotent: bool,
    counts: SyncCounts,
    committed_at_unix_nanoseconds: Option<u128>,
) -> Result<ReplicaSyncReport, RestoreError> {
    Ok(ReplicaSyncReport {
        format_version: REPLICA_FORMAT_VERSION,
        account_id: account_id.to_string(),
        previous_source_fingerprint: previous_source_fingerprint.to_string(),
        current_source_fingerprint: current_source_fingerprint.to_string(),
        idempotent,
        added_count: counts.added,
        changed_count: counts.changed,
        removed_count: counts.removed,
        conversation_count: table_count(connection, "conversation")?,
        participant_count: table_count(connection, "participant")?,
        message_count: table_count(connection, "message")?,
        artifact_count: table_count(connection, "artifact")?,
        cached_moment_count: table_count(connection, "cached_moment")?,
        cached_moment_interaction_count: table_count(connection, "cached_moment_interaction")?,
        committed_at_unix_nanoseconds,
    })
}

fn encode_change_cursor(cursor: &ReplicaChangeCursor) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(cursor).expect("change cursor serialization cannot fail"))
}

fn decode_change_cursor(value: &str) -> Result<ReplicaChangeCursor, RestoreError> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| RestoreError::Integrity("change cursor is not valid base64url".to_string()))?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn validate_message_filter(filter: &ReplicaMessageFilter) -> Result<(), RestoreError> {
    if filter
        .not_before_unix
        .zip(filter.not_after_unix)
        .is_some_and(|(start, end)| start > end)
    {
        return Err(RestoreError::Integrity(
            "message query has an inverted time range".to_string(),
        ));
    }
    if filter
        .full_text_query
        .as_ref()
        .is_some_and(|query| query.is_empty() || query.len() > 4_096)
    {
        return Err(RestoreError::Integrity(
            "full-text query must be between 1 and 4096 bytes".to_string(),
        ));
    }
    for (name, value) in [
        ("conversation", filter.conversation_id.as_deref()),
        ("sender", filter.sender_id.as_deref()),
        ("reply target", filter.reply_target_canonical_id.as_deref()),
    ] {
        if value.is_some_and(|value| value.is_empty() || value.len() > 512) {
            return Err(RestoreError::Integrity(format!(
                "{name} filter must be between 1 and 512 bytes"
            )));
        }
    }
    Ok(())
}

fn validate_cached_moment_filter(filter: &ReplicaCachedMomentFilter) -> Result<(), RestoreError> {
    if filter
        .not_before_unix
        .zip(filter.not_after_unix)
        .is_some_and(|(start, end)| start > end)
    {
        return Err(RestoreError::Integrity(
            "cached-moment query has an inverted time range".to_string(),
        ));
    }
    if filter
        .author_id
        .as_ref()
        .is_some_and(|identifier| identifier.is_empty() || identifier.len() > 512)
    {
        return Err(RestoreError::Integrity(
            "cached-moment author filter must be between 1 and 512 bytes".to_string(),
        ));
    }
    Ok(())
}

fn current_replica_identity(connection: &Connection) -> Result<(String, String), RestoreError> {
    connection
        .query_row(
            "SELECT account_id, current_source_fingerprint
             FROM replica_identity WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .map_err(RestoreError::from)
        .and_then(|(account, fingerprint)| {
            fingerprint
                .map(|fingerprint| (account, fingerprint))
                .ok_or_else(|| {
                    RestoreError::Integrity(
                        "replica has no authoritative source checkpoint".to_string(),
                    )
                })
        })
}

fn current_replica_checkpoint(
    connection: &Connection,
) -> Result<(String, String, String), RestoreError> {
    connection
        .query_row(
            "SELECT account_id, current_source_fingerprint, updated_at_unix_nanoseconds
             FROM replica_identity WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(RestoreError::from)
        .and_then(|(account, fingerprint, revision)| {
            let fingerprint = fingerprint.ok_or_else(|| {
                RestoreError::Integrity(
                    "replica has no authoritative source checkpoint".to_string(),
                )
            })?;
            revision.parse::<u128>().map_err(|_| {
                RestoreError::Integrity("replica checkpoint revision is invalid".to_string())
            })?;
            Ok((account, fingerprint, revision))
        })
}

fn encode_message_cursor(cursor: &ReplicaMessageCursor) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(cursor).expect("message cursor serialization cannot fail"))
}

fn decode_message_cursor(value: &str) -> Result<ReplicaMessageCursor, RestoreError> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| {
            RestoreError::Integrity("message cursor is not valid base64url".to_string())
        })?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn encode_cached_moment_cursor(cursor: &ReplicaCachedMomentCursor) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(cursor).expect("cached-moment cursor serialization cannot fail"))
}

fn decode_cached_moment_cursor(value: &str) -> Result<ReplicaCachedMomentCursor, RestoreError> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| {
            RestoreError::Integrity("cached-moment cursor is not valid base64url".to_string())
        })?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn bootstrap_report(
    opened: &OpenedReplica,
    report: &RestorationReport,
    idempotent: bool,
) -> Result<ReplicaBootstrapReport, RestoreError> {
    Ok(ReplicaBootstrapReport {
        format_version: REPLICA_FORMAT_VERSION,
        schema_version: CURRENT_SCHEMA_VERSION,
        account_id: report.account_id.clone(),
        source_fingerprint: report.source_fingerprint.clone(),
        cipher_version: opened.cipher_version.clone(),
        encrypted_at_rest: true,
        idempotent,
        conversation_count: table_count(&opened.connection, "conversation")?,
        participant_count: table_count(&opened.connection, "participant")?,
        message_count: table_count(&opened.connection, "message")?,
        artifact_count: table_count(&opened.connection, "artifact")?,
        cached_moment_count: table_count(&opened.connection, "cached_moment")?,
        cached_moment_interaction_count: table_count(
            &opened.connection,
            "cached_moment_interaction",
        )?,
        relationship_count: table_count(&opened.connection, "message_relationship")?,
        message_artifact_count: table_count(&opened.connection, "message_artifact")?,
        pre_migration_backup_file_name: opened.pre_migration_backup_file_name.clone(),
    })
}

fn for_each_ndjson<T: DeserializeOwned + Serialize>(
    path: &Path,
    mut body: impl FnMut(T, Vec<u8>) -> Result<(), RestoreError>,
) -> Result<(), RestoreError> {
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let value: T = serde_json::from_str(&line)?;
        let canonical = serde_json::to_vec(&value)?;
        body(value, canonical)?;
    }
    Ok(())
}

fn message_search_text(message: &CanonicalMessage) -> String {
    let mut values = Vec::new();
    if let TypedPayload::Decoded(value) = &message.typed_payload {
        collect_search_strings(value, None, &mut values);
    }
    if let Some(content) = &message.content_base64 {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(content) {
            if let Ok(value) = String::from_utf8(bytes) {
                if !values.iter().any(|existing| existing == &value) {
                    values.push(value);
                }
            }
        }
    }
    values.join("\n")
}

fn collect_search_strings(
    value: &serde_json::Value,
    field: Option<&str>,
    output: &mut Vec<String>,
) {
    if matches!(field, Some("raw_xml" | "raw")) {
        return;
    }
    match value {
        serde_json::Value::String(value) if !value.is_empty() => output.push(value.clone()),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_search_strings(value, field, output);
            }
        }
        serde_json::Value::Object(values) => {
            for (name, value) in values {
                collect_search_strings(value, Some(name), output);
            }
        }
        _ => {}
    }
}

fn require_account(actual: &str, expected: &str) -> Result<(), RestoreError> {
    if actual != expected {
        return Err(RestoreError::Integrity(
            "archive record crossed the account isolation boundary".to_string(),
        ));
    }
    Ok(())
}

fn json_enum(value: &impl Serialize) -> Result<String, RestoreError> {
    let encoded = serde_json::to_string(value)?;
    Ok(encoded.trim_matches('"').to_string())
}

fn table_count(connection: &Connection, table: &str) -> Result<u64, RestoreError> {
    let sql = format!("SELECT count(*) FROM {table}");
    let count: i64 = connection.query_row(&sql, [], |row| row.get(0))?;
    Ok(count.max(0) as u64)
}

fn replica_id(connection: &Connection) -> Result<String, RestoreError> {
    let value: String = connection.query_row(
        "SELECT replica_id FROM replica_generation WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(RestoreError::Integrity(
            "replica generation identity is invalid".to_string(),
        ));
    }
    Ok(value)
}

fn sha256(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn checked_i64(value: u64) -> Result<i64, RestoreError> {
    i64::try_from(value)
        .map_err(|_| RestoreError::Integrity("replica count exceeds SQLite range".to_string()))
}

fn checked_usize_i64(value: usize) -> Result<i64, RestoreError> {
    i64::try_from(value)
        .map_err(|_| RestoreError::Integrity("replica count exceeds SQLite range".to_string()))
}

fn unix_nanoseconds() -> Result<u128, RestoreError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .map_err(|_| RestoreError::Integrity("system clock predates Unix epoch".to_string()))
}

fn age_seconds(timestamp: u128) -> Result<u64, RestoreError> {
    let age = unix_nanoseconds()?.saturating_sub(timestamp) / 1_000_000_000;
    u64::try_from(age)
        .map_err(|_| RestoreError::Integrity("replica checkpoint age exceeds range".to_string()))
}

fn next_checkpoint_revision(
    transaction: &Transaction<'_>,
    account_id: &str,
) -> Result<u128, RestoreError> {
    let previous: String = transaction.query_row(
        "SELECT updated_at_unix_nanoseconds FROM replica_identity WHERE account_id = ?1",
        [account_id],
        |row| row.get(0),
    )?;
    let previous = previous.parse::<u128>().map_err(|_| {
        RestoreError::Integrity("replica checkpoint revision is invalid".to_string())
    })?;
    Ok(unix_nanoseconds()?.max(previous.saturating_add(1)))
}

fn load_archive_coverage(archive_directory: &Path) -> Result<RestorationCoverage, RestoreError> {
    let path = archive_directory.join("coverage.json");
    ensure_private_regular_file(&path)?;
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn load_cached_archive_inputs(
    archive_directory: &Path,
) -> Result<Option<CachedArchiveInputs>, RestoreError> {
    let moments_path = archive_directory.join("cached-moments.ndjson");
    let interactions_path = archive_directory.join("cached-moment-interactions.ndjson");
    let coverage_path = archive_directory.join("cached-surfaces.json");
    let exists = [
        moments_path.try_exists()?,
        interactions_path.try_exists()?,
        coverage_path.try_exists()?,
    ];
    if exists.iter().all(|value| !value) {
        return Ok(None);
    }
    if !exists.iter().all(|value| *value) {
        return Err(RestoreError::Integrity(
            "cached-surface archive files are incomplete".to_string(),
        ));
    }
    for path in [&moments_path, &interactions_path, &coverage_path] {
        ensure_private_regular_file(path)?;
    }
    let coverage = serde_json::from_slice(&fs::read(coverage_path)?)?;
    Ok(Some(CachedArchiveInputs {
        moments_path,
        interactions_path,
        coverage,
    }))
}

fn load_coverage_state(
    connection: &Connection,
    account_id: &str,
) -> Result<Option<(RestorationReport, RestorationCoverage)>, RestoreError> {
    let state: Option<(Vec<u8>, Vec<u8>)> = connection
        .query_row(
            "SELECT report_json, coverage_json FROM coverage_state WHERE account_id = ?1",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    state
        .map(|(report, coverage)| {
            Ok((
                serde_json::from_slice(&report)?,
                serde_json::from_slice(&coverage)?,
            ))
        })
        .transpose()
}

fn archive_revision_digest(report: &RestorationReport, coverage: &RestorationCoverage) -> String {
    sha256(
        &serde_json::to_vec(&(
            &report.source_fingerprint,
            &report.client_build_compatibility,
            &report.integrity,
            &report.completion,
            report.archive_scope,
            report.media_phase,
            coverage,
        ))
        .expect("restoration revision serialization cannot fail"),
    )
}

fn synchronization_kind(report: &RestorationReport) -> &'static str {
    match report.acquisition.as_ref().map(|evidence| evidence.mode) {
        Some(crate::SnapshotAcquisitionMode::Incremental) => "incrementalMerge",
        Some(crate::SnapshotAcquisitionMode::IntegrityScan) => "integrityScan",
        Some(crate::SnapshotAcquisitionMode::Bootstrap) => "fullScan",
        None => "reconcile",
    }
}

fn load_sync_health(
    connection: &Connection,
    account_id: Option<&String>,
) -> Result<SyncHealth, RestoreError> {
    let Some(account_id) = account_id else {
        return Ok(SyncHealth::default());
    };
    let latest: Option<(String, String, String)> = connection
        .query_row(
            "SELECT mode, started_at_unix_nanoseconds, committed_at_unix_nanoseconds
             FROM sync_run WHERE account_id = ?1
             ORDER BY length(committed_at_unix_nanoseconds) DESC,
                      committed_at_unix_nanoseconds DESC LIMIT 1",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let last_integrity_scan: Option<String> = connection
        .query_row(
            "SELECT committed_at_unix_nanoseconds FROM sync_run
             WHERE account_id = ?1 AND mode = 'integrityScan'
             ORDER BY length(committed_at_unix_nanoseconds) DESC,
                      committed_at_unix_nanoseconds DESC LIMIT 1",
            [account_id],
            |row| row.get(0),
        )
        .optional()?;
    let (last_kind, last_started, last_duration_milliseconds) = match latest {
        Some((kind, started, committed)) => {
            let started = started.parse::<u128>().map_err(|_| {
                RestoreError::Integrity("sync start timestamp is invalid".to_string())
            })?;
            let committed = committed.parse::<u128>().map_err(|_| {
                RestoreError::Integrity("sync commit timestamp is invalid".to_string())
            })?;
            let duration = committed.saturating_sub(started) / 1_000_000;
            let duration = u64::try_from(duration).map_err(|_| {
                RestoreError::Integrity("sync duration exceeds supported range".to_string())
            })?;
            (Some(kind), Some(started), Some(duration))
        }
        None => (None, None, None),
    };
    let last_integrity_scan = last_integrity_scan
        .map(|timestamp| {
            timestamp.parse::<u128>().map_err(|_| {
                RestoreError::Integrity("integrity scan timestamp is invalid".to_string())
            })
        })
        .transpose()?;
    Ok(SyncHealth {
        last_kind,
        last_started,
        last_duration_milliseconds,
        last_integrity_scan,
    })
}

fn checkpoint_and_secure(connection: &Connection, path: &Path) -> Result<(), RestoreError> {
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    secure_replica_files(path)
}

fn secure_replica_files(path: &Path) -> Result<(), RestoreError> {
    for candidate in replica_file_set(path) {
        if candidate.try_exists()? {
            let metadata = fs::symlink_metadata(&candidate)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.nlink() != 1 {
                return Err(RestoreError::Integrity(
                    "replica storage contains an unsafe file identity".to_string(),
                ));
            }
            fs::set_permissions(candidate, fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

fn remove_failed_replica_files(path: &Path) {
    for candidate in replica_file_set(path) {
        let _ = fs::remove_file(candidate);
    }
}

fn replica_file_set(path: &Path) -> [PathBuf; 3] {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut wal = bytes.to_vec();
    wal.extend_from_slice(b"-wal");
    let mut shm = bytes.to_vec();
    shm.extend_from_slice(b"-shm");
    [
        path.to_path_buf(),
        PathBuf::from(std::ffi::OsString::from_vec(wal)),
        PathBuf::from(std::ffi::OsString::from_vec(shm)),
    ]
}

trait OptionalRow<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalRow<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error),
        }
    }
}
