use std::collections::{BTreeSet, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use rusqlite::backup::Backup;
use rusqlite::{named_params, params, Connection, OpenFlags, Transaction, TransactionBehavior};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::archive::{ensure_private_directory, ensure_private_regular_file, load_report};
use crate::audit::{
    audit_archive_with_progress, validate_canonical_artifact, verify_recorded_artifact_files,
};
use crate::restore::available_free_bytes;
use crate::schema::{validate_cached_coverage_schema, validate_restoration_coverage_schema};
use crate::{
    CanonicalArtifact, CanonicalConversation, CanonicalMessage, CanonicalParticipant, NoProgress,
    ProgressEvent, ProgressObserver, ProgressPhase, ProgressState, ProgressUnit, ReplicaKey,
    RestorationCoverage, RestorationReport, RestoreError, TypedPayload,
};

const CURRENT_SCHEMA_VERSION: u32 = 5;
const REPLICA_FORMAT_VERSION: u32 = 1;
const MIGRATION_1_IDENTITY: &str = "canonical replica base schema";
const MIGRATION_2_IDENTITY: &str = "checkpoints change stream and exact FTS";
const MIGRATION_3_IDENTITY: &str = "encrypted reconciliation staging and resumable change stream";
const MIGRATION_4_IDENTITY: &str = "passive cached moments interactions and coverage";
const MIGRATION_5_IDENTITY: &str = "account holder identity binding";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaBootstrapReport {
    pub format_version: u32,
    pub schema_version: u32,
    pub account_id: String,
    pub self_participant_id: Option<String>,
    pub source_fingerprint: String,
    pub cipher_version: String,
    pub encrypted_at_rest: bool,
    pub idempotent: bool,
    pub archive_scope: crate::RestorationArchiveScope,
    pub authoritative_database_coverage: bool,
    pub total_database_count: usize,
    pub restored_database_count: usize,
    pub unavailable_database_count: usize,
    pub preserved_stale_database_count: usize,
    pub conversation_count: u64,
    pub participant_count: u64,
    pub message_count: u64,
    pub artifact_count: u64,
    pub cached_moment_count: u64,
    pub cached_moment_interaction_count: u64,
    #[serde(default)]
    pub cached_surface_omitted_row_count: u64,
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
    pub self_participant_id: Option<String>,
    pub current_source_fingerprint: Option<String>,
    pub checkpoint_revision: Option<String>,
    pub client_build_compatibility: Option<crate::ClientBuildCompatibilityEvidence>,
    pub acquisition_mode: Option<crate::SnapshotAcquisitionMode>,
    pub media_phase: Option<crate::RestorationMediaPhase>,
    pub archive_scope: Option<crate::RestorationArchiveScope>,
    pub authoritative_database_coverage: Option<bool>,
    pub total_database_count: Option<usize>,
    pub restored_database_count: Option<usize>,
    pub unavailable_database_count: Option<usize>,
    pub preserved_stale_database_count: Option<usize>,
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
    pub cached_surface_omitted_row_count: Option<u64>,
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
    /// Serving-time surfaces that could not be inspected. Identity, key, and
    /// checkpoint failures still fail closed; damaged optional/domain tables
    /// are isolated so healthy replica data remains available.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReplicaAuditReport {
    pub format_version: u32,
    pub privacy_safe_summary: bool,
    pub initialized: bool,
    pub schema_version: u32,
    pub encrypted_at_rest: bool,
    pub sqlite_integrity_verified: bool,
    pub foreign_keys_verified: bool,
    pub migration_ledger_verified: bool,
    pub identity_checkpoint_verified: bool,
    pub record_digests_verified: bool,
    pub indexed_projections_verified: bool,
    pub message_links_verified: bool,
    pub full_text_index_verified: bool,
    pub coverage_state_verified: bool,
    pub change_stream_verified: bool,
    pub transient_state_empty: bool,
    pub conversation_count: u64,
    pub participant_count: u64,
    pub message_count: u64,
    pub artifact_count: u64,
    pub cached_moment_count: u64,
    pub cached_moment_interaction_count: u64,
    pub relationship_count: u64,
    pub message_artifact_count: u64,
    pub change_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReplicaBackupAuditReport {
    pub format_version: u32,
    pub privacy_safe_summary: bool,
    pub schema_version: u32,
    pub replica_format_version: u32,
    pub initialized: bool,
    pub encrypted_at_rest: bool,
    pub sqlite_integrity_verified: bool,
    pub foreign_keys_verified: bool,
    pub migration_ledger_verified: bool,
    pub record_digests_verified: bool,
    pub indexed_projections_verified: bool,
    pub message_links_verified: bool,
    pub coverage_state_verified: bool,
    pub checkpoint_state_verified: bool,
    pub full_text_index_verified: bool,
    pub change_stream_verified: bool,
    pub transient_state_empty: bool,
    pub conversation_count: u64,
    pub participant_count: u64,
    pub message_count: u64,
    pub artifact_count: u64,
    pub relationship_count: u64,
    pub message_artifact_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReplicaRecoveryPreparationReport {
    pub format_version: u32,
    pub privacy_safe_summary: bool,
    pub source_backup_verified: bool,
    pub source_schema_version: u32,
    pub current_schema_version: u32,
    pub candidate_audit_verified: bool,
    pub initialized: bool,
    pub encrypted_at_rest: bool,
    pub conversation_count: u64,
    pub participant_count: u64,
    pub message_count: u64,
    pub artifact_count: u64,
    pub relationship_count: u64,
    pub message_artifact_count: u64,
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
    pub self_participant_id: Option<String>,
    pub previous_source_fingerprint: String,
    pub current_source_fingerprint: String,
    pub idempotent: bool,
    pub archive_scope: crate::RestorationArchiveScope,
    pub authoritative_database_coverage: bool,
    pub total_database_count: usize,
    pub restored_database_count: usize,
    pub unavailable_database_count: usize,
    pub preserved_stale_database_count: usize,
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
    #[serde(default)]
    pub omitted_item_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
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
    /// Rows that matched the query but could not be decoded or whose stored
    /// identity disagreed with their indexed identity. These rows are never
    /// released, but they also do not make healthy rows unavailable.
    #[serde(default)]
    pub omitted_item_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaConversationPage {
    pub account_id: String,
    pub items: Vec<CanonicalConversation>,
    #[serde(default)]
    pub omitted_item_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplicaCoverageView {
    pub account_id: String,
    pub self_participant_id: Option<String>,
    pub source_fingerprint: String,
    pub archive_scope: crate::RestorationArchiveScope,
    pub database_coverage: Option<crate::RestorationDatabaseCoverage>,
    pub coverage: RestorationCoverage,
    pub integrity: crate::RestorationIntegrity,
    pub completion: crate::RestorationCompletion,
    pub cached_surfaces: Option<crate::CachedSurfaceCoverage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
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
    #[serde(default)]
    pub omitted_item_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
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

#[derive(Clone, Copy)]
struct ReplicaApplicationPlan {
    archive_byte_count: u64,
    estimated_peak_byte_count: u64,
    required_free_byte_count: u64,
    available_free_byte_count_at_start: u64,
    total_work_record_count: u64,
    source_record_count: u64,
    file_count: usize,
    database_coverage: DatabaseCoverageSummary,
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

#[derive(Clone, Copy)]
struct DatabaseCoverageSummary {
    authoritative: bool,
    total: usize,
    restored: usize,
    unavailable: usize,
    preserved_stale: usize,
}

fn database_coverage_summary(report: &RestorationReport) -> DatabaseCoverageSummary {
    report.database_coverage.as_ref().map_or(
        DatabaseCoverageSummary {
            authoritative: report.archive_scope == crate::RestorationArchiveScope::Authoritative,
            total: report.integrity.database_count as usize,
            restored: report.integrity.database_count as usize,
            unavailable: 0,
            preserved_stale: 0,
        },
        |coverage| DatabaseCoverageSummary {
            authoritative: coverage.authoritative_database_coverage,
            total: coverage.total_database_count,
            restored: coverage.restored_database_count,
            unavailable: coverage.unavailable_database_count,
            preserved_stale: coverage.preserved_stale_database_count,
        },
    )
}

pub fn bootstrap_replica(
    archive_directory: &Path,
    replica_path: &Path,
    key: &ReplicaKey,
) -> Result<ReplicaBootstrapReport, RestoreError> {
    bootstrap_replica_with_progress(archive_directory, replica_path, key, &NoProgress)
}

pub fn bootstrap_replica_with_progress(
    archive_directory: &Path,
    replica_path: &Path,
    key: &ReplicaKey,
    progress: &dyn ProgressObserver,
) -> Result<ReplicaBootstrapReport, RestoreError> {
    ensure_private_directory(archive_directory)?;
    let report = load_report(archive_directory)?;
    require_serving_archive(&report)?;
    if report.format_version >= 3 {
        audit_archive_with_progress(archive_directory, progress)?;
    }
    bootstrap_audited_replica_with_progress(archive_directory, replica_path, key, &report, progress)
}

/// Applies an archive that the caller has already audited while holding the
/// publication identity stable. This avoids reading multi-gigabyte ledgers a
/// second time in the follower path; public entry points always audit first.
pub(crate) fn bootstrap_audited_replica_with_progress(
    archive_directory: &Path,
    replica_path: &Path,
    key: &ReplicaKey,
    report: &RestorationReport,
    progress: &dyn ProgressObserver,
) -> Result<ReplicaBootstrapReport, RestoreError> {
    ensure_private_directory(archive_directory)?;
    require_serving_archive(report)?;
    let replica_namespace_was_absent = replica_namespace_is_absent(replica_path)?;
    let result = (|| {
        let mut opened = open_replica(replica_path, key)?;
        let existing_identity: Option<(String, Option<String>)> = opened
            .connection
            .query_row(
                "SELECT account_id, self_participant_id FROM replica_identity WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if existing_identity
            .as_ref()
            .is_some_and(|(account, _)| account != &report.account_id)
        {
            return Err(RestoreError::Integrity(
                "replica belongs to a different account".to_string(),
            ));
        }
        if let Some((_, existing_self)) = existing_identity.as_ref() {
            require_compatible_self_participant(
                existing_self.as_deref(),
                report.self_participant_id.as_deref(),
            )?;
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
            if existing_identity.as_ref().map(|identity| &identity.1)
                != Some(&report.self_participant_id)
            {
                return Err(RestoreError::Integrity(
                    "an existing checkpoint cannot acquire a new account-holder binding through idempotent bootstrap; use synchronization"
                        .to_string(),
                ));
            }
            emit_idempotent_replica_progress(report, archive_directory, replica_path, progress)?;
            return bootstrap_report(&opened, report, true);
        }
        if existing_checkpoint.is_some() {
            return Err(RestoreError::Integrity(
                "replica is already bootstrapped from another checkpoint; use synchronization"
                    .to_string(),
            ));
        }

        let plan =
            preflight_replica_application(archive_directory, replica_path, report, progress)?;
        let counts = import_archive_transactionally(
            &mut opened.connection,
            archive_directory,
            report,
            &plan,
            replica_path,
            progress,
        )?;
        emit_replica_checkpoint_progress(&plan, replica_path, ProgressState::Started, progress)?;
        checkpoint_and_secure(&opened.connection, replica_path)?;
        emit_replica_checkpoint_progress(&plan, replica_path, ProgressState::Completed, progress)?;
        let database_coverage = database_coverage_summary(report);
        Ok(ReplicaBootstrapReport {
            format_version: REPLICA_FORMAT_VERSION,
            schema_version: CURRENT_SCHEMA_VERSION,
            account_id: report.account_id.clone(),
            self_participant_id: report.self_participant_id.clone(),
            source_fingerprint: report.source_fingerprint.clone(),
            cipher_version: opened.cipher_version,
            encrypted_at_rest: true,
            idempotent: false,
            archive_scope: report.archive_scope,
            authoritative_database_coverage: database_coverage.authoritative,
            total_database_count: database_coverage.total,
            restored_database_count: database_coverage.restored,
            unavailable_database_count: database_coverage.unavailable,
            preserved_stale_database_count: database_coverage.preserved_stale,
            conversation_count: counts.conversations,
            participant_count: counts.participants,
            message_count: counts.messages,
            artifact_count: counts.artifacts,
            cached_moment_count: counts.cached_moments,
            cached_moment_interaction_count: counts.cached_moment_interactions,
            cached_surface_omitted_row_count: report.integrity.cached_surface_omitted_row_count,
            relationship_count: counts.relationships,
            message_artifact_count: counts.message_artifacts,
            pre_migration_backup_file_name: opened.pre_migration_backup_file_name,
        })
    })();
    if result.is_err() && replica_namespace_was_absent {
        remove_failed_replica_files(replica_path);
    }
    result
}

pub fn replica_status(
    replica_path: &Path,
    key: &ReplicaKey,
) -> Result<ReplicaStatus, RestoreError> {
    let opened = open_replica(replica_path, key)?;
    let mut limitation_codes = BTreeSet::new();
    let identity = opened
        .connection
        .query_row(
            "SELECT account_id, current_source_fingerprint, restoration_complete,
                    updated_at_unix_nanoseconds, self_participant_id
             FROM replica_identity WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<bool>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?;
    if let Some((account_id, source_fingerprint, _, checkpoint_revision, _)) = identity.as_ref() {
        let revision_is_valid = checkpoint_revision
            .parse::<u128>()
            .is_ok_and(|revision| revision > 0);
        if account_id.is_empty()
            || source_fingerprint.as_deref().is_none_or(str::is_empty)
            || !revision_is_valid
        {
            return Err(RestoreError::Integrity(
                "replica identity or checkpoint revision is invalid".to_string(),
            ));
        }
    }
    let checkpoint = if let Some((account, _, _, _, _)) = identity.as_ref() {
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
        Some((account, _, _, _, _)) => load_coverage_state(&opened.connection, account)?,
        None => None,
    };
    let stored_report = stored_state.as_ref().map(|value| &value.0);
    let stored_coverage = stored_state.as_ref().map(|value| &value.1);
    if let Some(report) = stored_report {
        let Some((account_id, source_fingerprint, _, _, self_participant_id)) = identity.as_ref()
        else {
            return Err(RestoreError::Integrity(
                "uninitialized replica contains restoration coverage".to_string(),
            ));
        };
        if report.account_id != *account_id
            || source_fingerprint.as_deref() != Some(report.source_fingerprint.as_str())
        {
            return Err(RestoreError::Integrity(
                "replica identity disagrees with its restoration coverage".to_string(),
            ));
        }
        if report.self_participant_id != *self_participant_id {
            return Err(RestoreError::Integrity(
                "replica account-holder binding disagrees with its coverage state".to_string(),
            ));
        }
        if report.integrity.cached_surface_omitted_row_count > 0 {
            limitation_codes.insert("cachedSurfaceSourceRowsOmitted".to_string());
        }
    }
    let checkpoint_age_seconds = checkpoint.and_then(|timestamp| {
        let value = unix_nanoseconds()
            .ok()
            .map(|now| now.saturating_sub(timestamp) / 1_000_000_000)
            .and_then(|seconds| u64::try_from(seconds).ok());
        if value.is_none() {
            limitation_codes.insert("replicaCheckpointAgeUnavailable".to_string());
        }
        value
    });
    let semantic_decode_coverage_ratio = stored_report.and_then(|report| {
        (report.integrity.restored_row_count > 0).then(|| {
            let covered = report
                .integrity
                .restored_row_count
                .saturating_sub(report.integrity.semantic_gap_count);
            covered as f64 / report.integrity.restored_row_count as f64
        })
    });
    let health = match (identity.as_ref(), stored_report) {
        (None, _) => ReplicaHealthState::Uninitialized,
        (Some(_), None) => ReplicaHealthState::CurrentWithCoverageGaps,
        (_, Some(report)) if report.completion.full_restoration_achieved => {
            ReplicaHealthState::CurrentComplete
        }
        (_, Some(_)) => ReplicaHealthState::CurrentWithCoverageGaps,
    };
    let database_coverage = stored_report.map(database_coverage_summary);
    let sync_health =
        match load_sync_health(&opened.connection, identity.as_ref().map(|value| &value.0)) {
            Ok(value) => value,
            Err(_) => {
                limitation_codes.insert("replicaSynchronizationHistoryUnavailable".to_string());
                SyncHealth::default()
            }
        };
    let integrity_scan_age_seconds = sync_health.last_integrity_scan.and_then(|timestamp| {
        let value = age_seconds(timestamp).ok();
        if value.is_none() {
            limitation_codes.insert("replicaIntegrityScanAgeUnavailable".to_string());
        }
        value
    });
    let conversation_count = best_effort_table_count(
        &opened.connection,
        "conversation",
        "replicaConversationTableUnavailable",
        &mut limitation_codes,
    );
    let participant_count = best_effort_table_count(
        &opened.connection,
        "participant",
        "replicaParticipantTableUnavailable",
        &mut limitation_codes,
    );
    let message_count = best_effort_table_count(
        &opened.connection,
        "message",
        "replicaMessageTableUnavailable",
        &mut limitation_codes,
    );
    let artifact_count = best_effort_table_count(
        &opened.connection,
        "artifact",
        "replicaArtifactTableUnavailable",
        &mut limitation_codes,
    );
    let cached_moment_count = best_effort_table_count(
        &opened.connection,
        "cached_moment",
        "replicaCachedMomentTableUnavailable",
        &mut limitation_codes,
    );
    let cached_moment_interaction_count = best_effort_table_count(
        &opened.connection,
        "cached_moment_interaction",
        "replicaCachedMomentInteractionTableUnavailable",
        &mut limitation_codes,
    );
    Ok(ReplicaStatus {
        format_version: REPLICA_FORMAT_VERSION,
        schema_version: CURRENT_SCHEMA_VERSION,
        replica_id: replica_id(&opened.connection)?,
        account_id: identity.as_ref().map(|value| value.0.clone()),
        self_participant_id: identity.as_ref().and_then(|value| value.4.clone()),
        current_source_fingerprint: identity.as_ref().and_then(|value| value.1.clone()),
        checkpoint_revision: identity.as_ref().map(|value| value.3.clone()),
        client_build_compatibility: stored_report
            .map(|report| report.client_build_compatibility.clone()),
        acquisition_mode: stored_report
            .and_then(|report| report.acquisition.as_ref().map(|value| value.mode)),
        media_phase: stored_report.map(|report| report.media_phase),
        archive_scope: stored_report.map(|report| report.archive_scope),
        authoritative_database_coverage: database_coverage.map(|coverage| coverage.authoritative),
        total_database_count: database_coverage.map(|coverage| coverage.total),
        restored_database_count: database_coverage.map(|coverage| coverage.restored),
        unavailable_database_count: database_coverage.map(|coverage| coverage.unavailable),
        preserved_stale_database_count: database_coverage.map(|coverage| coverage.preserved_stale),
        decoder_name: stored_coverage.map(|coverage| coverage.decoder_name.clone()),
        decoder_version: stored_coverage.map(|coverage| coverage.decoder_version.clone()),
        cipher_version: opened.cipher_version,
        encrypted_at_rest: true,
        conversation_count,
        participant_count,
        message_count,
        artifact_count,
        cached_moment_count,
        cached_moment_interaction_count,
        cached_surface_omitted_row_count: stored_report
            .map(|report| report.integrity.cached_surface_omitted_row_count),
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
        limitation_codes: limitation_codes.into_iter().collect(),
    })
}

pub fn audit_replica(
    replica_path: &Path,
    key: &ReplicaKey,
) -> Result<ReplicaAuditReport, RestoreError> {
    audit_replica_with_progress(replica_path, key, &NoProgress)
}

pub fn audit_replica_with_progress(
    replica_path: &Path,
    key: &ReplicaKey,
    observer: &dyn ProgressObserver,
) -> Result<ReplicaAuditReport, RestoreError> {
    verify_private_replica_files(replica_path)?;
    let replica_byte_count = replica_namespace_byte_count(replica_path)?;
    let mut progress = ReplicaAuditProgress::new(observer, replica_byte_count);
    progress.emit(
        0,
        ProgressState::Planned,
        "planReplicaAudit",
        ProgressUnit::Bytes,
        0,
        replica_byte_count,
        ReplicaAuditCounter::None,
        None,
    );
    let (mut connection, _) = open_existing_replica_read_only(replica_path, key)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let connection = &*transaction;
    let version = schema_version(connection)?;
    if version != CURRENT_SCHEMA_VERSION {
        return Err(RestoreError::Integrity(format!(
            "replica audit requires current schema version {CURRENT_SCHEMA_VERSION}"
        )));
    }
    validate_replica_migration_ledger(connection, version)?;
    let _ = replica_id(connection)?;
    let conversation_count = table_count(connection, "conversation")?;
    let participant_count = table_count(connection, "participant")?;
    let message_count = table_count(connection, "message")?;
    let artifact_count = table_count(connection, "artifact")?;
    let cached_moment_count = table_count(connection, "cached_moment")?;
    let cached_moment_interaction_count = table_count(connection, "cached_moment_interaction")?;
    let relationship_count = table_count(connection, "message_relationship")?;
    let message_artifact_count = table_count(connection, "message_artifact")?;
    let change_count = table_count(connection, "change_log")?;
    let synchronization_run_count = table_count(connection, "sync_run")?;
    let membership_count = table_count(connection, "conversation_participant")?;
    let transient_count = table_count(connection, "sync_seen")?;
    if transient_count != 0 {
        return Err(RestoreError::Integrity(
            "replica reconciliation staging is not empty".to_string(),
        ));
    }

    let identity_count = table_count(connection, "replica_identity")?;
    if identity_count > 1 {
        return Err(RestoreError::Integrity(
            "replica contains multiple account identities".to_string(),
        ));
    }
    let initialized = identity_count == 1;
    progress.set_totals(ReplicaAuditTotals {
        conversation_count,
        message_count,
        canonical_record_count: conversation_count
            .saturating_add(participant_count)
            .saturating_add(message_count)
            .saturating_add(artifact_count)
            .saturating_add(cached_moment_count)
            .saturating_add(cached_moment_interaction_count),
        link_record_count: membership_count
            .saturating_add(relationship_count)
            .saturating_add(message_artifact_count),
        change_record_count: change_count.saturating_add(synchronization_run_count),
    });
    progress.emit(
        0,
        ProgressState::Completed,
        "openReplicaAuditSnapshot",
        ProgressUnit::Bytes,
        replica_byte_count,
        replica_byte_count,
        ReplicaAuditCounter::None,
        None,
    );
    with_replica_audit_heartbeat(
        &progress,
        1,
        "verifyReplicaSQLiteIntegrity",
        ProgressUnit::Bytes,
        replica_byte_count,
        || {
            verify_sqlite_integrity(connection)?;
            verify_foreign_keys(connection)
        },
    )?;
    if initialized {
        let identity: (String, String, bool, String, String) = connection.query_row(
            "SELECT account_id, current_source_fingerprint, restoration_complete,
                    created_at_unix_nanoseconds, updated_at_unix_nanoseconds
             FROM replica_identity WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        verify_positive_timestamp(&identity.3, "replica creation")?;
        verify_positive_timestamp(&identity.4, "replica update")?;
        if identity.0.is_empty() || identity.1.is_empty() {
            return Err(RestoreError::Integrity(
                "replica identity is incomplete".to_string(),
            ));
        }
        let record_audit = audit_replica_records(connection, &identity.0, true, &progress)?;
        if record_audit.conversation_count != conversation_count
            || record_audit.participant_count != participant_count
            || record_audit.message_count != message_count
            || record_audit.artifact_count != artifact_count
            || record_audit.cached_moment_count != cached_moment_count
            || record_audit.cached_moment_interaction_count != cached_moment_interaction_count
        {
            return Err(RestoreError::Integrity(
                "replica record audit counts changed during verification".to_string(),
            ));
        }
        verify_replica_message_links(connection, &identity.0, &record_audit, &progress)?;
        progress.emit(
            4,
            ProgressState::Started,
            "verifyReplicaCheckpointCoverage",
            ProgressUnit::Items,
            0,
            1,
            ReplicaAuditCounter::None,
            None,
        );
        verify_replica_checkpoint_and_coverage(
            connection,
            &identity,
            &record_audit,
            conversation_count,
            participant_count,
            message_count,
            artifact_count,
            cached_moment_count,
            cached_moment_interaction_count,
        )?;
        progress.emit(
            4,
            ProgressState::Completed,
            "verifyReplicaCheckpointCoverage",
            ProgressUnit::Items,
            1,
            1,
            ReplicaAuditCounter::None,
            None,
        );
        with_replica_audit_heartbeat(
            &progress,
            5,
            "verifyReplicaFullTextIndex",
            ProgressUnit::Records,
            message_count,
            || verify_replica_fts(connection, &identity.0, message_count),
        )?;
        verify_replica_change_stream(
            connection,
            &identity.0,
            change_count,
            synchronization_run_count,
            &progress,
        )?;
    } else {
        let content_count = conversation_count
            .saturating_add(participant_count)
            .saturating_add(message_count)
            .saturating_add(artifact_count)
            .saturating_add(cached_moment_count)
            .saturating_add(cached_moment_interaction_count)
            .saturating_add(relationship_count)
            .saturating_add(message_artifact_count)
            .saturating_add(change_count)
            .saturating_add(membership_count)
            .saturating_add(table_count(connection, "coverage_state")?)
            .saturating_add(table_count(connection, "source_checkpoint")?)
            .saturating_add(table_count(connection, "sync_run")?)
            .saturating_add(table_count(connection, "cached_surface_state")?)
            .saturating_add(table_count(connection, "message_fts")?);
        if content_count != 0 {
            return Err(RestoreError::Integrity(
                "uninitialized replica contains orphan serving state".to_string(),
            ));
        }
        progress.emit_rows(
            2,
            ProgressState::Completed,
            "verifyReplicaCanonicalRecords",
            0,
            0,
            ReplicaAuditCounter::CanonicalRecords,
            None,
        );
        progress.emit_rows(
            3,
            ProgressState::Completed,
            "verifyReplicaMessageLinks",
            0,
            0,
            ReplicaAuditCounter::Links,
            None,
        );
        progress.emit(
            4,
            ProgressState::Started,
            "verifyReplicaCheckpointCoverage",
            ProgressUnit::Items,
            0,
            1,
            ReplicaAuditCounter::None,
            None,
        );
        progress.emit(
            4,
            ProgressState::Completed,
            "verifyReplicaCheckpointCoverage",
            ProgressUnit::Items,
            1,
            1,
            ReplicaAuditCounter::None,
            None,
        );
        progress.emit_rows(
            5,
            ProgressState::Completed,
            "verifyReplicaFullTextIndex",
            0,
            0,
            ReplicaAuditCounter::None,
            None,
        );
        progress.emit_rows(
            6,
            ProgressState::Completed,
            "verifyReplicaChangeStream",
            0,
            0,
            ReplicaAuditCounter::Changes,
            None,
        );
    }
    progress.emit(
        7,
        ProgressState::Started,
        "finalizeReplicaAudit",
        ProgressUnit::Items,
        0,
        1,
        ReplicaAuditCounter::None,
        None,
    );
    transaction.rollback()?;
    progress.emit(
        7,
        ProgressState::Completed,
        "finalizeReplicaAudit",
        ProgressUnit::Items,
        1,
        1,
        ReplicaAuditCounter::None,
        None,
    );
    Ok(ReplicaAuditReport {
        format_version: 1,
        privacy_safe_summary: true,
        initialized,
        schema_version: version,
        encrypted_at_rest: true,
        sqlite_integrity_verified: true,
        foreign_keys_verified: true,
        migration_ledger_verified: true,
        identity_checkpoint_verified: true,
        record_digests_verified: true,
        indexed_projections_verified: true,
        message_links_verified: true,
        full_text_index_verified: true,
        coverage_state_verified: true,
        change_stream_verified: true,
        transient_state_empty: true,
        conversation_count,
        participant_count,
        message_count,
        artifact_count,
        cached_moment_count,
        cached_moment_interaction_count,
        relationship_count,
        message_artifact_count,
        change_count,
    })
}

pub fn audit_replica_backup(
    backup_path: &Path,
    key: &ReplicaKey,
) -> Result<ReplicaBackupAuditReport, RestoreError> {
    audit_replica_backup_with_progress(backup_path, key, &NoProgress)
}

pub fn audit_replica_backup_with_progress(
    backup_path: &Path,
    key: &ReplicaKey,
    observer: &dyn ProgressObserver,
) -> Result<ReplicaBackupAuditReport, RestoreError> {
    verify_private_replica_files(backup_path)?;
    let replica_byte_count = replica_namespace_byte_count(backup_path)?;
    let mut progress = ReplicaAuditProgress::new(observer, replica_byte_count);
    progress.emit(
        0,
        ProgressState::Planned,
        "planReplicaBackupAudit",
        ProgressUnit::Bytes,
        0,
        replica_byte_count,
        ReplicaAuditCounter::None,
        None,
    );
    let (mut connection, _) = open_existing_replica_read_only(backup_path, key)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let connection = &*transaction;
    let version = schema_version(connection)?;
    if version == 0 || version >= CURRENT_SCHEMA_VERSION {
        return Err(RestoreError::Integrity(
            "replica backup audit requires a non-empty older supported schema".to_string(),
        ));
    }
    validate_replica_migration_ledger(connection, version)?;
    let conversation_count = table_count(connection, "conversation")?;
    let participant_count = table_count(connection, "participant")?;
    let message_count = table_count(connection, "message")?;
    let artifact_count = table_count(connection, "artifact")?;
    let cached_moment_count = if version >= 4 {
        table_count(connection, "cached_moment")?
    } else {
        0
    };
    let cached_moment_interaction_count = if version >= 4 {
        table_count(connection, "cached_moment_interaction")?
    } else {
        0
    };
    let membership_count = table_count(connection, "conversation_participant")?;
    let relationship_count = table_count(connection, "message_relationship")?;
    let message_artifact_count = table_count(connection, "message_artifact")?;
    let change_count = if version >= 2 {
        table_count(connection, "change_log")?
    } else {
        0
    };
    let synchronization_run_count = if version >= 2 {
        table_count(connection, "sync_run")?
    } else {
        0
    };
    progress.set_totals(ReplicaAuditTotals {
        conversation_count,
        message_count,
        canonical_record_count: conversation_count
            .saturating_add(participant_count)
            .saturating_add(message_count)
            .saturating_add(artifact_count)
            .saturating_add(cached_moment_count)
            .saturating_add(cached_moment_interaction_count),
        link_record_count: membership_count
            .saturating_add(relationship_count)
            .saturating_add(message_artifact_count),
        change_record_count: change_count.saturating_add(synchronization_run_count),
    });
    progress.emit(
        0,
        ProgressState::Completed,
        "openReplicaBackupAuditSnapshot",
        ProgressUnit::Bytes,
        replica_byte_count,
        replica_byte_count,
        ReplicaAuditCounter::None,
        None,
    );
    with_replica_audit_heartbeat(
        &progress,
        1,
        "verifyReplicaSQLiteIntegrity",
        ProgressUnit::Bytes,
        replica_byte_count,
        || {
            verify_sqlite_integrity(connection)?;
            verify_foreign_keys(connection)
        },
    )?;
    let replica_format_version: i64 = connection.query_row(
        "SELECT replica_format_version FROM replica_schema WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    let replica_format_version = u32::try_from(replica_format_version)
        .map_err(|_| RestoreError::Integrity("replica backup format is invalid".to_string()))?;
    let identity_count = table_count(connection, "replica_identity")?;
    if identity_count > 1 {
        return Err(RestoreError::Integrity(
            "replica backup contains multiple account identities".to_string(),
        ));
    }
    let initialized = identity_count == 1;
    let mut record_audit = ReplicaRecordAudit::default();
    let mut checkpoint_state_verified = version < 2;
    let mut full_text_index_verified = version < 2;
    let mut change_stream_verified = version < 2;
    let mut transient_state_empty = version < 3;
    if initialized {
        let identity: (String, String, bool, String, String) = connection.query_row(
            "SELECT account_id, current_source_fingerprint, restoration_complete,
                    created_at_unix_nanoseconds, updated_at_unix_nanoseconds
             FROM replica_identity WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        verify_positive_timestamp(&identity.3, "replica backup creation")?;
        verify_positive_timestamp(&identity.4, "replica backup update")?;
        if identity.0.is_empty() || identity.1.is_empty() {
            return Err(RestoreError::Integrity(
                "replica backup identity is incomplete".to_string(),
            ));
        }
        let include_cached = version >= 4;
        record_audit = audit_replica_records(connection, &identity.0, include_cached, &progress)?;
        verify_replica_message_links(connection, &identity.0, &record_audit, &progress)?;
        progress.emit(
            4,
            ProgressState::Started,
            "verifyReplicaCheckpointCoverage",
            ProgressUnit::Items,
            0,
            1,
            ReplicaAuditCounter::None,
            None,
        );
        verify_legacy_replica_coverage(connection, &identity, &record_audit, include_cached)?;
        if version >= 2 {
            verify_legacy_replica_checkpoint(connection, &identity, &record_audit)?;
        }
        progress.emit(
            4,
            ProgressState::Completed,
            "verifyReplicaCheckpointCoverage",
            ProgressUnit::Items,
            1,
            1,
            ReplicaAuditCounter::None,
            None,
        );
        if version >= 2 {
            with_replica_audit_heartbeat(
                &progress,
                5,
                "verifyReplicaFullTextIndex",
                ProgressUnit::Records,
                record_audit.message_count,
                || verify_replica_fts(connection, &identity.0, record_audit.message_count),
            )?;
            verify_replica_change_stream(
                connection,
                &identity.0,
                change_count,
                synchronization_run_count,
                &progress,
            )?;
            checkpoint_state_verified = true;
            full_text_index_verified = true;
            change_stream_verified = true;
        } else {
            progress.emit_rows(
                5,
                ProgressState::Completed,
                "verifyReplicaFullTextIndex",
                0,
                0,
                ReplicaAuditCounter::None,
                None,
            );
            progress.emit_rows(
                6,
                ProgressState::Completed,
                "verifyReplicaChangeStream",
                0,
                0,
                ReplicaAuditCounter::Changes,
                None,
            );
        }
        if version >= 3 {
            let _ = replica_id(connection)?;
            if table_count(connection, "sync_seen")? != 0 {
                return Err(RestoreError::Integrity(
                    "replica backup contains reconciliation staging".to_string(),
                ));
            }
            transient_state_empty = true;
        }
    } else {
        let mut content_count = conversation_count
            .saturating_add(participant_count)
            .saturating_add(message_count)
            .saturating_add(artifact_count)
            .saturating_add(membership_count)
            .saturating_add(relationship_count)
            .saturating_add(message_artifact_count)
            .saturating_add(table_count(connection, "coverage_state")?);
        if version >= 2 {
            content_count = content_count
                .saturating_add(table_count(connection, "source_checkpoint")?)
                .saturating_add(synchronization_run_count)
                .saturating_add(change_count)
                .saturating_add(table_count(connection, "message_fts")?);
            checkpoint_state_verified = true;
            full_text_index_verified = true;
            change_stream_verified = true;
        }
        if version >= 3 {
            let _ = replica_id(connection)?;
            content_count = content_count.saturating_add(table_count(connection, "sync_seen")?);
            transient_state_empty = true;
        }
        if version >= 4 {
            content_count = content_count
                .saturating_add(cached_moment_count)
                .saturating_add(cached_moment_interaction_count)
                .saturating_add(table_count(connection, "cached_surface_state")?);
        }
        if content_count != 0 {
            return Err(RestoreError::Integrity(
                "uninitialized replica backup contains orphan serving state".to_string(),
            ));
        }
        progress.emit_rows(
            2,
            ProgressState::Completed,
            "verifyReplicaCanonicalRecords",
            0,
            0,
            ReplicaAuditCounter::CanonicalRecords,
            None,
        );
        progress.emit_rows(
            3,
            ProgressState::Completed,
            "verifyReplicaMessageLinks",
            0,
            0,
            ReplicaAuditCounter::Links,
            None,
        );
        progress.emit(
            4,
            ProgressState::Started,
            "verifyReplicaCheckpointCoverage",
            ProgressUnit::Items,
            0,
            1,
            ReplicaAuditCounter::None,
            None,
        );
        progress.emit(
            4,
            ProgressState::Completed,
            "verifyReplicaCheckpointCoverage",
            ProgressUnit::Items,
            1,
            1,
            ReplicaAuditCounter::None,
            None,
        );
        progress.emit_rows(
            5,
            ProgressState::Completed,
            "verifyReplicaFullTextIndex",
            0,
            0,
            ReplicaAuditCounter::None,
            None,
        );
        progress.emit_rows(
            6,
            ProgressState::Completed,
            "verifyReplicaChangeStream",
            0,
            0,
            ReplicaAuditCounter::Changes,
            None,
        );
    }
    progress.emit(
        7,
        ProgressState::Started,
        "finalizeReplicaBackupAudit",
        ProgressUnit::Items,
        0,
        1,
        ReplicaAuditCounter::None,
        None,
    );
    transaction.rollback()?;
    progress.emit(
        7,
        ProgressState::Completed,
        "finalizeReplicaBackupAudit",
        ProgressUnit::Items,
        1,
        1,
        ReplicaAuditCounter::None,
        None,
    );
    Ok(ReplicaBackupAuditReport {
        format_version: 1,
        privacy_safe_summary: true,
        schema_version: version,
        replica_format_version,
        initialized,
        encrypted_at_rest: true,
        sqlite_integrity_verified: true,
        foreign_keys_verified: true,
        migration_ledger_verified: true,
        record_digests_verified: true,
        indexed_projections_verified: true,
        message_links_verified: true,
        coverage_state_verified: true,
        checkpoint_state_verified,
        full_text_index_verified,
        change_stream_verified,
        transient_state_empty,
        conversation_count: record_audit.conversation_count,
        participant_count: record_audit.participant_count,
        message_count: record_audit.message_count,
        artifact_count: record_audit.artifact_count,
        relationship_count: record_audit.relationships.len() as u64,
        message_artifact_count: record_audit.message_artifacts.len() as u64,
    })
}

pub fn prepare_replica_recovery(
    backup_path: &Path,
    candidate_path: &Path,
    key: &ReplicaKey,
) -> Result<ReplicaRecoveryPreparationReport, RestoreError> {
    let source_seal = seal_replica_storage(backup_path)?;
    let source_audit = audit_replica_backup(backup_path, key)?;
    if seal_replica_storage(backup_path)? != source_seal {
        return Err(RestoreError::Integrity(
            "replica backup changed while it was audited".to_string(),
        ));
    }
    let backup = fs::canonicalize(backup_path)?;
    let candidate_parent = candidate_path
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath("recovery candidate has no parent".to_string()))?;
    ensure_private_directory(candidate_parent)?;
    let candidate_name = candidate_path.file_name().ok_or_else(|| {
        RestoreError::UnsafePath("recovery candidate has no filename".to_string())
    })?;
    let candidate = fs::canonicalize(candidate_parent)?.join(candidate_name);
    ensure_distinct_replica_namespaces(&backup, &candidate)?;
    ensure_replica_namespace_absent(&candidate, "recovery candidate")?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&candidate)?;
    let result = (|| {
        let (source, _) = open_existing_replica_read_only(&backup, key)?;
        let mut destination = open_keyed_connection(&candidate, key)?;
        let backup_copy = Backup::new(&source, &mut destination)?;
        backup_copy.run_to_completion(128, Duration::from_millis(2), None)?;
        drop(backup_copy);
        destination.execute_batch(
            "PRAGMA wal_checkpoint(TRUNCATE);
             PRAGMA journal_mode = DELETE;
             PRAGMA synchronous = FULL;",
        )?;
        drop(destination);
        drop(source);
        if seal_replica_storage(&backup)? != source_seal {
            return Err(RestoreError::Integrity(
                "replica backup changed while it was copied".to_string(),
            ));
        }
        secure_replica_files(&candidate)?;

        let copied_audit = audit_replica_backup(&candidate, key)?;
        if serde_json::to_vec(&copied_audit)? != serde_json::to_vec(&source_audit)? {
            return Err(RestoreError::Integrity(
                "recovery candidate differs from its audited backup".to_string(),
            ));
        }
        let mut recovered = open_keyed_connection(&candidate, key)?;
        let copied_version = schema_version(&recovered)?;
        if copied_version != source_audit.schema_version {
            return Err(RestoreError::Integrity(
                "recovery candidate schema changed before migration".to_string(),
            ));
        }
        let copied_cached_moment_count = if copied_version >= 4 {
            table_count(&recovered, "cached_moment")?
        } else {
            0
        };
        let copied_cached_moment_interaction_count = if copied_version >= 4 {
            table_count(&recovered, "cached_moment_interaction")?
        } else {
            0
        };
        validate_replica_migration_ledger(&recovered, copied_version)?;
        apply_migrations(&mut recovered, copied_version)?;
        validate_replica_migration_ledger(&recovered, CURRENT_SCHEMA_VERSION)?;
        recovered.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             PRAGMA wal_autocheckpoint = 1000;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )?;
        secure_replica_files(&candidate)?;
        drop(recovered);

        let current_audit = audit_replica(&candidate, key)?;
        if current_audit.initialized != source_audit.initialized
            || current_audit.conversation_count != source_audit.conversation_count
            || current_audit.participant_count != source_audit.participant_count
            || current_audit.message_count != source_audit.message_count
            || current_audit.artifact_count != source_audit.artifact_count
            || current_audit.relationship_count != source_audit.relationship_count
            || current_audit.message_artifact_count != source_audit.message_artifact_count
            || current_audit.cached_moment_count != copied_cached_moment_count
            || current_audit.cached_moment_interaction_count
                != copied_cached_moment_interaction_count
        {
            return Err(RestoreError::Integrity(
                "migrated recovery candidate differs from its source backup".to_string(),
            ));
        }
        Ok(ReplicaRecoveryPreparationReport {
            format_version: 1,
            privacy_safe_summary: true,
            source_backup_verified: true,
            source_schema_version: source_audit.schema_version,
            current_schema_version: current_audit.schema_version,
            candidate_audit_verified: true,
            initialized: current_audit.initialized,
            encrypted_at_rest: true,
            conversation_count: current_audit.conversation_count,
            participant_count: current_audit.participant_count,
            message_count: current_audit.message_count,
            artifact_count: current_audit.artifact_count,
            relationship_count: current_audit.relationship_count,
            message_artifact_count: current_audit.message_artifact_count,
        })
    })();
    if result.is_err() {
        remove_failed_replica_files(&candidate);
    }
    result
}

pub fn synchronize_replica(
    archive_directory: &Path,
    replica_path: &Path,
    key: &ReplicaKey,
) -> Result<ReplicaSyncReport, RestoreError> {
    synchronize_replica_with_progress(archive_directory, replica_path, key, &NoProgress)
}

pub fn synchronize_replica_with_progress(
    archive_directory: &Path,
    replica_path: &Path,
    key: &ReplicaKey,
    progress: &dyn ProgressObserver,
) -> Result<ReplicaSyncReport, RestoreError> {
    ensure_private_directory(archive_directory)?;
    let report = load_report(archive_directory)?;
    require_authoritative_archive(&report)?;
    if report.format_version >= 3 {
        audit_archive_with_progress(archive_directory, progress)?;
    }
    synchronize_audited_replica_with_progress(
        archive_directory,
        replica_path,
        key,
        &report,
        progress,
    )
}

/// Synchronizes from a publication that the caller has already audited and
/// sealed. Public callers use `synchronize_replica_with_progress`, which
/// performs its own audit before any replica mutation.
pub(crate) fn synchronize_audited_replica_with_progress(
    archive_directory: &Path,
    replica_path: &Path,
    key: &ReplicaKey,
    report: &RestorationReport,
    progress: &dyn ProgressObserver,
) -> Result<ReplicaSyncReport, RestoreError> {
    ensure_private_directory(archive_directory)?;
    require_authoritative_archive(report)?;
    let replica_namespace_was_absent = replica_namespace_is_absent(replica_path)?;
    let result = (|| {
        let mut opened = open_replica(replica_path, key)?;
        let identity: Option<(String, Option<String>, Option<String>)> = opened
            .connection
            .query_row(
                "SELECT account_id, current_source_fingerprint, self_participant_id
                 FROM replica_identity WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((account_id, previous_fingerprint, existing_self_participant_id)) = identity
        else {
            return Err(RestoreError::Integrity(
                "replica must be bootstrapped before synchronization".to_string(),
            ));
        };
        require_account(&report.account_id, &account_id)?;
        require_compatible_self_participant(
            existing_self_participant_id.as_deref(),
            report.self_participant_id.as_deref(),
        )?;
        let previous_fingerprint = previous_fingerprint.ok_or_else(|| {
            RestoreError::Integrity("replica has no authoritative source checkpoint".to_string())
        })?;
        let incoming_coverage = load_archive_coverage(archive_directory)?;
        let stored_state = load_coverage_state(&opened.connection, &account_id)?;
        if let Some((stored_report, stored_coverage)) = stored_state.as_ref() {
            ensure_partial_database_transition_is_lossless(stored_report, stored_coverage, report)?;
        }
        let unchanged_revision =
            stored_state
                .as_ref()
                .is_some_and(|(stored_report, stored_coverage)| {
                    archive_revision_digest(stored_report, stored_coverage)
                        == archive_revision_digest(report, &incoming_coverage)
                });
        if previous_fingerprint == report.source_fingerprint && unchanged_revision {
            emit_idempotent_replica_progress(report, archive_directory, replica_path, progress)?;
            return sync_report(
                &opened.connection,
                &account_id,
                &previous_fingerprint,
                report,
                true,
                SyncCounts::default(),
                None,
            );
        }
        let plan =
            preflight_replica_application(archive_directory, replica_path, report, progress)?;
        let (counts, committed) = reconcile_archive_transactionally(
            &mut opened.connection,
            archive_directory,
            report,
            &previous_fingerprint,
            &plan,
            replica_path,
            progress,
        )?;
        emit_replica_checkpoint_progress(&plan, replica_path, ProgressState::Started, progress)?;
        checkpoint_and_secure(&opened.connection, replica_path)?;
        emit_replica_checkpoint_progress(&plan, replica_path, ProgressState::Completed, progress)?;
        sync_report(
            &opened.connection,
            &account_id,
            &previous_fingerprint,
            report,
            false,
            counts,
            Some(committed),
        )
    })();
    if result.is_err() && replica_namespace_was_absent {
        remove_failed_replica_files(replica_path);
    }
    result
}

pub fn replica_matches_authoritative_archive(
    archive_directory: &Path,
    replica_path: &Path,
    key: &ReplicaKey,
) -> Result<bool, RestoreError> {
    ensure_private_directory(archive_directory)?;
    let report = load_report(archive_directory)?;
    require_authoritative_archive(&report)?;
    let incoming_coverage = load_archive_coverage(archive_directory)?;
    let opened = open_replica(replica_path, key)?;
    let identity: Option<(String, Option<String>, Option<String>)> = opened
        .connection
        .query_row(
            "SELECT account_id, current_source_fingerprint, self_participant_id
             FROM replica_identity WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((account_id, source_fingerprint, self_participant_id)) = identity else {
        return Ok(false);
    };
    if account_id != report.account_id
        || source_fingerprint.as_deref() != Some(report.source_fingerprint.as_str())
        || self_participant_id != report.self_participant_id
    {
        return Ok(false);
    }
    let stored_state = load_coverage_state(&opened.connection, &account_id)?;
    Ok(stored_state
        .as_ref()
        .is_some_and(|(stored_report, stored_coverage)| {
            archive_revision_digest(stored_report, stored_coverage)
                == archive_revision_digest(&report, &incoming_coverage)
        }))
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
    if account_id.is_empty() {
        return Err(RestoreError::Integrity(
            "replica change stream has an empty account identity".to_string(),
        ));
    }
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
    let scan_limit = limit.saturating_mul(8).saturating_add(1).min(8_001);
    let query_limit = checked_usize_i64(scan_limit)?;
    let mut statement = match opened.connection.prepare(
        "SELECT sequence, source_fingerprint, change_kind, entity_kind, entity_id,
                conversation_id, record_sha256, observed_at_unix_nanoseconds
         FROM change_log
         WHERE account_id = ?1 AND sequence > ?2
         ORDER BY sequence LIMIT ?3",
    ) {
        Ok(statement) => statement,
        Err(_) => {
            return Ok(ReplicaChangePage {
                account_id,
                items: Vec::new(),
                next_cursor: None,
                omitted_item_count: 0,
                limitation_codes: vec!["replicaChangeTableUnavailable".to_string()],
            });
        }
    };
    let values = match statement.query_map(
        params![account_id, checked_i64(after)?, query_limit],
        |row| {
            let sequence = row.get::<_, i64>(0)?;
            let remainder = (|| {
                Ok::<_, rusqlite::Error>((
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })();
            Ok((sequence, remainder))
        },
    ) {
        Ok(values) => values,
        Err(_) => {
            return Ok(ReplicaChangePage {
                account_id,
                items: Vec::new(),
                next_cursor: None,
                omitted_item_count: 0,
                limitation_codes: vec!["replicaChangeTableUnavailable".to_string()],
            });
        }
    };
    let mut items = Vec::with_capacity(limit);
    let mut omitted_item_count = 0_u64;
    let mut last_scanned_sequence = None;
    for value in values {
        let (sequence, remainder) = match value {
            Ok(value) => value,
            Err(_) => {
                omitted_item_count = omitted_item_count.saturating_add(1);
                continue;
            }
        };
        let Ok(sequence) = u64::try_from(sequence) else {
            omitted_item_count = omitted_item_count.saturating_add(1);
            continue;
        };
        last_scanned_sequence = Some(sequence);
        let Ok((source, kind, entity_kind, entity_id, conversation, digest, timestamp)) = remainder
        else {
            omitted_item_count = omitted_item_count.saturating_add(1);
            continue;
        };
        let Ok(observed_at_unix_nanoseconds) = timestamp.parse() else {
            omitted_item_count = omitted_item_count.saturating_add(1);
            continue;
        };
        items.push(ReplicaChange {
            sequence,
            source_fingerprint: source,
            change_kind: kind,
            entity_kind,
            entity_id,
            conversation_id: conversation,
            record_sha256: digest,
            observed_at_unix_nanoseconds,
        });
        if items.len() == limit {
            break;
        }
    }
    let next_cursor = last_scanned_sequence.map(|after_sequence| {
        encode_change_cursor(&ReplicaChangeCursor {
            format_version: 1,
            account_id: account_id.clone(),
            replica_id,
            after_sequence,
        })
    });
    Ok(ReplicaChangePage {
        account_id,
        items,
        next_cursor,
        omitted_item_count,
        limitation_codes: (omitted_item_count > 0)
            .then_some("malformedReplicaChangeOmitted".to_string())
            .into_iter()
            .collect(),
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
    // Scan beyond the requested healthy-item limit so malformed rows do not
    // shrink a page unnecessarily. The bounded multiplier prevents a damaged
    // shard from turning one request into an unbounded scan; a cursor still
    // advances across every inspected row.
    let scan_limit = limit.saturating_mul(8).saturating_add(1).min(8_001);
    let query_limit = checked_usize_i64(scan_limit)?;
    let reply_filter = if filter.reply_target_canonical_id.is_some() {
        "EXISTS(
           SELECT 1 FROM message_relationship AS r
           WHERE r.account_id = m.account_id
             AND r.source_canonical_id = m.canonical_id
             AND r.target_canonical_id = :reply_target
         )"
    } else {
        ":reply_target IS NULL"
    };
    let attachment_filter = if filter.has_attachment.is_some() {
        "(
           :has_attachment = 1 AND EXISTS(
             SELECT 1 FROM message_artifact AS a
             WHERE a.account_id = m.account_id AND a.canonical_id = m.canonical_id
           )
         ) OR (
           :has_attachment = 0 AND NOT EXISTS(
             SELECT 1 FROM message_artifact AS a
             WHERE a.account_id = m.account_id AND a.canonical_id = m.canonical_id
           )
         )"
    } else {
        ":has_attachment IS NULL"
    };
    let full_text_filter = if filter.full_text_query.is_some() {
        "EXISTS(
           SELECT 1 FROM message_fts
           WHERE message_fts.account_id = m.account_id
             AND message_fts.canonical_id = m.canonical_id
             AND message_fts MATCH :full_text
         )"
    } else {
        ":full_text IS NULL"
    };
    // Optional indexes/link tables are referenced only when the caller asks
    // for their filter. Losing FTS, relationship, or artifact links therefore
    // cannot disable ordinary message pagination from the healthy base table.
    let query = format!(
        "SELECT m.created_at_unix, m.conversation_id, m.conversation_ordinal,
                m.canonical_id, m.record_json
         FROM message AS m
         WHERE m.account_id = :account
           AND (:conversation IS NULL OR m.conversation_id = :conversation)
           AND (:sender IS NULL OR m.sender_id = :sender)
           AND (:direction IS NULL OR m.direction = :direction)
           AND (:logical_type IS NULL OR m.logical_type = :logical_type)
           AND (:sub_type IS NULL OR m.sub_type = :sub_type)
           AND (:not_before IS NULL OR m.created_at_unix >= :not_before)
           AND (:not_after IS NULL OR m.created_at_unix <= :not_after)
           AND ({reply_filter})
           AND ({attachment_filter})
           AND ({full_text_filter})
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
         LIMIT :limit"
    );
    let mut statement = match opened.connection.prepare(&query) {
        Ok(statement) => statement,
        Err(_) => {
            return Ok(ReplicaMessagePage {
                account_id,
                source_fingerprint,
                checkpoint_revision,
                items: Vec::new(),
                next_cursor: None,
                omitted_item_count: 0,
                limitation_codes: vec!["replicaMessageQueryUnavailable".to_string()],
            });
        }
    };
    let rows = match statement.query_map(
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
        |row| {
            Ok((
                (
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ),
                row.get::<_, Vec<u8>>(4),
            ))
        },
    ) {
        Ok(rows) => rows,
        Err(_) => {
            return Ok(ReplicaMessagePage {
                account_id,
                source_fingerprint,
                checkpoint_revision,
                items: Vec::new(),
                next_cursor: None,
                omitted_item_count: 0,
                limitation_codes: vec!["replicaMessageQueryUnavailable".to_string()],
            });
        }
    };
    let mut items = Vec::new();
    let mut omitted_item_count = 0_u64;
    let mut last_scanned_identity = None;
    let mut has_more = false;
    let mut scanned_count = 0_usize;
    for row in rows {
        scanned_count = scanned_count.saturating_add(1);
        let ((indexed_created_at, indexed_conversation_id, indexed_ordinal, indexed_id), bytes) =
            match row {
                Ok(value) => value,
                Err(_) => {
                    omitted_item_count = omitted_item_count.saturating_add(1);
                    continue;
                }
            };
        let indexed_ordinal = u64::try_from(indexed_ordinal).unwrap_or_default();
        last_scanned_identity = Some((
            indexed_created_at.unwrap_or(i64::MIN),
            indexed_conversation_id.clone(),
            indexed_ordinal,
            indexed_id.clone(),
        ));
        let Ok(bytes) = bytes else {
            omitted_item_count = omitted_item_count.saturating_add(1);
            continue;
        };
        let Ok(message) = serde_json::from_slice::<CanonicalMessage>(&bytes) else {
            omitted_item_count = omitted_item_count.saturating_add(1);
            continue;
        };
        if message.account_id != account_id
            || message.canonical_id.is_empty()
            || message.conversation_id.is_empty()
            || message.canonical_id != indexed_id
            || message.conversation_id != indexed_conversation_id
            || message.conversation_ordinal != indexed_ordinal
            || message.created_at_unix != indexed_created_at
        {
            omitted_item_count = omitted_item_count.saturating_add(1);
            continue;
        }
        items.push(message);
        if items.len() == limit {
            has_more = true;
            break;
        }
    }
    if items.len() < limit && scanned_count == scan_limit && last_scanned_identity.is_some() {
        has_more = true;
    }
    let next_cursor = if has_more {
        last_scanned_identity.map(
            |(
                after_sort_time,
                after_conversation_id,
                after_conversation_ordinal,
                after_canonical_id,
            )| {
                encode_message_cursor(&ReplicaMessageCursor {
                    format_version: 2,
                    account_id: account_id.clone(),
                    replica_id: generation,
                    source_fingerprint: source_fingerprint.clone(),
                    checkpoint_revision: checkpoint_revision.clone(),
                    filter_sha256,
                    after_sort_time,
                    after_conversation_id,
                    after_conversation_ordinal,
                    after_canonical_id,
                })
            },
        )
    } else {
        None
    };
    Ok(ReplicaMessagePage {
        account_id,
        source_fingerprint,
        checkpoint_revision,
        items,
        next_cursor,
        omitted_item_count,
        limitation_codes: (omitted_item_count > 0)
            .then_some("malformedReplicaMessageOmitted".to_string())
            .into_iter()
            .collect(),
    })
}

pub(crate) fn count_replica_messages_for_scopes(
    replica_path: &Path,
    key: &ReplicaKey,
    scopes: &[(String, Option<i64>, Option<i64>)],
) -> Result<u64, RestoreError> {
    let opened = open_replica(replica_path, key)?;
    let (account_id, _) = current_replica_identity(&opened.connection)?;
    let mut statement = match opened.connection.prepare(
        "SELECT COUNT(*) FROM message
         WHERE account_id = :account
           AND conversation_id = :conversation
           AND (:not_before IS NULL OR created_at_unix >= :not_before)
           AND (:not_after IS NULL OR created_at_unix <= :not_after)",
    ) {
        Ok(statement) => statement,
        // Message planning is advisory. A missing/corrupt message surface is
        // represented by the serving page's limitation code during export;
        // it must not prevent healthy conversation/contact data from being
        // projected.
        Err(_) => return Ok(0),
    };
    let mut total = 0_u64;
    let mut seen = HashSet::new();
    for (conversation_id, not_before_unix, not_after_unix) in scopes {
        if conversation_id.is_empty() {
            return Err(RestoreError::Integrity(
                "AI context scope has an empty conversation identity".to_string(),
            ));
        }
        if !seen.insert(conversation_id) {
            return Err(RestoreError::Integrity(
                "AI context scope repeats a conversation identity".to_string(),
            ));
        }
        if not_before_unix
            .zip(*not_after_unix)
            .is_some_and(|(start, end)| start > end)
        {
            return Err(RestoreError::Integrity(
                "AI context scope has an inverted time range".to_string(),
            ));
        }
        let count: i64 = match statement.query_row(
            named_params! {
                ":account": account_id,
                ":conversation": conversation_id,
                ":not_before": not_before_unix,
                ":not_after": not_after_unix,
            },
            |row| row.get(0),
        ) {
            Ok(count) => count,
            Err(_) => continue,
        };
        total = total.saturating_add(u64::try_from(count).map_err(|_| {
            RestoreError::Integrity("replica returned a negative message count".to_string())
        })?);
    }
    Ok(total)
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

    let mut limitation_codes = BTreeSet::new();
    let coverage_bytes: Option<Vec<u8>> = match opened
        .connection
        .query_row(
            "SELECT coverage_json FROM cached_surface_state WHERE account_id = ?1",
            [&account_id],
            |row| row.get(0),
        )
        .optional()
    {
        Ok(value) => value,
        Err(_) => {
            limitation_codes.insert("cachedMomentCoverageUnavailable".to_string());
            None
        }
    };
    let coverage: Option<crate::CachedSurfaceCoverage> =
        coverage_bytes.and_then(|bytes| match serde_json::from_slice(&bytes) {
            Ok(value) if validate_cached_coverage_schema(&value).is_ok() => Some(value),
            Err(_) => {
                limitation_codes.insert("malformedCachedMomentCoverageOmitted".to_string());
                None
            }
            Ok(_) => {
                limitation_codes.insert("malformedCachedMomentCoverageOmitted".to_string());
                None
            }
        });
    if let Some(coverage) = coverage.as_ref() {
        limitation_codes.extend(coverage.limitation_codes.iter().cloned());
        if coverage.omitted_row_count > 0 {
            limitation_codes.insert("cachedSurfaceSourceRowsOmitted".to_string());
        }
    }
    let reported_availability = match coverage.as_ref() {
        None => ReplicaCachedSurfaceAvailability::Unavailable,
        Some(coverage) if !coverage.source_database_present => {
            ReplicaCachedSurfaceAvailability::Unavailable
        }
        Some(coverage)
            if coverage.moment_count == 0
                && coverage.limitation_codes.iter().any(|code| {
                    matches!(
                        code.as_str(),
                        "cachedSurfaceDatabaseUnavailable"
                            | "cachedSurfaceTableUnavailable"
                            | "cachedSurfaceSourceCountUnavailable"
                    )
                }) =>
        {
            ReplicaCachedSurfaceAvailability::Unavailable
        }
        Some(coverage) if coverage.moment_count == 0 && coverage.omitted_row_count > 0 => {
            ReplicaCachedSurfaceAvailability::Unavailable
        }
        Some(coverage) if coverage.moment_count == 0 => {
            ReplicaCachedSurfaceAvailability::AvailableEmpty
        }
        Some(_) => ReplicaCachedSurfaceAvailability::Available,
    };
    if reported_availability == ReplicaCachedSurfaceAvailability::Unavailable {
        limitation_codes.insert("cachedMomentSurfaceUnavailable".to_string());
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
    let scan_limit = limit.saturating_mul(8).saturating_add(1).min(8_001);
    let query_limit = checked_usize_i64(scan_limit)?;
    let mut statement = match opened.connection.prepare(
        "SELECT created_at_unix, canonical_id, record_json FROM cached_moment
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
    ) {
        Ok(statement) => statement,
        Err(_) => {
            limitation_codes.insert("cachedMomentTableUnavailable".to_string());
            return Ok(ReplicaCachedMomentPage {
                account_id,
                source_fingerprint,
                checkpoint_revision,
                availability: ReplicaCachedSurfaceAvailability::Unavailable,
                cache_completeness: coverage
                    .as_ref()
                    .map(|coverage| coverage.cache_completeness),
                observed_at: coverage.map(|coverage| coverage.observed_at),
                items: Vec::new(),
                next_cursor: None,
                omitted_item_count: 0,
                limitation_codes: limitation_codes.into_iter().collect(),
            });
        }
    };
    let rows = match statement.query_map(
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
        |row| {
            Ok((
                (row.get::<_, Option<i64>>(0)?, row.get::<_, String>(1)?),
                row.get::<_, Vec<u8>>(2),
            ))
        },
    ) {
        Ok(rows) => rows,
        Err(_) => {
            limitation_codes.insert("cachedMomentTableUnavailable".to_string());
            return Ok(ReplicaCachedMomentPage {
                account_id,
                source_fingerprint,
                checkpoint_revision,
                availability: ReplicaCachedSurfaceAvailability::Unavailable,
                cache_completeness: coverage
                    .as_ref()
                    .map(|coverage| coverage.cache_completeness),
                observed_at: coverage.map(|coverage| coverage.observed_at),
                items: Vec::new(),
                next_cursor: None,
                omitted_item_count: 0,
                limitation_codes: limitation_codes.into_iter().collect(),
            });
        }
    };
    let mut items = Vec::with_capacity(limit);
    let mut omitted_item_count = 0_u64;
    let mut last_scanned_identity = None;
    let mut scanned_count = 0_usize;
    let mut has_more = false;
    for row in rows {
        scanned_count = scanned_count.saturating_add(1);
        let ((indexed_created_at, indexed_id), bytes) = match row {
            Ok(value) => value,
            Err(_) => {
                omitted_item_count = omitted_item_count.saturating_add(1);
                continue;
            }
        };
        last_scanned_identity = Some((indexed_created_at.unwrap_or(i64::MIN), indexed_id.clone()));
        let Ok(bytes) = bytes else {
            omitted_item_count = omitted_item_count.saturating_add(1);
            continue;
        };
        let Ok(moment) = serde_json::from_slice::<crate::CanonicalCachedMoment>(&bytes) else {
            omitted_item_count = omitted_item_count.saturating_add(1);
            continue;
        };
        if moment.account_id != account_id
            || moment.canonical_id.is_empty()
            || moment.canonical_id != indexed_id
            || moment.created_at_unix != indexed_created_at
        {
            omitted_item_count = omitted_item_count.saturating_add(1);
            continue;
        }
        items.push(moment);
        if items.len() == limit {
            has_more = true;
            break;
        }
    }
    if items.len() < limit && scanned_count == scan_limit && last_scanned_identity.is_some() {
        has_more = true;
    }
    if omitted_item_count > 0 {
        limitation_codes.insert("malformedCachedMomentOmitted".to_string());
    }
    let availability = if !items.is_empty() {
        if reported_availability != ReplicaCachedSurfaceAvailability::Available {
            limitation_codes.insert("cachedMomentCoverageDisagreesWithRecords".to_string());
        }
        ReplicaCachedSurfaceAvailability::Available
    } else {
        reported_availability
    };
    let next_cursor = if has_more {
        last_scanned_identity.map(|(after_created_at_unix, after_canonical_id)| {
            encode_cached_moment_cursor(&ReplicaCachedMomentCursor {
                format_version: 1,
                account_id: account_id.clone(),
                replica_id: generation,
                source_fingerprint: source_fingerprint.clone(),
                checkpoint_revision: checkpoint_revision.clone(),
                filter_sha256,
                after_created_at_unix,
                after_canonical_id,
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
        omitted_item_count,
        limitation_codes: limitation_codes.into_iter().collect(),
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
    let bytes: Option<Vec<u8>> = match opened
        .connection
        .query_row(
            "SELECT record_json FROM message
             WHERE account_id = ?1 AND canonical_id = ?2",
            params![account_id, canonical_id],
            |row| row.get(0),
        )
        .optional()
    {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let Ok(message) = serde_json::from_slice::<CanonicalMessage>(&bytes) else {
        return Ok(None);
    };
    if message.account_id != account_id
        || message.canonical_id != canonical_id
        || message.conversation_id.is_empty()
    {
        return Ok(None);
    }
    Ok(Some(message))
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
    let limit = requested_limit.clamp(1, 1_000);
    let scan_limit = limit.saturating_mul(8).min(8_000);
    let query_limit = checked_usize_i64(scan_limit)?;
    let mut statement = match opened.connection.prepare(
        "SELECT conversation_ordinal, canonical_id, created_at_unix, record_json FROM message
         WHERE account_id = ?1 AND conversation_id = ?2
           AND (?3 IS NULL OR created_at_unix >= ?3)
           AND (?4 IS NULL OR created_at_unix <= ?4)
         ORDER BY conversation_ordinal DESC, canonical_id DESC LIMIT ?5",
    ) {
        Ok(statement) => statement,
        Err(_) => return Ok(Vec::new()),
    };
    let rows = match statement.query_map(
        params![
            account_id,
            conversation_id,
            not_before_unix,
            not_after_unix,
            query_limit
        ],
        |row| {
            Ok((
                (
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ),
                row.get::<_, Vec<u8>>(3),
            ))
        },
    ) {
        Ok(rows) => rows,
        Err(_) => return Ok(Vec::new()),
    };
    let mut messages = Vec::with_capacity(limit);
    for row in rows {
        let Ok(((indexed_ordinal, indexed_id, indexed_created_at), bytes)) = row else {
            continue;
        };
        let Ok(bytes) = bytes else {
            continue;
        };
        let Ok(indexed_ordinal) = u64::try_from(indexed_ordinal) else {
            continue;
        };
        let Ok(message) = serde_json::from_slice::<CanonicalMessage>(&bytes) else {
            continue;
        };
        if message.account_id != account_id
            || message.canonical_id.is_empty()
            || message.conversation_id != conversation_id
            || message.canonical_id != indexed_id
            || message.conversation_ordinal != indexed_ordinal
            || message.created_at_unix != indexed_created_at
        {
            continue;
        }
        messages.push(message);
        if messages.len() == limit {
            break;
        }
    }
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

pub fn replica_restoration_report(
    replica_path: &Path,
    key: &ReplicaKey,
) -> Result<Option<RestorationReport>, RestoreError> {
    let opened = open_replica(replica_path, key)?;
    let identity: Option<(String, Option<String>)> = opened
        .connection
        .query_row(
            "SELECT account_id, current_source_fingerprint
             FROM replica_identity WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((account_id, source_fingerprint)) = identity else {
        return Ok(None);
    };
    let report: Option<Vec<u8>> = opened
        .connection
        .query_row(
            "SELECT report_json FROM coverage_state WHERE account_id = ?1",
            [&account_id],
            |row| row.get(0),
        )
        .optional()?;
    report
        .map(|bytes| {
            let report: RestorationReport = serde_json::from_slice(&bytes)?;
            require_account(&report.account_id, &account_id)?;
            if source_fingerprint.as_deref() != Some(report.source_fingerprint.as_str()) {
                return Err(RestoreError::Integrity(
                    "replica restoration report belongs to another checkpoint".to_string(),
                ));
            }
            Ok(report)
        })
        .transpose()
}

pub fn replica_conversation_references_artifact(
    replica_path: &Path,
    key: &ReplicaKey,
    conversation_id: &str,
    artifact_id: &str,
) -> Result<bool, RestoreError> {
    replica_conversation_references_artifact_in_range(
        replica_path,
        key,
        conversation_id,
        artifact_id,
        None,
        None,
    )
}

pub fn replica_conversation_references_artifact_in_range(
    replica_path: &Path,
    key: &ReplicaKey,
    conversation_id: &str,
    artifact_id: &str,
    not_before_unix: Option<i64>,
    not_after_unix: Option<i64>,
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
             AND (?4 IS NULL OR m.created_at_unix >= ?4)
             AND (?5 IS NULL OR m.created_at_unix <= ?5)
         )",
        params![
            account_id,
            conversation_id,
            artifact_id,
            not_before_unix,
            not_after_unix
        ],
        |row| row.get(0),
    )?;
    Ok(exists)
}

trait ReplicaRecordIdentity {
    fn replica_account_id(&self) -> Option<&str>;
    fn replica_record_id(&self) -> &str;
}

impl ReplicaRecordIdentity for CanonicalConversation {
    fn replica_account_id(&self) -> Option<&str> {
        Some(&self.account_id)
    }

    fn replica_record_id(&self) -> &str {
        &self.conversation_id
    }
}

impl ReplicaRecordIdentity for CanonicalParticipant {
    fn replica_account_id(&self) -> Option<&str> {
        Some(&self.account_id)
    }

    fn replica_record_id(&self) -> &str {
        &self.participant_id
    }
}

impl ReplicaRecordIdentity for CanonicalArtifact {
    fn replica_account_id(&self) -> Option<&str> {
        // Artifact identities are account-scoped by the encrypted table key;
        // the canonical artifact payload predates an embedded account field.
        None
    }

    fn replica_record_id(&self) -> &str {
        &self.artifact_id
    }
}

fn get_replica_record<T: DeserializeOwned + ReplicaRecordIdentity>(
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
    let bytes: Option<Vec<u8>> = match opened
        .connection
        .query_row(&query, params![account_id, identifier], |row| row.get(0))
        .optional()
    {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let Ok(value) = serde_json::from_slice::<T>(&bytes) else {
        return Ok(None);
    };
    if value
        .replica_account_id()
        .is_some_and(|record_account| record_account != account_id)
        || value.replica_record_id() != identifier
    {
        return Ok(None);
    }
    Ok(Some(value))
}

pub fn list_replica_conversations(
    replica_path: &Path,
    key: &ReplicaKey,
    requested_limit: usize,
) -> Result<ReplicaConversationPage, RestoreError> {
    let opened = open_replica(replica_path, key)?;
    let (account_id, _) = current_replica_identity(&opened.connection)?;
    let limit = requested_limit.clamp(1, 1_000);
    let scan_limit = limit.saturating_mul(8).min(8_000);
    let query_limit = checked_usize_i64(scan_limit)?;
    let mut statement = match opened.connection.prepare(
        "SELECT conversation_id, record_json FROM conversation
         WHERE account_id = ?1 ORDER BY conversation_id LIMIT ?2",
    ) {
        Ok(statement) => statement,
        Err(_) => {
            return Ok(ReplicaConversationPage {
                account_id,
                items: Vec::new(),
                omitted_item_count: 0,
                limitation_codes: vec!["replicaConversationTableUnavailable".to_string()],
            });
        }
    };
    let rows = match statement.query_map(params![account_id, query_limit], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)))
    }) {
        Ok(rows) => rows,
        Err(_) => {
            return Ok(ReplicaConversationPage {
                account_id,
                items: Vec::new(),
                omitted_item_count: 0,
                limitation_codes: vec!["replicaConversationTableUnavailable".to_string()],
            });
        }
    };
    let mut items = Vec::with_capacity(limit);
    let mut omitted_item_count = 0_u64;
    let mut scanned_count = 0_usize;
    for row in rows {
        scanned_count = scanned_count.saturating_add(1);
        let (indexed_id, bytes) = match row {
            Ok(value) => value,
            Err(_) => {
                omitted_item_count = omitted_item_count.saturating_add(1);
                continue;
            }
        };
        let Ok(bytes) = bytes else {
            omitted_item_count = omitted_item_count.saturating_add(1);
            continue;
        };
        let Ok(conversation) = serde_json::from_slice::<CanonicalConversation>(&bytes) else {
            omitted_item_count = omitted_item_count.saturating_add(1);
            continue;
        };
        if conversation.account_id != account_id
            || conversation.conversation_id.is_empty()
            || conversation.conversation_id != indexed_id
        {
            omitted_item_count = omitted_item_count.saturating_add(1);
            continue;
        }
        items.push(conversation);
        if items.len() == limit {
            break;
        }
    }
    let scan_limit_reached = items.len() < limit && scanned_count == scan_limit;
    let mut limitation_codes = BTreeSet::new();
    if omitted_item_count > 0 {
        limitation_codes.insert("malformedReplicaConversationOmitted".to_string());
    }
    if scan_limit_reached {
        limitation_codes.insert("replicaConversationScanLimitReached".to_string());
    }
    Ok(ReplicaConversationPage {
        account_id,
        items,
        omitted_item_count,
        limitation_codes: limitation_codes.into_iter().collect(),
    })
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
    validate_restoration_coverage_schema(&coverage)?;
    if report.account_id != account_id || report.source_fingerprint != source_fingerprint {
        return Err(RestoreError::Integrity(
            "replica coverage belongs to another account or checkpoint".to_string(),
        ));
    }
    let mut limitation_codes = BTreeSet::new();
    let cached_surface_bytes: Option<Vec<u8>> = match opened
        .connection
        .query_row(
            "SELECT coverage_json FROM cached_surface_state WHERE account_id = ?1",
            [&account_id],
            |row| row.get(0),
        )
        .optional()
    {
        Ok(value) => value,
        Err(_) => {
            limitation_codes.insert("cachedMomentCoverageUnavailable".to_string());
            None
        }
    };
    let cached_surfaces = cached_surface_bytes.and_then(|bytes| {
        let Ok(coverage) = serde_json::from_slice::<crate::CachedSurfaceCoverage>(&bytes) else {
            limitation_codes.insert("malformedCachedMomentCoverageOmitted".to_string());
            return None;
        };
        if validate_cached_coverage_schema(&coverage).is_err() {
            limitation_codes.insert("malformedCachedMomentCoverageOmitted".to_string());
            return None;
        }
        Some(coverage)
    });
    Ok(ReplicaCoverageView {
        account_id,
        self_participant_id: report.self_participant_id.clone(),
        source_fingerprint,
        archive_scope: report.archive_scope,
        database_coverage: report.database_coverage.clone(),
        coverage,
        integrity: report.integrity,
        completion: report.completion,
        cached_surfaces,
        limitation_codes: limitation_codes.into_iter().collect(),
    })
}

#[derive(Default)]
struct ReplicaRecordAudit {
    conversation_count: u64,
    participant_count: u64,
    message_count: u64,
    artifact_count: u64,
    cached_moment_count: u64,
    cached_moment_interaction_count: u64,
    memberships: BTreeSet<Vec<u8>>,
    relationships: BTreeSet<Vec<u8>>,
    message_artifacts: BTreeSet<Vec<u8>>,
}

impl ReplicaRecordAudit {
    fn canonical_record_count(&self) -> u64 {
        self.conversation_count
            .saturating_add(self.participant_count)
            .saturating_add(self.message_count)
            .saturating_add(self.artifact_count)
            .saturating_add(self.cached_moment_count)
            .saturating_add(self.cached_moment_interaction_count)
    }
}

const REPLICA_AUDIT_STAGE_COUNT: usize = 8;
const REPLICA_AUDIT_STAGE_RESOLUTION: u64 = 1_000_000;
const REPLICA_AUDIT_ROW_EVENT_INTERVAL: u64 = 1_000;
const REPLICA_AUDIT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const REPLICA_AUDIT_RECORD_STAGE: usize = 2;
const REPLICA_AUDIT_LINK_STAGE: usize = 3;
const REPLICA_AUDIT_CHANGE_STAGE: usize = 6;

#[derive(Debug, Clone, Copy, Default)]
struct ReplicaAuditTotals {
    conversation_count: u64,
    message_count: u64,
    canonical_record_count: u64,
    link_record_count: u64,
    change_record_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplicaAuditCounter {
    None,
    CanonicalRecords,
    Links,
    Changes,
}

struct ReplicaAuditProgress<'a> {
    observer: &'a dyn ProgressObserver,
    replica_byte_count: u64,
    totals: Option<ReplicaAuditTotals>,
    started_at: Instant,
}

impl<'a> ReplicaAuditProgress<'a> {
    fn new(observer: &'a dyn ProgressObserver, replica_byte_count: u64) -> Self {
        Self {
            observer,
            replica_byte_count,
            totals: None,
            started_at: Instant::now(),
        }
    }

    fn set_totals(&mut self, totals: ReplicaAuditTotals) {
        self.totals = Some(totals);
    }

    #[allow(clippy::too_many_arguments)]
    fn emit(
        &self,
        stage: usize,
        state: ProgressState,
        operation: &str,
        unit: ProgressUnit,
        completed: u64,
        total: u64,
        counter: ReplicaAuditCounter,
        table_name: Option<&str>,
    ) {
        debug_assert!(stage < REPLICA_AUDIT_STAGE_COUNT);
        let within_stage = if total > 0 {
            (completed.min(total) as u128 * REPLICA_AUDIT_STAGE_RESOLUTION as u128 / total as u128)
                as u64
        } else if state == ProgressState::Completed {
            REPLICA_AUDIT_STAGE_RESOLUTION
        } else {
            0
        };
        let phase_completed = (stage as u64)
            .saturating_mul(REPLICA_AUDIT_STAGE_RESOLUTION)
            .saturating_add(within_stage);
        let phase_total =
            (REPLICA_AUDIT_STAGE_COUNT as u64).saturating_mul(REPLICA_AUDIT_STAGE_RESOLUTION);
        let mut event = ProgressEvent::new(
            ProgressPhase::ReplicaAudit,
            state,
            operation,
            unit,
            completed,
            total,
            phase_completed,
            phase_total,
        );
        event.stage_index = Some(stage + 1);
        event.stage_count = Some(REPLICA_AUDIT_STAGE_COUNT);
        event.replica_file_byte_count = Some(self.replica_byte_count);
        event.table_name = table_name.map(str::to_string);
        event.elapsed_milliseconds = Some(self.started_at.elapsed().as_millis() as u64);
        if let Some(totals) = self.totals {
            event.conversation_record_count = Some(totals.conversation_count);
            event.message_record_count = Some(totals.message_count);
            event.canonical_record_count = Some(totals.canonical_record_count);
            event.link_record_count = Some(totals.link_record_count);
            event.change_record_count = Some(totals.change_record_count);
            event.verified_record_count = Some(match stage.cmp(&REPLICA_AUDIT_RECORD_STAGE) {
                std::cmp::Ordering::Less => 0,
                std::cmp::Ordering::Equal if counter == ReplicaAuditCounter::CanonicalRecords => {
                    completed.min(totals.canonical_record_count)
                }
                _ => totals.canonical_record_count,
            });
            event.verified_link_count = Some(match stage.cmp(&REPLICA_AUDIT_LINK_STAGE) {
                std::cmp::Ordering::Less => 0,
                std::cmp::Ordering::Equal if counter == ReplicaAuditCounter::Links => {
                    completed.min(totals.link_record_count)
                }
                _ => totals.link_record_count,
            });
            event.verified_change_count = Some(match stage.cmp(&REPLICA_AUDIT_CHANGE_STAGE) {
                std::cmp::Ordering::Less => 0,
                std::cmp::Ordering::Equal if counter == ReplicaAuditCounter::Changes => {
                    completed.min(totals.change_record_count)
                }
                _ => totals.change_record_count,
            });
        }
        self.observer.observe(event);
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_rows(
        &self,
        stage: usize,
        state: ProgressState,
        operation: &str,
        completed: u64,
        total: u64,
        counter: ReplicaAuditCounter,
        table_name: Option<&str>,
    ) {
        self.emit(
            stage,
            state,
            operation,
            ProgressUnit::Records,
            completed,
            total,
            counter,
            table_name,
        );
    }
}

fn replica_audit_row_event_due(completed: u64, total: u64) -> bool {
    completed == total || completed.is_multiple_of(REPLICA_AUDIT_ROW_EVENT_INTERVAL)
}

fn with_replica_audit_heartbeat<T>(
    progress: &ReplicaAuditProgress<'_>,
    stage: usize,
    operation: &str,
    unit: ProgressUnit,
    total: u64,
    body: impl FnOnce() -> Result<T, RestoreError>,
) -> Result<T, RestoreError> {
    progress.emit(
        stage,
        ProgressState::Started,
        operation,
        unit,
        0,
        total,
        ReplicaAuditCounter::None,
        None,
    );
    let result = std::thread::scope(|scope| {
        let (stop_sender, stop_receiver) = std::sync::mpsc::channel::<()>();
        scope.spawn(move || loop {
            match stop_receiver.recv_timeout(REPLICA_AUDIT_HEARTBEAT_INTERVAL) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => progress.emit(
                    stage,
                    ProgressState::Advanced,
                    operation,
                    unit,
                    0,
                    total,
                    ReplicaAuditCounter::None,
                    None,
                ),
            }
        });
        let result = body();
        let _ = stop_sender.send(());
        result
    });
    if result.is_ok() {
        progress.emit(
            stage,
            ProgressState::Completed,
            operation,
            unit,
            total,
            total,
            ReplicaAuditCounter::None,
            None,
        );
    }
    result
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReplicaStorageEntrySeal {
    present: bool,
    device: u64,
    inode: u64,
    byte_count: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    sha256: Option<[u8; 32]>,
}

fn verify_private_replica_files(path: &Path) -> Result<(), RestoreError> {
    ensure_private_regular_file(path)?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| RestoreError::UnsafePath("replica filename is invalid".to_string()))?;
    let parent = path
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath("replica has no parent".to_string()))?;
    ensure_private_directory(parent)?;
    for sidecar in [
        format!("{name}-wal"),
        format!("{name}-shm"),
        format!("{name}-journal"),
    ] {
        let sidecar = parent.join(sidecar);
        match fs::symlink_metadata(&sidecar) {
            Ok(_) => ensure_private_regular_file(&sidecar)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn seal_replica_storage(path: &Path) -> Result<Vec<ReplicaStorageEntrySeal>, RestoreError> {
    verify_private_replica_files(path)?;
    let mut seals = Vec::new();
    for candidate in sqlite_file_namespace(path) {
        match fs::symlink_metadata(&candidate) {
            Ok(_) => {
                ensure_private_regular_file(&candidate)?;
                let mut file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
                    .open(&candidate)?;
                let before = file.metadata()?;
                let mut digest = Sha256::new();
                let mut buffer = [0_u8; 64 * 1024];
                loop {
                    let count = file.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    digest.update(&buffer[..count]);
                }
                let after = file.metadata()?;
                if before.dev() != after.dev()
                    || before.ino() != after.ino()
                    || before.len() != after.len()
                    || before.mtime() != after.mtime()
                    || before.mtime_nsec() != after.mtime_nsec()
                {
                    return Err(RestoreError::Integrity(
                        "replica storage changed while it was sealed".to_string(),
                    ));
                }
                seals.push(ReplicaStorageEntrySeal {
                    present: true,
                    device: before.dev(),
                    inode: before.ino(),
                    byte_count: before.len(),
                    modified_seconds: before.mtime(),
                    modified_nanoseconds: before.mtime_nsec(),
                    sha256: Some(digest.finalize().into()),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                seals.push(ReplicaStorageEntrySeal {
                    present: false,
                    device: 0,
                    inode: 0,
                    byte_count: 0,
                    modified_seconds: 0,
                    modified_nanoseconds: 0,
                    sha256: None,
                });
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(seals)
}

fn open_existing_replica_read_only(
    path: &Path,
    key: &ReplicaKey,
) -> Result<(Connection, String), RestoreError> {
    ensure_private_regular_file(path)?;
    let canonical = fs::canonicalize(path)?;
    let connection = Connection::open_with_flags(
        canonical,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    configure_keyed_connection(&connection, key, false)?;
    let cipher_version =
        connection.pragma_query_value(None, "cipher_version", |row| row.get::<_, String>(0))?;
    if cipher_version.is_empty() {
        return Err(RestoreError::Integrity(
            "replica SQLite build does not provide SQLCipher".to_string(),
        ));
    }
    Ok((connection, cipher_version))
}

fn verify_sqlite_integrity(connection: &Connection) -> Result<(), RestoreError> {
    let mut statement = connection.prepare("PRAGMA integrity_check")?;
    let results = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if results.as_slice() != ["ok"] {
        return Err(RestoreError::Integrity(
            "encrypted replica failed SQLite integrity verification".to_string(),
        ));
    }
    Ok(())
}

fn verify_foreign_keys(connection: &Connection) -> Result<(), RestoreError> {
    let mut statement = connection.prepare("PRAGMA foreign_key_check")?;
    if statement.query([])?.next()?.is_some() {
        return Err(RestoreError::Integrity(
            "encrypted replica failed foreign-key verification".to_string(),
        ));
    }
    Ok(())
}

fn verify_positive_timestamp(value: &str, label: &str) -> Result<u128, RestoreError> {
    value
        .parse::<u128>()
        .ok()
        .filter(|timestamp| *timestamp > 0)
        .ok_or_else(|| RestoreError::Integrity(format!("{label} timestamp is invalid")))
}

fn verify_stored_record<T: DeserializeOwned + Serialize>(
    bytes: &[u8],
    digest: &str,
) -> Result<T, RestoreError> {
    if digest.len() != 64
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || sha256(bytes) != digest
    {
        return Err(RestoreError::Integrity(
            "replica canonical record digest is invalid".to_string(),
        ));
    }
    let value: T = serde_json::from_slice(bytes)?;
    if serde_json::to_vec(&value)? != bytes {
        return Err(RestoreError::Integrity(
            "replica canonical record encoding is not stable".to_string(),
        ));
    }
    Ok(value)
}

fn audit_replica_records(
    connection: &Connection,
    account_id: &str,
    include_cached: bool,
    progress: &ReplicaAuditProgress<'_>,
) -> Result<ReplicaRecordAudit, RestoreError> {
    let mut audit = ReplicaRecordAudit::default();
    let total = progress
        .totals
        .map_or(0, |totals| totals.canonical_record_count);
    progress.emit_rows(
        REPLICA_AUDIT_RECORD_STAGE,
        ProgressState::Started,
        "verifyReplicaCanonicalRecords",
        0,
        total,
        ReplicaAuditCounter::CanonicalRecords,
        Some("conversation"),
    );
    {
        let mut statement = connection.prepare(
            "SELECT account_id, conversation_id, kind, entity_decode_state,
                    participant_count, record_sha256, record_json
             FROM conversation ORDER BY conversation_id",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let stored_account: String = row.get(0)?;
            let stored_id: String = row.get(1)?;
            let kind: String = row.get(2)?;
            let decode_state: String = row.get(3)?;
            let participant_count: i64 = row.get(4)?;
            let digest: String = row.get(5)?;
            let bytes: Vec<u8> = row.get(6)?;
            let value: CanonicalConversation = verify_stored_record(&bytes, &digest)?;
            if stored_account != account_id
                || value.account_id != account_id
                || stored_id != value.conversation_id
                || kind != json_enum(&value.kind)?
                || decode_state != json_enum(&value.entity_decode_state)?
                || u64::try_from(participant_count).ok() != Some(value.participant_ids.len() as u64)
            {
                return Err(RestoreError::Integrity(
                    "replica conversation projection differs from its canonical record".to_string(),
                ));
            }
            for membership in value.memberships {
                audit.memberships.insert(serde_json::to_vec(&(
                    account_id,
                    &stored_id,
                    membership.participant_id,
                    json_enum(&membership.role)?,
                    membership.display_name_base64,
                ))?);
            }
            audit.conversation_count = audit.conversation_count.saturating_add(1);
            let completed = audit.canonical_record_count();
            if replica_audit_row_event_due(completed, total) {
                progress.emit_rows(
                    REPLICA_AUDIT_RECORD_STAGE,
                    ProgressState::Advanced,
                    "verifyReplicaCanonicalRecords",
                    completed,
                    total,
                    ReplicaAuditCounter::CanonicalRecords,
                    Some("conversation"),
                );
            }
        }
    }
    {
        let mut statement = connection.prepare(
            "SELECT account_id, participant_id, local_profile_state,
                    record_sha256, record_json
             FROM participant ORDER BY participant_id",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let stored_account: String = row.get(0)?;
            let stored_id: String = row.get(1)?;
            let state: String = row.get(2)?;
            let digest: String = row.get(3)?;
            let bytes: Vec<u8> = row.get(4)?;
            let value: CanonicalParticipant = verify_stored_record(&bytes, &digest)?;
            if stored_account != account_id
                || value.account_id != account_id
                || stored_id != value.participant_id
                || state != json_enum(&value.local_profile_state)?
            {
                return Err(RestoreError::Integrity(
                    "replica participant projection differs from its canonical record".to_string(),
                ));
            }
            audit.participant_count = audit.participant_count.saturating_add(1);
            let completed = audit.canonical_record_count();
            if replica_audit_row_event_due(completed, total) {
                progress.emit_rows(
                    REPLICA_AUDIT_RECORD_STAGE,
                    ProgressState::Advanced,
                    "verifyReplicaCanonicalRecords",
                    completed,
                    total,
                    ReplicaAuditCounter::CanonicalRecords,
                    Some("participant"),
                );
            }
        }
    }
    {
        let mut statement = connection.prepare(
            "SELECT account_id, artifact_id, kind, role, availability,
                    source_sha256, decoded_sha256, record_sha256, record_json
             FROM artifact ORDER BY artifact_id",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let stored_account: String = row.get(0)?;
            let stored_id: String = row.get(1)?;
            let kind: String = row.get(2)?;
            let role: String = row.get(3)?;
            let availability: String = row.get(4)?;
            let source_sha: Option<String> = row.get(5)?;
            let decoded_sha: Option<String> = row.get(6)?;
            let digest: String = row.get(7)?;
            let bytes: Vec<u8> = row.get(8)?;
            let value: CanonicalArtifact = verify_stored_record(&bytes, &digest)?;
            if stored_account != account_id
                || stored_id != value.artifact_id
                || kind != json_enum(&value.kind)?
                || role != json_enum(&value.role)?
                || availability != json_enum(&value.availability)?
                || source_sha != value.source_sha256
                || decoded_sha != value.decoded_sha256
            {
                return Err(RestoreError::Integrity(
                    "replica artifact projection differs from its canonical record".to_string(),
                ));
            }
            audit.artifact_count = audit.artifact_count.saturating_add(1);
            let completed = audit.canonical_record_count();
            if replica_audit_row_event_due(completed, total) {
                progress.emit_rows(
                    REPLICA_AUDIT_RECORD_STAGE,
                    ProgressState::Advanced,
                    "verifyReplicaCanonicalRecords",
                    completed,
                    total,
                    ReplicaAuditCounter::CanonicalRecords,
                    Some("artifact"),
                );
            }
        }
    }
    {
        let mut statement = connection.prepare(
            "SELECT account_id, canonical_id, conversation_id, sender_id,
                    conversation_ordinal, created_at_unix, direction, logical_type,
                    sub_type, semantic_decode_state, search_text, record_sha256, record_json
             FROM message ORDER BY canonical_id",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let stored_account: String = row.get(0)?;
            let stored_id: String = row.get(1)?;
            let conversation_id: String = row.get(2)?;
            let sender_id: Option<String> = row.get(3)?;
            let ordinal: i64 = row.get(4)?;
            let created_at: Option<i64> = row.get(5)?;
            let direction: String = row.get(6)?;
            let logical_type: Option<i64> = row.get(7)?;
            let sub_type: Option<i64> = row.get(8)?;
            let decode_state: String = row.get(9)?;
            let search_text: String = row.get(10)?;
            let digest: String = row.get(11)?;
            let bytes: Vec<u8> = row.get(12)?;
            let value: CanonicalMessage = verify_stored_record(&bytes, &digest)?;
            if stored_account != account_id
                || value.account_id != account_id
                || stored_id != value.canonical_id
                || conversation_id != value.conversation_id
                || sender_id != value.sender_id
                || u64::try_from(ordinal).ok() != Some(value.conversation_ordinal)
                || created_at != value.created_at_unix
                || direction != json_enum(&value.direction)?
                || logical_type != value.logical_type.map(i64::from)
                || sub_type != value.sub_type.map(i64::from)
                || decode_state != json_enum(&value.semantic_decode_state)?
                || search_text != message_search_text(&value)
            {
                return Err(RestoreError::Integrity(
                    "replica message projection differs from its canonical record".to_string(),
                ));
            }
            for (index, relationship) in value.relationships.iter().enumerate() {
                audit.relationships.insert(serde_json::to_vec(&(
                    account_id,
                    &stored_id,
                    index as u64,
                    json_enum(&relationship.kind)?,
                    &relationship.target_canonical_id,
                    relationship.resolved,
                    serde_json::to_vec(relationship)?,
                ))?);
            }
            for (index, reference) in value.artifact_references.iter().enumerate() {
                audit.message_artifacts.insert(serde_json::to_vec(&(
                    account_id,
                    &stored_id,
                    index as u64,
                    &reference.artifact_id,
                    json_enum(&reference.role)?,
                    reference.preferred,
                ))?);
            }
            audit.message_count = audit.message_count.saturating_add(1);
            let completed = audit.canonical_record_count();
            if replica_audit_row_event_due(completed, total) {
                progress.emit_rows(
                    REPLICA_AUDIT_RECORD_STAGE,
                    ProgressState::Advanced,
                    "verifyReplicaCanonicalRecords",
                    completed,
                    total,
                    ReplicaAuditCounter::CanonicalRecords,
                    Some("message"),
                );
            }
        }
    }
    if include_cached {
        audit_cached_records(connection, account_id, &mut audit, progress, total)?;
    }
    progress.emit_rows(
        REPLICA_AUDIT_RECORD_STAGE,
        ProgressState::Completed,
        "verifyReplicaCanonicalRecords",
        total,
        total,
        ReplicaAuditCounter::CanonicalRecords,
        None,
    );
    Ok(audit)
}

fn audit_cached_records(
    connection: &Connection,
    account_id: &str,
    audit: &mut ReplicaRecordAudit,
    progress: &ReplicaAuditProgress<'_>,
    total: u64,
) -> Result<(), RestoreError> {
    let mut statement = connection.prepare(
        "SELECT account_id, canonical_id, author_id, created_at_unix, content_type,
                record_sha256, record_json
         FROM cached_moment ORDER BY canonical_id",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let stored_account: String = row.get(0)?;
        let stored_id: String = row.get(1)?;
        let author: Option<String> = row.get(2)?;
        let created: Option<i64> = row.get(3)?;
        let content_type: Option<i64> = row.get(4)?;
        let digest: String = row.get(5)?;
        let bytes: Vec<u8> = row.get(6)?;
        let value: crate::CanonicalCachedMoment = verify_stored_record(&bytes, &digest)?;
        if stored_account != account_id
            || value.account_id != account_id
            || stored_id != value.canonical_id
            || author != value.author_id
            || created != value.created_at_unix
            || content_type != value.content_type
        {
            return Err(RestoreError::Integrity(
                "replica cached moment projection differs from its canonical record".to_string(),
            ));
        }
        audit.cached_moment_count = audit.cached_moment_count.saturating_add(1);
        let completed = audit.canonical_record_count();
        if replica_audit_row_event_due(completed, total) {
            progress.emit_rows(
                REPLICA_AUDIT_RECORD_STAGE,
                ProgressState::Advanced,
                "verifyReplicaCanonicalRecords",
                completed,
                total,
                ReplicaAuditCounter::CanonicalRecords,
                Some("cached_moment"),
            );
        }
    }
    drop(rows);
    drop(statement);
    let mut statement = connection.prepare(
        "SELECT account_id, canonical_id, created_at_unix, interaction_kind,
                from_participant_id, to_participant_id, record_sha256, record_json
         FROM cached_moment_interaction ORDER BY canonical_id",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let stored_account: String = row.get(0)?;
        let stored_id: String = row.get(1)?;
        let created: Option<i64> = row.get(2)?;
        let kind: String = row.get(3)?;
        let from: Option<String> = row.get(4)?;
        let to: Option<String> = row.get(5)?;
        let digest: String = row.get(6)?;
        let bytes: Vec<u8> = row.get(7)?;
        let value: crate::CanonicalCachedMomentInteraction = verify_stored_record(&bytes, &digest)?;
        if stored_account != account_id
            || value.account_id != account_id
            || stored_id != value.canonical_id
            || created != value.created_at_unix
            || kind != json_enum(&value.kind)?
            || from != value.from_participant_id
            || to != value.to_participant_id
        {
            return Err(RestoreError::Integrity(
                "replica cached interaction projection differs from its canonical record"
                    .to_string(),
            ));
        }
        audit.cached_moment_interaction_count =
            audit.cached_moment_interaction_count.saturating_add(1);
        let completed = audit.canonical_record_count();
        if replica_audit_row_event_due(completed, total) {
            progress.emit_rows(
                REPLICA_AUDIT_RECORD_STAGE,
                ProgressState::Advanced,
                "verifyReplicaCanonicalRecords",
                completed,
                total,
                ReplicaAuditCounter::CanonicalRecords,
                Some("cached_moment_interaction"),
            );
        }
    }
    Ok(())
}

fn verify_replica_message_links(
    connection: &Connection,
    account_id: &str,
    audit: &ReplicaRecordAudit,
    progress: &ReplicaAuditProgress<'_>,
) -> Result<(), RestoreError> {
    let total = progress.totals.map_or(0, |totals| totals.link_record_count);
    let mut completed = 0_u64;
    progress.emit_rows(
        REPLICA_AUDIT_LINK_STAGE,
        ProgressState::Started,
        "verifyReplicaMessageLinks",
        completed,
        total,
        ReplicaAuditCounter::Links,
        Some("conversation_participant"),
    );
    let mut memberships = BTreeSet::new();
    let mut statement = connection.prepare(
        "SELECT account_id, conversation_id, participant_id, membership_role,
                display_name_base64
         FROM conversation_participant
         ORDER BY conversation_id, participant_id, membership_role",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let account: String = row.get(0)?;
        if account != account_id {
            return Err(RestoreError::Integrity(
                "replica membership crossed the account boundary".to_string(),
            ));
        }
        memberships.insert(serde_json::to_vec(&(
            account,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))?);
        completed = completed.saturating_add(1);
        if replica_audit_row_event_due(completed, total) {
            progress.emit_rows(
                REPLICA_AUDIT_LINK_STAGE,
                ProgressState::Advanced,
                "verifyReplicaMessageLinks",
                completed,
                total,
                ReplicaAuditCounter::Links,
                Some("conversation_participant"),
            );
        }
    }
    if memberships != audit.memberships {
        return Err(RestoreError::Integrity(
            "replica membership projection differs from canonical conversations".to_string(),
        ));
    }
    drop(rows);
    drop(statement);

    let mut relationships = BTreeSet::new();
    let mut statement = connection.prepare(
        "SELECT account_id, source_canonical_id, relationship_ordinal, kind,
                target_canonical_id, resolved, record_json
         FROM message_relationship
         ORDER BY source_canonical_id, relationship_ordinal",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let account: String = row.get(0)?;
        let ordinal: i64 = row.get(2)?;
        if account != account_id || ordinal < 0 {
            return Err(RestoreError::Integrity(
                "replica relationship identity is invalid".to_string(),
            ));
        }
        relationships.insert(serde_json::to_vec(&(
            account,
            row.get::<_, String>(1)?,
            ordinal as u64,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, bool>(5)?,
            row.get::<_, Vec<u8>>(6)?,
        ))?);
        completed = completed.saturating_add(1);
        if replica_audit_row_event_due(completed, total) {
            progress.emit_rows(
                REPLICA_AUDIT_LINK_STAGE,
                ProgressState::Advanced,
                "verifyReplicaMessageLinks",
                completed,
                total,
                ReplicaAuditCounter::Links,
                Some("message_relationship"),
            );
        }
    }
    if relationships != audit.relationships {
        return Err(RestoreError::Integrity(
            "replica relationship projection differs from canonical messages".to_string(),
        ));
    }
    drop(rows);
    drop(statement);

    let mut artifacts = BTreeSet::new();
    let mut statement = connection.prepare(
        "SELECT account_id, canonical_id, artifact_ordinal, artifact_id, role, preferred
         FROM message_artifact ORDER BY canonical_id, artifact_ordinal",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let account: String = row.get(0)?;
        let ordinal: i64 = row.get(2)?;
        if account != account_id || ordinal < 0 {
            return Err(RestoreError::Integrity(
                "replica message-artifact identity is invalid".to_string(),
            ));
        }
        artifacts.insert(serde_json::to_vec(&(
            account,
            row.get::<_, String>(1)?,
            ordinal as u64,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, bool>(5)?,
        ))?);
        completed = completed.saturating_add(1);
        if replica_audit_row_event_due(completed, total) {
            progress.emit_rows(
                REPLICA_AUDIT_LINK_STAGE,
                ProgressState::Advanced,
                "verifyReplicaMessageLinks",
                completed,
                total,
                ReplicaAuditCounter::Links,
                Some("message_artifact"),
            );
        }
    }
    if artifacts != audit.message_artifacts {
        return Err(RestoreError::Integrity(
            "replica message-artifact projection differs from canonical messages".to_string(),
        ));
    }
    progress.emit_rows(
        REPLICA_AUDIT_LINK_STAGE,
        ProgressState::Completed,
        "verifyReplicaMessageLinks",
        total,
        total,
        ReplicaAuditCounter::Links,
        None,
    );
    Ok(())
}

fn verify_replica_fts(
    connection: &Connection,
    account_id: &str,
    message_count: u64,
) -> Result<(), RestoreError> {
    if table_count(connection, "message_fts")? != message_count {
        return Err(RestoreError::Integrity(
            "replica full-text row count differs from canonical messages".to_string(),
        ));
    }
    let missing: i64 = connection.query_row(
        "SELECT count(*) FROM message m
         WHERE m.account_id = ?1 AND NOT EXISTS (
           SELECT 1 FROM message_fts f
           WHERE f.account_id = m.account_id
             AND f.canonical_id = m.canonical_id
             AND f.conversation_id = m.conversation_id
             AND f.search_text = m.search_text
         )",
        [account_id],
        |row| row.get(0),
    )?;
    let extra: i64 = connection.query_row(
        "SELECT count(*) FROM message_fts f
         WHERE f.account_id != ?1 OR NOT EXISTS (
           SELECT 1 FROM message m
           WHERE m.account_id = f.account_id
             AND m.canonical_id = f.canonical_id
             AND m.conversation_id = f.conversation_id
             AND m.search_text = f.search_text
         )",
        [account_id],
        |row| row.get(0),
    )?;
    let duplicate: Option<i64> = connection
        .query_row(
            "SELECT count(*) FROM message_fts
             GROUP BY account_id, canonical_id HAVING count(*) != 1 LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if missing != 0 || extra != 0 || duplicate.is_some() {
        return Err(RestoreError::Integrity(
            "replica full-text projection differs from canonical messages".to_string(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify_replica_checkpoint_and_coverage(
    connection: &Connection,
    identity: &(String, String, bool, String, String),
    audit: &ReplicaRecordAudit,
    conversation_count: u64,
    participant_count: u64,
    message_count: u64,
    artifact_count: u64,
    cached_moment_count: u64,
    cached_moment_interaction_count: u64,
) -> Result<(), RestoreError> {
    let checkpoint: (String, String, i64, i64, i64, i64) = connection.query_row(
        "SELECT source_fingerprint, committed_at_unix_nanoseconds,
                conversation_count, participant_count, message_count, artifact_count
         FROM source_checkpoint WHERE account_id = ?1",
        [&identity.0],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;
    let checkpoint_timestamp = verify_positive_timestamp(&checkpoint.1, "replica checkpoint")?;
    if table_count(connection, "source_checkpoint")? != 1
        || checkpoint.0 != identity.1
        || u64::try_from(checkpoint.2).ok() != Some(conversation_count)
        || u64::try_from(checkpoint.3).ok() != Some(participant_count)
        || u64::try_from(checkpoint.4).ok() != Some(message_count)
        || u64::try_from(checkpoint.5).ok() != Some(artifact_count)
    {
        return Err(RestoreError::Integrity(
            "replica checkpoint differs from committed canonical state".to_string(),
        ));
    }
    let mut matching_checkpoint_run = false;
    let mut statement = connection
        .prepare("SELECT source_fingerprint, committed_at_unix_nanoseconds FROM sync_run")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let source: String = row.get(0)?;
        let committed =
            verify_positive_timestamp(&row.get::<_, String>(1)?, "replica synchronization commit")?;
        if committed > checkpoint_timestamp {
            return Err(RestoreError::Integrity(
                "replica synchronization history leads its checkpoint".to_string(),
            ));
        }
        if committed == checkpoint_timestamp && source == identity.1 {
            matching_checkpoint_run = true;
        }
    }
    if !matching_checkpoint_run {
        return Err(RestoreError::Integrity(
            "replica checkpoint does not match the latest synchronization run".to_string(),
        ));
    }
    let (coverage_source, coverage_bytes, report_bytes, stored_complete): (
        String,
        Vec<u8>,
        Vec<u8>,
        bool,
    ) = connection.query_row(
        "SELECT source_fingerprint, coverage_json, report_json,
                full_restoration_achieved
         FROM coverage_state WHERE account_id = ?1",
        [&identity.0],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    if table_count(connection, "coverage_state")? != 1 {
        return Err(RestoreError::Integrity(
            "replica coverage state is not singular".to_string(),
        ));
    }
    let coverage: RestorationCoverage = serde_json::from_slice(&coverage_bytes)?;
    let report: RestorationReport = serde_json::from_slice(&report_bytes)?;
    let stored_self_participant_id: Option<String> = connection.query_row(
        "SELECT self_participant_id FROM replica_identity WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    let account_binding_valid = report
        .self_participant_id
        .as_deref()
        .is_none_or(is_lower_sha256)
        && (report.self_participant_id.is_some() == report.account_binding_evidence.is_some())
        && if report.format_version >= 6 {
            report.self_participant_id.is_some()
        } else if report.format_version >= 3 {
            report.self_participant_id.is_none()
        } else {
            true
        };
    validate_restoration_coverage_schema(&coverage)?;
    let recomputed_completion = crate::RestorationCompletion::evaluate_report(&report);
    if coverage_source != identity.1
        || report.account_id != identity.0
        || report.self_participant_id != stored_self_participant_id
        || !account_binding_valid
        || report.source_fingerprint != identity.1
        || require_serving_archive(&report).is_err()
        || report.completion.full_restoration_achieved != identity.2
        || stored_complete != identity.2
        || serde_json::to_vec(&recomputed_completion)? != serde_json::to_vec(&report.completion)?
        || !report.integrity.row_equation_holds()
        || report.integrity.restored_row_count != message_count
        || report.integrity.conversation_count != conversation_count
        || report.integrity.participant_count != participant_count
        || report.integrity.unique_artifact_count != artifact_count
        || (report.format_version >= 3
            && (report.integrity.relationship_reference_count != audit.relationships.len() as u64
                || report.integrity.artifact_reference_count
                    != audit.message_artifacts.len() as u64))
        || report.integrity.cached_moment_count != cached_moment_count
        || report.integrity.cached_moment_interaction_count != cached_moment_interaction_count
        || audit.conversation_count != conversation_count
        || audit.participant_count != participant_count
        || audit.message_count != message_count
        || audit.artifact_count != artifact_count
    {
        return Err(RestoreError::Integrity(
            "replica coverage state differs from committed canonical state".to_string(),
        ));
    }
    let cached_state_count = table_count(connection, "cached_surface_state")?;
    if cached_state_count > 1 {
        return Err(RestoreError::Integrity(
            "replica cached coverage state is not singular".to_string(),
        ));
    }
    if cached_state_count == 1 {
        let (account, observed_at, bytes): (String, String, Vec<u8>) = connection.query_row(
            "SELECT account_id, observed_at, coverage_json FROM cached_surface_state",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let cached: crate::CachedSurfaceCoverage = serde_json::from_slice(&bytes)?;
        validate_cached_coverage_schema(&cached)?;
        if account != identity.0
            || observed_at != cached.observed_at
            || cached.moment_count != cached_moment_count
            || cached.interaction_count != cached_moment_interaction_count
        {
            return Err(RestoreError::Integrity(
                "replica cached coverage projection is inconsistent".to_string(),
            ));
        }
    } else if cached_moment_count != 0 || cached_moment_interaction_count != 0 {
        return Err(RestoreError::Integrity(
            "replica cached records have no coverage state".to_string(),
        ));
    }
    Ok(())
}

fn verify_legacy_replica_coverage(
    connection: &Connection,
    identity: &(String, String, bool, String, String),
    audit: &ReplicaRecordAudit,
    include_cached: bool,
) -> Result<(), RestoreError> {
    if table_count(connection, "coverage_state")? != 1 {
        return Err(RestoreError::Integrity(
            "replica backup coverage state is not singular".to_string(),
        ));
    }
    let (source, coverage_bytes, report_bytes, stored_complete): (String, Vec<u8>, Vec<u8>, bool) =
        connection.query_row(
            "SELECT source_fingerprint, coverage_json, report_json,
                full_restoration_achieved
         FROM coverage_state WHERE account_id = ?1",
            [&identity.0],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    let coverage: RestorationCoverage = serde_json::from_slice(&coverage_bytes)?;
    let report: RestorationReport = serde_json::from_slice(&report_bytes)?;
    validate_restoration_coverage_schema(&coverage)?;
    let recomputed_completion = crate::RestorationCompletion::evaluate_report(&report);
    if source != identity.1
        || report.account_id != identity.0
        || report.source_fingerprint != identity.1
        || report.archive_scope != crate::RestorationArchiveScope::Authoritative
        || report.completion.full_restoration_achieved != identity.2
        || stored_complete != identity.2
        || serde_json::to_vec(&recomputed_completion)? != serde_json::to_vec(&report.completion)?
        || report.integrity.restored_row_count != audit.message_count
        || report.integrity.conversation_count != audit.conversation_count
        || report.integrity.participant_count != audit.participant_count
        || report.integrity.unique_artifact_count != audit.artifact_count
        || report.integrity.cached_moment_count != audit.cached_moment_count
        || report.integrity.cached_moment_interaction_count != audit.cached_moment_interaction_count
        || (report.format_version >= 3
            && (report.integrity.relationship_reference_count != audit.relationships.len() as u64
                || report.integrity.artifact_reference_count
                    != audit.message_artifacts.len() as u64))
        || !report.integrity.row_equation_holds()
    {
        return Err(RestoreError::Integrity(
            "replica backup coverage differs from its canonical state".to_string(),
        ));
    }
    if include_cached {
        let cached_state_count = table_count(connection, "cached_surface_state")?;
        if cached_state_count > 1 {
            return Err(RestoreError::Integrity(
                "replica backup cached coverage state is not singular".to_string(),
            ));
        }
        if cached_state_count == 1 {
            let (account, observed_at, bytes): (String, String, Vec<u8>) = connection.query_row(
                "SELECT account_id, observed_at, coverage_json FROM cached_surface_state",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            let cached: crate::CachedSurfaceCoverage = serde_json::from_slice(&bytes)?;
            validate_cached_coverage_schema(&cached)?;
            if account != identity.0
                || observed_at != cached.observed_at
                || cached.moment_count != audit.cached_moment_count
                || cached.interaction_count != audit.cached_moment_interaction_count
            {
                return Err(RestoreError::Integrity(
                    "replica backup cached coverage differs from canonical state".to_string(),
                ));
            }
        } else if audit.cached_moment_count != 0 || audit.cached_moment_interaction_count != 0 {
            return Err(RestoreError::Integrity(
                "replica backup cached records have no coverage state".to_string(),
            ));
        }
    }
    Ok(())
}

fn verify_legacy_replica_checkpoint(
    connection: &Connection,
    identity: &(String, String, bool, String, String),
    audit: &ReplicaRecordAudit,
) -> Result<(), RestoreError> {
    if table_count(connection, "source_checkpoint")? != 1 {
        return Err(RestoreError::Integrity(
            "replica backup checkpoint is not singular".to_string(),
        ));
    }
    let checkpoint: (String, String, i64, i64, i64, i64) = connection.query_row(
        "SELECT source_fingerprint, committed_at_unix_nanoseconds,
                conversation_count, participant_count, message_count, artifact_count
         FROM source_checkpoint WHERE account_id = ?1",
        [&identity.0],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;
    let checkpoint_timestamp =
        verify_positive_timestamp(&checkpoint.1, "replica backup checkpoint")?;
    if checkpoint.0 != identity.1
        || u64::try_from(checkpoint.2).ok() != Some(audit.conversation_count)
        || u64::try_from(checkpoint.3).ok() != Some(audit.participant_count)
        || u64::try_from(checkpoint.4).ok() != Some(audit.message_count)
        || u64::try_from(checkpoint.5).ok() != Some(audit.artifact_count)
    {
        return Err(RestoreError::Integrity(
            "replica backup checkpoint differs from canonical state".to_string(),
        ));
    }
    let matching_run: i64 = connection.query_row(
        "SELECT count(*) FROM sync_run
         WHERE account_id = ?1 AND source_fingerprint = ?2
           AND committed_at_unix_nanoseconds = ?3",
        params![identity.0, identity.1, checkpoint_timestamp.to_string()],
        |row| row.get(0),
    )?;
    if matching_run == 0 {
        return Err(RestoreError::Integrity(
            "replica backup checkpoint has no matching synchronization run".to_string(),
        ));
    }
    Ok(())
}

fn verify_replica_change_stream(
    connection: &Connection,
    account_id: &str,
    change_count: u64,
    synchronization_run_count: u64,
    progress: &ReplicaAuditProgress<'_>,
) -> Result<(), RestoreError> {
    let total = change_count.saturating_add(synchronization_run_count);
    let mut completed = 0_u64;
    progress.emit_rows(
        REPLICA_AUDIT_CHANGE_STAGE,
        ProgressState::Started,
        "verifyReplicaChangeStream",
        completed,
        total,
        ReplicaAuditCounter::Changes,
        Some("change_log"),
    );
    if change_count == 0 {
        return Err(RestoreError::Integrity(
            "initialized replica has no checkpoint change event".to_string(),
        ));
    }
    let (minimum, maximum): (i64, i64) = connection.query_row(
        "SELECT min(sequence), max(sequence) FROM change_log",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if minimum != 1 || u64::try_from(maximum).ok() != Some(change_count) {
        return Err(RestoreError::Integrity(
            "replica change stream sequence is not contiguous".to_string(),
        ));
    }
    let mut statement = connection.prepare(
        "SELECT account_id, source_fingerprint, change_kind, entity_kind,
                record_sha256, observed_at_unix_nanoseconds
         FROM change_log ORDER BY sequence",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let account: String = row.get(0)?;
        let source: String = row.get(1)?;
        let change_kind: String = row.get(2)?;
        let entity_kind: String = row.get(3)?;
        let digest: Option<String> = row.get(4)?;
        let timestamp: String = row.get(5)?;
        if account != account_id
            || source.is_empty()
            || !matches!(
                change_kind.as_str(),
                "bootstrap" | "added" | "changed" | "removed"
            )
            || !matches!(
                entity_kind.as_str(),
                "checkpoint"
                    | "conversation"
                    | "participant"
                    | "message"
                    | "artifact"
                    | "cachedMoment"
                    | "cachedMomentInteraction"
            )
            || digest.as_deref().is_some_and(|value| {
                value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err(RestoreError::Integrity(
                "replica change stream entry is malformed".to_string(),
            ));
        }
        verify_positive_timestamp(&timestamp, "replica change")?;
        completed = completed.saturating_add(1);
        if replica_audit_row_event_due(completed, total) {
            progress.emit_rows(
                REPLICA_AUDIT_CHANGE_STAGE,
                ProgressState::Advanced,
                "verifyReplicaChangeStream",
                completed,
                total,
                ReplicaAuditCounter::Changes,
                Some("change_log"),
            );
        }
    }
    let mut statement = connection.prepare(
        "SELECT account_id, mode, source_fingerprint, started_at_unix_nanoseconds,
                committed_at_unix_nanoseconds, changed_record_count
         FROM sync_run ORDER BY committed_at_unix_nanoseconds",
    )?;
    let mut rows = statement.query([])?;
    let mut run_count = 0_u64;
    while let Some(row) = rows.next()? {
        let account: String = row.get(0)?;
        let mode: String = row.get(1)?;
        let source: String = row.get(2)?;
        let started = verify_positive_timestamp(&row.get::<_, String>(3)?, "sync start")?;
        let committed = verify_positive_timestamp(&row.get::<_, String>(4)?, "sync commit")?;
        let changed: i64 = row.get(5)?;
        if account != account_id
            || source.is_empty()
            || !matches!(
                mode.as_str(),
                "bootstrap" | "incrementalMerge" | "integrityScan" | "fullScan" | "reconcile"
            )
            || committed < started
            || changed < 0
        {
            return Err(RestoreError::Integrity(
                "replica synchronization history is malformed".to_string(),
            ));
        }
        run_count = run_count.saturating_add(1);
        completed = completed.saturating_add(1);
        if replica_audit_row_event_due(completed, total) {
            progress.emit_rows(
                REPLICA_AUDIT_CHANGE_STAGE,
                ProgressState::Advanced,
                "verifyReplicaChangeStream",
                completed,
                total,
                ReplicaAuditCounter::Changes,
                Some("sync_run"),
            );
        }
    }
    if run_count == 0 {
        return Err(RestoreError::Integrity(
            "initialized replica has no synchronization history".to_string(),
        ));
    }
    if run_count != synchronization_run_count {
        return Err(RestoreError::Integrity(
            "replica synchronization history changed during verification".to_string(),
        ));
    }
    progress.emit_rows(
        REPLICA_AUDIT_CHANGE_STAGE,
        ProgressState::Completed,
        "verifyReplicaChangeStream",
        total,
        total,
        ReplicaAuditCounter::Changes,
        None,
    );
    Ok(())
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
        ensure_replica_namespace_absent(path, "replica")?;
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
        validate_replica_migration_ledger(&connection, version)?;
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
        validate_replica_migration_ledger(&connection, CURRENT_SCHEMA_VERSION)?;
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
    let canonical = fs::canonicalize(path)?;
    let connection = Connection::open_with_flags(
        canonical,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    configure_keyed_connection(&connection, key, true)?;
    Ok(connection)
}

fn configure_keyed_connection(
    connection: &Connection,
    key: &ReplicaKey,
    writable: bool,
) -> Result<(), RestoreError> {
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
         PRAGMA temp_store = MEMORY;",
    )?;
    if writable {
        connection.execute_batch("PRAGMA secure_delete = ON;")?;
    }
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))?;
    Ok(())
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

fn validate_replica_migration_ledger(
    connection: &Connection,
    expected_schema_version: u32,
) -> Result<(), RestoreError> {
    if expected_schema_version == 0 {
        return Ok(());
    }
    let (recorded_schema_version, replica_format_version): (i64, i64) = connection.query_row(
        "SELECT schema_version, replica_format_version
         FROM replica_schema WHERE singleton = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if u32::try_from(recorded_schema_version).ok() != Some(expected_schema_version)
        || u32::try_from(replica_format_version).ok() != Some(REPLICA_FORMAT_VERSION)
    {
        return Err(RestoreError::Integrity(
            "replica schema identity does not match its supported format".to_string(),
        ));
    }
    let identities = [
        MIGRATION_1_IDENTITY,
        MIGRATION_2_IDENTITY,
        MIGRATION_3_IDENTITY,
        MIGRATION_4_IDENTITY,
        MIGRATION_5_IDENTITY,
    ];
    let mut statement = connection.prepare(
        "SELECT schema_version, applied_at_unix_nanoseconds, migration_sha256
         FROM migration_history ORDER BY schema_version",
    )?;
    let mut rows = statement.query([])?;
    let mut observed = 0_u32;
    while let Some(row) = rows.next()? {
        let version: i64 = row.get(0)?;
        let applied_at: String = row.get(1)?;
        let digest: String = row.get(2)?;
        observed = observed.checked_add(1).ok_or_else(|| {
            RestoreError::Integrity("replica migration count overflowed".to_string())
        })?;
        let expected_identity = identities.get((observed - 1) as usize).ok_or_else(|| {
            RestoreError::Integrity("replica migration history has unexpected entries".to_string())
        })?;
        let expected_digest = hex::encode(Sha256::digest(expected_identity.as_bytes()));
        if u32::try_from(version).ok() != Some(observed)
            || applied_at
                .parse::<u128>()
                .ok()
                .is_none_or(|timestamp| timestamp == 0)
            || digest != expected_digest
        {
            return Err(RestoreError::Integrity(
                "replica migration history failed identity verification".to_string(),
            ));
        }
    }
    if observed != expected_schema_version {
        return Err(RestoreError::Integrity(
            "replica migration history is incomplete".to_string(),
        ));
    }
    Ok(())
}

fn apply_migrations(connection: &mut Connection, from: u32) -> Result<(), RestoreError> {
    for version in (from + 1)..=CURRENT_SCHEMA_VERSION {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        match version {
            1 => migration_1(&transaction)?,
            2 => migration_2(&transaction)?,
            3 => migration_3(&transaction)?,
            4 => migration_4(&transaction)?,
            5 => migration_5(&transaction)?,
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
    record_migration(transaction, 1, MIGRATION_1_IDENTITY)?;
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
         INSERT INTO message_fts(account_id, canonical_id, conversation_id, search_text)
           SELECT account_id, canonical_id, conversation_id, search_text FROM message;
         INSERT INTO source_checkpoint(
           account_id, source_fingerprint, committed_at_unix_nanoseconds,
           conversation_count, participant_count, message_count, artifact_count
         )
           SELECT account_id, current_source_fingerprint, updated_at_unix_nanoseconds,
                  (SELECT count(*) FROM conversation),
                  (SELECT count(*) FROM participant),
                  (SELECT count(*) FROM message),
                  (SELECT count(*) FROM artifact)
           FROM replica_identity
           WHERE current_source_fingerprint IS NOT NULL;
         INSERT INTO sync_run(
           run_id, account_id, mode, source_fingerprint,
           started_at_unix_nanoseconds, committed_at_unix_nanoseconds,
           changed_record_count
         )
           SELECT lower(hex(randomblob(16))), account_id, 'reconcile',
                  current_source_fingerprint, updated_at_unix_nanoseconds,
                  updated_at_unix_nanoseconds,
                  (SELECT count(*) FROM conversation)
                    + (SELECT count(*) FROM participant)
                    + (SELECT count(*) FROM message)
                    + (SELECT count(*) FROM artifact)
           FROM replica_identity
           WHERE current_source_fingerprint IS NOT NULL;
         INSERT INTO change_log(
           account_id, source_fingerprint, change_kind, entity_kind, entity_id,
           conversation_id, record_sha256, observed_at_unix_nanoseconds
         )
           SELECT account_id, current_source_fingerprint, 'bootstrap', 'checkpoint',
                  current_source_fingerprint, NULL, NULL, updated_at_unix_nanoseconds
           FROM replica_identity
           WHERE current_source_fingerprint IS NOT NULL;
         UPDATE replica_schema SET schema_version = 2 WHERE singleton = 1;",
    )?;
    record_migration(transaction, 2, MIGRATION_2_IDENTITY)?;
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
    record_migration(transaction, 3, MIGRATION_3_IDENTITY)?;
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
    record_migration(transaction, 4, MIGRATION_4_IDENTITY)?;
    Ok(())
}

fn migration_5(transaction: &Transaction<'_>) -> Result<(), RestoreError> {
    transaction.execute_batch(
        "ALTER TABLE replica_identity ADD COLUMN self_participant_id TEXT;
         UPDATE replica_schema SET schema_version = 5 WHERE singleton = 1;",
    )?;
    record_migration(transaction, 5, MIGRATION_5_IDENTITY)?;
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
    ensure_replica_namespace_absent(&path, "pre-migration backup")?;
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
        destination.execute_batch(
            "PRAGMA wal_checkpoint(TRUNCATE);
             PRAGMA journal_mode = DELETE;
             PRAGMA synchronous = FULL;",
        )?;
        drop(destination);
        secure_replica_files(&path)?;
        let audit = audit_replica_backup(&path, key)?;
        if audit.schema_version != version {
            return Err(RestoreError::Integrity(
                "pre-migration backup schema changed during verification".to_string(),
            ));
        }
        Ok(())
    })();
    if result.is_err() {
        remove_failed_replica_files(&path);
    }
    result.map(|()| file_name)
}

impl ReplicaApplicationPlan {
    fn prepare(
        archive_directory: &Path,
        replica_path: &Path,
        report: &RestorationReport,
    ) -> Result<Self, RestoreError> {
        let archive_byte_count = report
            .storage
            .as_ref()
            .map(|storage| storage.actual_archive_byte_count)
            .filter(|value| *value > 0)
            .map_or_else(|| archive_input_byte_count(archive_directory, report), Ok)?;
        let estimated_peak_byte_count = archive_byte_count.saturating_mul(4);
        let reserve_byte_count = (archive_byte_count / 10).max(64 * 1024 * 1024);
        let required_free_byte_count = estimated_peak_byte_count.saturating_add(reserve_byte_count);
        let parent = replica_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let available_free_byte_count_at_start = available_free_bytes(parent)?;
        let integrity = &report.integrity;
        let source_record_count = integrity
            .conversation_count
            .saturating_add(integrity.participant_count)
            .saturating_add(integrity.restored_row_count)
            .saturating_add(integrity.unique_artifact_count)
            .saturating_add(integrity.cached_moment_count)
            .saturating_add(integrity.cached_moment_interaction_count);
        let total_work_record_count = source_record_count
            .saturating_add(integrity.conversation_count)
            .saturating_add(1);
        let cached_file_count = usize::from(report.cached_moments_path.is_some())
            + usize::from(report.cached_moment_interactions_path.is_some());
        Ok(Self {
            archive_byte_count,
            estimated_peak_byte_count,
            required_free_byte_count,
            available_free_byte_count_at_start,
            total_work_record_count,
            source_record_count,
            file_count: 5 + cached_file_count,
            database_coverage: database_coverage_summary(report),
        })
    }
}

fn preflight_replica_application(
    archive_directory: &Path,
    replica_path: &Path,
    report: &RestorationReport,
    progress: &dyn ProgressObserver,
) -> Result<ReplicaApplicationPlan, RestoreError> {
    let plan = ReplicaApplicationPlan::prepare(archive_directory, replica_path, report)?;
    let mut planned = ProgressEvent::new(
        ProgressPhase::ReplicaApplication,
        ProgressState::Planned,
        "preflightReplicaStorage",
        ProgressUnit::Bytes,
        plan.available_free_byte_count_at_start
            .min(plan.required_free_byte_count),
        plan.required_free_byte_count,
        0,
        plan.total_work_record_count,
    );
    attach_replica_application_evidence(&mut planned, &plan, replica_path)?;
    progress.observe(planned);
    if plan.available_free_byte_count_at_start < plan.required_free_byte_count {
        return Err(RestoreError::InsufficientReplicaDiskSpace {
            available_byte_count: plan.available_free_byte_count_at_start,
            required_free_byte_count: plan.required_free_byte_count,
            estimated_peak_byte_count: plan.estimated_peak_byte_count,
        });
    }
    let mut completed = ProgressEvent::new(
        ProgressPhase::ReplicaApplication,
        ProgressState::Completed,
        "preflightReplicaStorage",
        ProgressUnit::Bytes,
        plan.required_free_byte_count,
        plan.required_free_byte_count,
        0,
        plan.total_work_record_count,
    );
    attach_replica_application_evidence(&mut completed, &plan, replica_path)?;
    progress.observe(completed);
    Ok(plan)
}

fn emit_idempotent_replica_progress(
    report: &RestorationReport,
    archive_directory: &Path,
    replica_path: &Path,
    progress: &dyn ProgressObserver,
) -> Result<(), RestoreError> {
    let plan = ReplicaApplicationPlan::prepare(archive_directory, replica_path, report)?;
    let mut event = ProgressEvent::new(
        ProgressPhase::ReplicaApplication,
        ProgressState::Completed,
        "reuseReplicaCheckpoint",
        ProgressUnit::Records,
        plan.source_record_count,
        plan.source_record_count,
        plan.total_work_record_count,
        plan.total_work_record_count,
    );
    event.restored_record_count = Some(plan.source_record_count);
    attach_replica_application_evidence(&mut event, &plan, replica_path)?;
    progress.observe(event);
    Ok(())
}

fn emit_replica_checkpoint_progress(
    plan: &ReplicaApplicationPlan,
    replica_path: &Path,
    state: ProgressState,
    progress: &dyn ProgressObserver,
) -> Result<(), RestoreError> {
    let completed = u64::from(state == ProgressState::Completed);
    let mut event = ProgressEvent::new(
        ProgressPhase::ReplicaApplication,
        state,
        "checkpointEncryptedReplica",
        ProgressUnit::Items,
        completed,
        1,
        plan.total_work_record_count.saturating_sub(1) + completed,
        plan.total_work_record_count,
    );
    event.restored_record_count = Some(plan.source_record_count);
    attach_replica_application_evidence(&mut event, plan, replica_path)?;
    progress.observe(event);
    Ok(())
}

fn attach_replica_application_evidence(
    event: &mut ProgressEvent,
    plan: &ReplicaApplicationPlan,
    replica_path: &Path,
) -> Result<(), RestoreError> {
    let parent = replica_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    event.archive_byte_count = Some(plan.archive_byte_count);
    event.replica_file_byte_count = Some(replica_namespace_byte_count(replica_path)?);
    event.estimated_peak_byte_count = Some(plan.estimated_peak_byte_count);
    event.required_free_byte_count = Some(plan.required_free_byte_count);
    event.available_free_byte_count = Some(available_free_bytes(parent)?);
    event.database_count = Some(plan.database_coverage.total);
    event.available_database_count = Some(plan.database_coverage.restored);
    event.unavailable_database_count = Some(plan.database_coverage.unavailable);
    event.source_record_count = Some(plan.source_record_count);
    Ok(())
}

fn archive_input_byte_count(
    archive_directory: &Path,
    report: &RestorationReport,
) -> Result<u64, RestoreError> {
    let mut relative_paths = vec![
        "messages.ndjson",
        "artifacts.ndjson",
        "conversations.ndjson",
        "participants.ndjson",
        "coverage.json",
        "report.json",
    ];
    if report.cached_moments_path.is_some() {
        relative_paths.push("cached-moments.ndjson");
    }
    if report.cached_moment_interactions_path.is_some() {
        relative_paths.push("cached-moment-interactions.ndjson");
    }
    if report.cached_surfaces_path.is_some() {
        relative_paths.push("cached-surfaces.json");
    }
    let mut total = 0_u64;
    for relative in relative_paths {
        let path = archive_directory.join(relative);
        ensure_private_regular_file(&path)?;
        total = total.saturating_add(fs::metadata(path)?.len());
    }
    Ok(total)
}

fn replica_namespace_byte_count(replica_path: &Path) -> Result<u64, RestoreError> {
    let mut total = 0_u64;
    for path in sqlite_file_namespace(replica_path) {
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.nlink() != 1
                {
                    return Err(RestoreError::Integrity(
                        "replica storage contains an unsafe file identity".to_string(),
                    ));
                }
                total = total.saturating_add(metadata.len());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(total)
}

#[allow(clippy::too_many_arguments)]
fn for_each_ndjson_with_replica_progress<T: DeserializeOwned + Serialize>(
    path: &Path,
    operation: &str,
    file_index: usize,
    expected_record_count: u64,
    phase_start: u64,
    restored_before: u64,
    counts_as_restored: bool,
    plan: &ReplicaApplicationPlan,
    replica_path: &Path,
    progress: &dyn ProgressObserver,
    mut body: impl FnMut(T, Vec<u8>) -> Result<(), RestoreError>,
) -> Result<u64, RestoreError> {
    let file_byte_count = fs::metadata(path)?.len();
    let started_at = Instant::now();
    let mut started = ProgressEvent::new(
        ProgressPhase::ReplicaApplication,
        ProgressState::Started,
        operation,
        ProgressUnit::Records,
        0,
        expected_record_count,
        phase_start,
        plan.total_work_record_count,
    );
    started.file_index = Some(file_index);
    started.file_count = Some(plan.file_count);
    started.file_completed_byte_count = Some(0);
    started.file_byte_count = Some(file_byte_count);
    started.logical_path = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    started.restored_record_count = Some(restored_before);
    attach_replica_application_evidence(&mut started, plan, replica_path)?;
    progress.observe(started);

    let mut reader = BufReader::new(File::open(path)?);
    let mut line = Vec::new();
    let mut processed = 0_u64;
    let mut completed_bytes = 0_u64;
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        completed_bytes = completed_bytes.saturating_add(read as u64);
        if line.last() == Some(&b'\n') {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
        }
        if line.is_empty() {
            continue;
        }
        let value: T = serde_json::from_slice(&line)?;
        let canonical = serde_json::to_vec(&value)?;
        body(value, canonical)?;
        processed = processed.saturating_add(1);
        if processed == 1 || processed.is_multiple_of(1_000) {
            let mut advanced = ProgressEvent::new(
                ProgressPhase::ReplicaApplication,
                ProgressState::Advanced,
                operation,
                ProgressUnit::Records,
                processed,
                expected_record_count,
                phase_start.saturating_add(processed),
                plan.total_work_record_count,
            );
            advanced.file_index = Some(file_index);
            advanced.file_count = Some(plan.file_count);
            advanced.file_completed_byte_count = Some(completed_bytes.min(file_byte_count));
            advanced.file_byte_count = Some(file_byte_count);
            advanced.logical_path = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            advanced.restored_record_count = Some(if counts_as_restored {
                restored_before.saturating_add(processed)
            } else {
                restored_before
            });
            attach_replica_application_evidence(&mut advanced, plan, replica_path)?;
            progress.observe(advanced);
        }
    }
    if processed != expected_record_count || completed_bytes != file_byte_count {
        return Err(RestoreError::Integrity(format!(
            "replica import inventory changed for {}: expected {expected_record_count} records/{file_byte_count} bytes, observed {processed} records/{completed_bytes} bytes",
            path.file_name()
                .map_or_else(|| "archive ledger".into(), |name| name.to_string_lossy())
        )));
    }
    let mut completed = ProgressEvent::new(
        ProgressPhase::ReplicaApplication,
        ProgressState::Completed,
        operation,
        ProgressUnit::Records,
        processed,
        expected_record_count,
        phase_start.saturating_add(processed),
        plan.total_work_record_count,
    );
    completed.file_index = Some(file_index);
    completed.file_count = Some(plan.file_count);
    completed.file_completed_byte_count = Some(file_byte_count);
    completed.file_byte_count = Some(file_byte_count);
    completed.logical_path = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    completed.restored_record_count = Some(if counts_as_restored {
        restored_before.saturating_add(processed)
    } else {
        restored_before
    });
    completed.elapsed_milliseconds =
        Some(u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX));
    attach_replica_application_evidence(&mut completed, plan, replica_path)?;
    progress.observe(completed);
    Ok(processed)
}

fn import_archive_transactionally(
    connection: &mut Connection,
    archive_directory: &Path,
    report: &RestorationReport,
    plan: &ReplicaApplicationPlan,
    replica_path: &Path,
    progress: &dyn ProgressObserver,
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
           created_at_unix_nanoseconds, updated_at_unix_nanoseconds, self_participant_id
         ) VALUES (1, ?1, NULL, NULL, ?2, ?2, ?3)",
        params![
            report.account_id,
            started.to_string(),
            report.self_participant_id
        ],
    )?;
    let mut counts = ImportCounts::default();
    let mut phase_cursor = 0_u64;
    let mut restored_record_count = 0_u64;
    let mut file_index = 1_usize;

    let processed = for_each_ndjson_with_replica_progress::<CanonicalConversation>(
        &conversations_path,
        "applyReplicaConversations",
        file_index,
        report.integrity.conversation_count,
        phase_cursor,
        restored_record_count,
        true,
        plan,
        replica_path,
        progress,
        |conversation, bytes| {
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
        },
    )?;
    phase_cursor = phase_cursor.saturating_add(processed);
    restored_record_count = restored_record_count.saturating_add(processed);
    file_index += 1;

    let processed = for_each_ndjson_with_replica_progress::<CanonicalParticipant>(
        &participants_path,
        "applyReplicaParticipants",
        file_index,
        report.integrity.participant_count,
        phase_cursor,
        restored_record_count,
        true,
        plan,
        replica_path,
        progress,
        |participant, bytes| {
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
        },
    )?;
    phase_cursor = phase_cursor.saturating_add(processed);
    restored_record_count = restored_record_count.saturating_add(processed);
    file_index += 1;

    let processed = for_each_ndjson_with_replica_progress::<CanonicalConversation>(
        &conversations_path,
        "applyReplicaConversationMemberships",
        file_index,
        report.integrity.conversation_count,
        phase_cursor,
        restored_record_count,
        false,
        plan,
        replica_path,
        progress,
        |conversation, _| {
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
        },
    )?;
    phase_cursor = phase_cursor.saturating_add(processed);
    file_index += 1;

    let processed = for_each_ndjson_with_replica_progress::<CanonicalArtifact>(
        &artifacts_path,
        "applyReplicaArtifacts",
        file_index,
        report.integrity.unique_artifact_count,
        phase_cursor,
        restored_record_count,
        true,
        plan,
        replica_path,
        progress,
        |artifact, bytes| {
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
        },
    )?;
    phase_cursor = phase_cursor.saturating_add(processed);
    restored_record_count = restored_record_count.saturating_add(processed);
    file_index += 1;

    let processed = for_each_ndjson_with_replica_progress::<CanonicalMessage>(
        &messages_path,
        "applyReplicaMessages",
        file_index,
        report.integrity.restored_row_count,
        phase_cursor,
        restored_record_count,
        true,
        plan,
        replica_path,
        progress,
        |message, bytes| {
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
        },
    )?;
    phase_cursor = phase_cursor.saturating_add(processed);
    restored_record_count = restored_record_count.saturating_add(processed);
    file_index += 1;

    if let Some(cached) = cached.as_ref() {
        let processed = for_each_ndjson_with_replica_progress::<crate::CanonicalCachedMoment>(
            &cached.moments_path,
            "applyReplicaCachedMoments",
            file_index,
            report.integrity.cached_moment_count,
            phase_cursor,
            restored_record_count,
            true,
            plan,
            replica_path,
            progress,
            |moment, bytes| {
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
            },
        )?;
        phase_cursor = phase_cursor.saturating_add(processed);
        restored_record_count = restored_record_count.saturating_add(processed);
        file_index += 1;

        let processed =
            for_each_ndjson_with_replica_progress::<crate::CanonicalCachedMomentInteraction>(
                &cached.interactions_path,
                "applyReplicaCachedMomentInteractions",
                file_index,
                report.integrity.cached_moment_interaction_count,
                phase_cursor,
                restored_record_count,
                true,
                plan,
                replica_path,
                progress,
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
        phase_cursor = phase_cursor.saturating_add(processed);
        restored_record_count = restored_record_count.saturating_add(processed);
        file_index += 1;
        transaction.execute(
            "INSERT INTO cached_surface_state VALUES (?1, ?2, ?3)",
            params![
                report.account_id,
                cached.coverage.observed_at,
                serde_json::to_vec(&cached.coverage)?,
            ],
        )?;
    }

    if phase_cursor != plan.total_work_record_count.saturating_sub(1)
        || restored_record_count != plan.source_record_count
        || file_index.saturating_sub(1) != plan.file_count
    {
        return Err(RestoreError::Integrity(
            "replica import progress inventory differs from the audited archive".to_string(),
        ));
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
           updated_at_unix_nanoseconds = ?4,
           self_participant_id = ?5
         WHERE account_id = ?1",
        params![
            report.account_id,
            report.source_fingerprint,
            report.completion.full_restoration_achieved,
            committed.to_string(),
            report.self_participant_id,
        ],
    )?;
    transaction.commit()?;
    Ok(counts)
}

fn require_authoritative_archive(report: &RestorationReport) -> Result<(), RestoreError> {
    if report.replica_mutation_eligible() {
        return Ok(());
    }
    let scope = replica_ineligible_scope(report);
    Err(RestoreError::Integrity(format!(
        "replica mutation requires an independently audited replica-eligible archive; archive scope is {scope}"
    )))
}

fn require_serving_archive(report: &RestorationReport) -> Result<(), RestoreError> {
    if report.replica_serving_eligible() {
        return Ok(());
    }
    let scope = replica_ineligible_scope(report);
    Err(RestoreError::Integrity(format!(
        "replica bootstrap requires an audited archive that accounts for every source database as fresh or explicitly unavailable; archive scope is {scope}"
    )))
}

fn replica_ineligible_scope(report: &RestorationReport) -> &'static str {
    match report.archive_scope {
        crate::RestorationArchiveScope::Authoritative
        | crate::RestorationArchiveScope::PartialDatabaseCoverage => "invalidDatabaseCoverage",
        crate::RestorationArchiveScope::IncrementalFragment => "incrementalFragment",
        crate::RestorationArchiveScope::DiagnosticSubset => "diagnosticSubset",
    }
}

fn ensure_partial_database_transition_is_lossless(
    previous_report: &RestorationReport,
    previous_coverage: &RestorationCoverage,
    incoming_report: &RestorationReport,
) -> Result<(), RestoreError> {
    if incoming_report.archive_scope != crate::RestorationArchiveScope::PartialDatabaseCoverage {
        return Ok(());
    }
    let incoming = incoming_report.database_coverage.as_ref().ok_or_else(|| {
        RestoreError::Integrity(
            "partial replica synchronization has no database coverage evidence".to_string(),
        )
    })?;
    let previous_included = previous_report
        .database_coverage
        .as_ref()
        .map(|coverage| {
            coverage
                .included_source_set_ids()
                .into_iter()
                .map(str::to_string)
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_else(|| {
            previous_coverage
                .all_tables
                .iter()
                .map(|table| table.source_set_id.clone())
                .collect()
        });
    let current_inventory = incoming
        .snapshot_source_set_ids
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let incoming_included = incoming
        .included_source_set_ids()
        .into_iter()
        .map(str::to_string)
        .collect::<std::collections::BTreeSet<_>>();
    let would_drop_unavailable_records = previous_included
        .intersection(&current_inventory)
        .any(|source_set| !incoming_included.contains(source_set));
    if would_drop_unavailable_records {
        return Err(RestoreError::Integrity(
            "partial replica synchronization would discard records from an unavailable database; merge it with the previous archive first"
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
    plan: &ReplicaApplicationPlan,
    replica_path: &Path,
    progress: &dyn ProgressObserver,
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
    let mut phase_cursor = 0_u64;
    let mut restored_record_count = 0_u64;
    let mut file_index = 1_usize;

    let processed = for_each_ndjson_with_replica_progress::<CanonicalConversation>(
        &conversations_path,
        "reconcileReplicaConversations",
        file_index,
        report.integrity.conversation_count,
        phase_cursor,
        restored_record_count,
        true,
        plan,
        replica_path,
        progress,
        |conversation, bytes| {
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
        },
    )?;
    phase_cursor = phase_cursor.saturating_add(processed);
    restored_record_count = restored_record_count.saturating_add(processed);
    file_index += 1;

    let processed = for_each_ndjson_with_replica_progress::<CanonicalParticipant>(
        &participants_path,
        "reconcileReplicaParticipants",
        file_index,
        report.integrity.participant_count,
        phase_cursor,
        restored_record_count,
        true,
        plan,
        replica_path,
        progress,
        |participant, bytes| {
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
        },
    )?;
    phase_cursor = phase_cursor.saturating_add(processed);
    restored_record_count = restored_record_count.saturating_add(processed);
    file_index += 1;

    let processed = for_each_ndjson_with_replica_progress::<CanonicalConversation>(
        &conversations_path,
        "reconcileReplicaConversationMemberships",
        file_index,
        report.integrity.conversation_count,
        phase_cursor,
        restored_record_count,
        false,
        plan,
        replica_path,
        progress,
        |conversation, _| {
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
        },
    )?;
    phase_cursor = phase_cursor.saturating_add(processed);
    file_index += 1;

    let processed = for_each_ndjson_with_replica_progress::<CanonicalArtifact>(
        &artifacts_path,
        "reconcileReplicaArtifacts",
        file_index,
        report.integrity.unique_artifact_count,
        phase_cursor,
        restored_record_count,
        true,
        plan,
        replica_path,
        progress,
        |artifact, bytes| {
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
            if existing.as_deref() != Some(digest.as_str()) {
                validate_canonical_artifact(&artifact, &coverage)?;
                if report.format_version >= 3 {
                    verify_recorded_artifact_files(archive_directory, &artifact)?;
                }
            }
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
        },
    )?;
    phase_cursor = phase_cursor.saturating_add(processed);
    restored_record_count = restored_record_count.saturating_add(processed);
    file_index += 1;

    let processed = for_each_ndjson_with_replica_progress::<CanonicalMessage>(
        &messages_path,
        "reconcileReplicaMessages",
        file_index,
        report.integrity.restored_row_count,
        phase_cursor,
        restored_record_count,
        true,
        plan,
        replica_path,
        progress,
        |message, bytes| {
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
        },
    )?;
    phase_cursor = phase_cursor.saturating_add(processed);
    restored_record_count = restored_record_count.saturating_add(processed);
    file_index += 1;

    reconcile_cached_surfaces(
        &transaction,
        report,
        cached.as_ref(),
        &mut counts,
        started,
        plan,
        replica_path,
        progress,
        &mut phase_cursor,
        &mut restored_record_count,
        &mut file_index,
    )?;

    if phase_cursor != plan.total_work_record_count.saturating_sub(1)
        || restored_record_count != plan.source_record_count
        || file_index.saturating_sub(1) != plan.file_count
    {
        return Err(RestoreError::Integrity(
            "replica reconciliation progress inventory differs from the audited archive"
                .to_string(),
        ));
    }

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
           updated_at_unix_nanoseconds = ?4,
           self_participant_id = ?5
         WHERE account_id = ?1",
        params![
            report.account_id,
            report.source_fingerprint,
            report.completion.full_restoration_achieved,
            committed.to_string(),
            report.self_participant_id,
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

#[allow(clippy::too_many_arguments)]
fn reconcile_cached_surfaces(
    transaction: &Transaction<'_>,
    report: &RestorationReport,
    cached: Option<&CachedArchiveInputs>,
    counts: &mut SyncCounts,
    observed_at: u128,
    plan: &ReplicaApplicationPlan,
    replica_path: &Path,
    progress: &dyn ProgressObserver,
    phase_cursor: &mut u64,
    restored_record_count: &mut u64,
    file_index: &mut usize,
) -> Result<(), RestoreError> {
    let Some(cached) = cached else {
        return Ok(());
    };
    let processed = for_each_ndjson_with_replica_progress::<crate::CanonicalCachedMoment>(
        &cached.moments_path,
        "reconcileReplicaCachedMoments",
        *file_index,
        report.integrity.cached_moment_count,
        *phase_cursor,
        *restored_record_count,
        true,
        plan,
        replica_path,
        progress,
        |moment, bytes| {
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
        },
    )?;
    *phase_cursor = phase_cursor.saturating_add(processed);
    *restored_record_count = restored_record_count.saturating_add(processed);
    *file_index += 1;

    let processed = for_each_ndjson_with_replica_progress::<crate::CanonicalCachedMomentInteraction>(
        &cached.interactions_path,
        "reconcileReplicaCachedMomentInteractions",
        *file_index,
        report.integrity.cached_moment_interaction_count,
        *phase_cursor,
        *restored_record_count,
        true,
        plan,
        replica_path,
        progress,
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
    *phase_cursor = phase_cursor.saturating_add(processed);
    *restored_record_count = restored_record_count.saturating_add(processed);
    *file_index += 1;
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
    report: &RestorationReport,
    idempotent: bool,
    counts: SyncCounts,
    committed_at_unix_nanoseconds: Option<u128>,
) -> Result<ReplicaSyncReport, RestoreError> {
    let database_coverage = database_coverage_summary(report);
    Ok(ReplicaSyncReport {
        format_version: REPLICA_FORMAT_VERSION,
        account_id: account_id.to_string(),
        self_participant_id: report.self_participant_id.clone(),
        previous_source_fingerprint: previous_source_fingerprint.to_string(),
        current_source_fingerprint: report.source_fingerprint.clone(),
        idempotent,
        archive_scope: report.archive_scope,
        authoritative_database_coverage: database_coverage.authoritative,
        total_database_count: database_coverage.total,
        restored_database_count: database_coverage.restored,
        unavailable_database_count: database_coverage.unavailable,
        preserved_stale_database_count: database_coverage.preserved_stale,
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
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .map_err(RestoreError::from)
        .and_then(|(account, fingerprint)| {
            let fingerprint = fingerprint
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    RestoreError::Integrity(
                        "replica has no authoritative source checkpoint".to_string(),
                    )
                })?;
            if account.is_empty() {
                return Err(RestoreError::Integrity(
                    "replica has an empty account identity".to_string(),
                ));
            }
            Ok((account, fingerprint))
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
            let fingerprint = fingerprint
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    RestoreError::Integrity(
                        "replica has no authoritative source checkpoint".to_string(),
                    )
                })?;
            if account.is_empty() || !revision.parse::<u128>().is_ok_and(|revision| revision > 0) {
                return Err(RestoreError::Integrity(
                    "replica checkpoint identity or revision is invalid".to_string(),
                ));
            }
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
    let database_coverage = database_coverage_summary(report);
    Ok(ReplicaBootstrapReport {
        format_version: REPLICA_FORMAT_VERSION,
        schema_version: CURRENT_SCHEMA_VERSION,
        account_id: report.account_id.clone(),
        self_participant_id: report.self_participant_id.clone(),
        source_fingerprint: report.source_fingerprint.clone(),
        cipher_version: opened.cipher_version.clone(),
        encrypted_at_rest: true,
        idempotent,
        archive_scope: report.archive_scope,
        authoritative_database_coverage: database_coverage.authoritative,
        total_database_count: database_coverage.total,
        restored_database_count: database_coverage.restored,
        unavailable_database_count: database_coverage.unavailable,
        preserved_stale_database_count: database_coverage.preserved_stale,
        conversation_count: table_count(&opened.connection, "conversation")?,
        participant_count: table_count(&opened.connection, "participant")?,
        message_count: table_count(&opened.connection, "message")?,
        artifact_count: table_count(&opened.connection, "artifact")?,
        cached_moment_count: table_count(&opened.connection, "cached_moment")?,
        cached_moment_interaction_count: table_count(
            &opened.connection,
            "cached_moment_interaction",
        )?,
        cached_surface_omitted_row_count: report.integrity.cached_surface_omitted_row_count,
        relationship_count: table_count(&opened.connection, "message_relationship")?,
        message_artifact_count: table_count(&opened.connection, "message_artifact")?,
        pre_migration_backup_file_name: opened.pre_migration_backup_file_name.clone(),
    })
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

fn require_compatible_self_participant(
    existing: Option<&str>,
    incoming: Option<&str>,
) -> Result<(), RestoreError> {
    if existing.is_some_and(|value| Some(value) != incoming)
        || (existing.is_some() && incoming.is_none())
    {
        return Err(RestoreError::Integrity(
            "replica belongs to a different account holder".to_string(),
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

fn best_effort_table_count(
    connection: &Connection,
    table: &str,
    limitation_code: &str,
    limitation_codes: &mut BTreeSet<String>,
) -> u64 {
    match table_count(connection, table) {
        Ok(count) => count,
        Err(_) => {
            limitation_codes.insert(limitation_code.to_string());
            0
        }
    }
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

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
    let coverage = serde_json::from_slice(&fs::read(path)?)?;
    validate_restoration_coverage_schema(&coverage)?;
    Ok(coverage)
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
    validate_cached_coverage_schema(&coverage)?;
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
            &report.self_participant_id,
            &report.account_binding_evidence,
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
    for candidate in sqlite_file_namespace(path) {
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
    for candidate in sqlite_file_namespace(path) {
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

fn ensure_distinct_replica_namespaces(
    source: &Path,
    destination: &Path,
) -> Result<(), RestoreError> {
    let source = sqlite_file_namespace(source);
    let destination = sqlite_file_namespace(destination);
    if source.iter().any(|path| destination.contains(path)) {
        return Err(RestoreError::Integrity(
            "recovery source and candidate SQLite namespaces overlap".to_string(),
        ));
    }
    Ok(())
}

fn ensure_replica_namespace_absent(path: &Path, label: &str) -> Result<(), RestoreError> {
    for candidate in sqlite_file_namespace(path) {
        match fs::symlink_metadata(candidate) {
            Ok(_) => {
                return Err(RestoreError::Integrity(format!(
                    "{label} storage already exists"
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn replica_namespace_is_absent(path: &Path) -> Result<bool, RestoreError> {
    for candidate in sqlite_file_namespace(path) {
        match fs::symlink_metadata(candidate) {
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}

fn sqlite_file_namespace(path: &Path) -> Vec<PathBuf> {
    let mut result = replica_file_set(path).to_vec();
    let mut journal = path.as_os_str().as_encoded_bytes().to_vec();
    journal.extend_from_slice(b"-journal");
    result.push(PathBuf::from(std::ffi::OsString::from_vec(journal)));
    result
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
