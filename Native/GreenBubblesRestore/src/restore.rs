use std::collections::{BTreeSet, HashMap};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::mem::MaybeUninit;
use std::ops::Deref;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine;
use rusqlite::{types::ValueRef, Connection, OpenFlags, OptionalExtension, Row};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::cached::restore_cached_surfaces_with_progress;
use crate::entities::{restore_entities, EntitySeeds};
use crate::schema::{schema_profile_fingerprint, table_schema_fingerprint};
use crate::{
    artifact::ArtifactResolver, ArtifactAvailability, ArtifactDecodeState, CanonicalMessage,
    DirectionEvidence, MessageDirection, MessageOrderingBasis, MessageRelationship,
    MessageRelationshipKind, MessageTableCoverage, NoProgress, PreparedCatalog, ProgressEvent,
    ProgressObserver, ProgressPhase, ProgressState, ProgressUnit, RawSQLiteValue, RejectedRow,
    RelationshipResolutionState, RestorationCompletion, RestorationCoverage,
    RestorationDatabaseCoverage, RestorationIntegrity, RestorationReport,
    RestorationStorageEvidence, RestorationUnavailableDatabase, RestoreError, SemanticDecodeState,
    SnapshotFileRole, TableCoverageRole, TableSchemaCoverage, TypedPayload,
};

const STAGING_COMPRESSION_LEVEL: i32 = 1;
const ARCHIVE_SOURCE_BYTE_MULTIPLIER: u64 = 16;
const ARCHIVE_MESSAGE_RECORD_OVERHEAD: u64 = 4 * 1024;
const ARCHIVE_OTHER_RECORD_OVERHEAD: u64 = 512;
const ARCHIVE_FIXED_OVERHEAD: u64 = 16 * 1024 * 1024;
const STAGING_SOURCE_BYTE_MULTIPLIER: u64 = 4;
const STAGING_MESSAGE_RECORD_OVERHEAD: u64 = 1024;
const STAGING_FIXED_OVERHEAD: u64 = 8 * 1024 * 1024;
const MINIMUM_FREE_SPACE_RESERVE: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct RestorationOptions {
    pub output_directory: PathBuf,
    pub account_root: Option<PathBuf>,
    pub defer_media: bool,
}

struct ResolvedAccountBinding {
    account_id: String,
    self_source_identifier: Option<String>,
    self_participant_id: Option<String>,
    evidence: Option<crate::AccountHolderBindingEvidence>,
}

#[derive(Debug, Clone)]
struct RestorationStoragePlan {
    source_byte_count: u64,
    message_record_count: u64,
    observed_table_record_count: u64,
    estimated_archive_byte_count: u64,
    estimated_staging_byte_count: u64,
    estimated_peak_byte_count: u64,
    reserve_byte_count: u64,
    required_free_byte_count: u64,
    available_free_byte_count_at_start: u64,
}

#[derive(Debug, Default, Clone, Copy)]
struct StagingStorageStats {
    uncompressed_payload_byte_count: u64,
    compressed_payload_byte_count: u64,
    peak_file_byte_count: u64,
}

struct RestorationStaging {
    _directory: tempfile::TempDir,
    path: PathBuf,
    connection: Connection,
}

impl RestorationStaging {
    fn create(output_directory: &Path) -> Result<Self, RestoreError> {
        let directory = tempfile::Builder::new()
            .prefix(".staging-")
            .tempdir_in(output_directory)?;
        create_owner_only_directory(directory.path())?;
        let path = directory.path().join("messages.sqlite");
        let connection = Connection::open(&path)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        connection.execute_batch(
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
               message_json_zstd BLOB NOT NULL
             );
             CREATE INDEX staged_by_order ON staged_message(
               conversation_id, sort_sequence, server_id, created_at, local_id,
               source_logical_path, source_table_id, source_row_id
             );
             CREATE INDEX staged_by_server ON staged_message(conversation_id, server_id);
             CREATE INDEX staged_by_local ON staged_message(conversation_id, local_id);",
        )?;
        Ok(Self {
            _directory: directory,
            path,
            connection,
        })
    }

    fn file_byte_count(&self) -> Result<u64, RestoreError> {
        Ok(fs::metadata(&self.path)?.len())
    }
}

impl Deref for RestorationStaging {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

struct ByteCountingWriter<W> {
    inner: W,
    byte_count: u64,
}

impl<W> ByteCountingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            byte_count: 0,
        }
    }

    fn byte_count(&self) -> u64 {
        self.byte_count
    }
}

impl<W: Write> Write for ByteCountingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.byte_count = self.byte_count.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub fn restore_catalog(
    catalog: &PreparedCatalog,
    options: &RestorationOptions,
) -> Result<RestorationReport, RestoreError> {
    restore_catalog_with_progress(catalog, options, &NoProgress)
}

pub fn restore_catalog_with_progress(
    catalog: &PreparedCatalog,
    options: &RestorationOptions,
    progress: &dyn ProgressObserver,
) -> Result<RestorationReport, RestoreError> {
    let progress_plan = plan_restoration(catalog, progress)?;
    let storage_plan = preflight_restoration_storage(
        catalog,
        &progress_plan,
        &options.output_directory,
        progress,
    )?;
    create_owner_only_directory(&options.output_directory)?;
    let output_directory = fs::canonicalize(&options.output_directory)?;
    let messages_path = output_directory.join("messages.ndjson");
    let rejections_path = output_directory.join("rejections.ndjson");
    let artifacts_path = output_directory.join("artifacts.ndjson");
    let coverage_path = output_directory.join("coverage.json");
    let report_path = output_directory.join("report.json");
    let mut rejections = owner_only_writer(&rejections_path)?;
    let staging = RestorationStaging::create(&output_directory)?;
    let mut staging_storage = StagingStorageStats::default();
    let mut artifact_resolver = ArtifactResolver::new(
        catalog,
        options.account_root.as_deref(),
        &output_directory,
        options.defer_media,
    )?;
    let account_binding = resolve_account_binding(catalog, options.account_root.as_deref())?;
    let account_id = account_binding.account_id.clone();
    let mut integrity = RestorationIntegrity {
        database_count: catalog.databases.len() as u64,
        observed_table_row_count: progress_plan.total_observed_table_rows,
        ..Default::default()
    };
    let mut table_coverage = Vec::new();
    let mut all_table_coverage = Vec::new();
    let mut entity_seeds = EntitySeeds::default();
    let mut overall_processed_rows = 0_u64;

    for (database_index, database) in catalog.databases.iter().enumerate() {
        let database_plan = progress_plan
            .databases
            .get(&database.source_set_id)
            .ok_or_else(|| {
                RestoreError::Integrity("record progress plan lost a prepared database".to_string())
            })?;
        let database_started = Instant::now();
        let database_rows_before = overall_processed_rows;
        let mut database_start = restoration_database_event(
            ProgressPhase::RecordRestoration,
            ProgressState::Started,
            "restoreDatabaseRecords",
            0,
            database_plan.message_rows,
            overall_processed_rows,
            progress_plan.total_message_rows,
            database_index,
            catalog.databases.len(),
            database,
        );
        database_start.table_count = Some(database.table_count);
        database_start.message_table_count = Some(database_plan.message_table_count);
        progress.observe(database_start);
        let connection =
            Connection::open_with_flags(&database.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.execute_batch("PRAGMA query_only = ON")?;
        let names = load_name_map(&connection).unwrap_or_default();

        for table in &database.tables {
            let columns = table_columns(&connection, table)?;
            let schema_fingerprint = table_schema_fingerprint(&connection, table)?;
            let table_id = opaque_id(table.as_bytes());
            let (role, classification_reason) = classify_table(table, &columns);
            *integrity
                .table_role_counts
                .entry(table_role_name(role).to_string())
                .or_default() += 1;
            *integrity
                .table_classification_reason_counts
                .entry(classification_reason.to_string())
                .or_default() += 1;
            if role == TableCoverageRole::UnhandledMessageCandidate {
                integrity.message_candidate_gap_count += 1;
            }
            all_table_coverage.push(TableSchemaCoverage {
                source_set_id: database.source_set_id.clone(),
                source_logical_path: database.logical_path.clone(),
                source_table_id: table_id.clone(),
                source_table_name: table.clone(),
                columns: columns.clone(),
                source_row_count: database_plan.table_rows.get(table).copied(),
                schema_fingerprint: Some(schema_fingerprint.clone()),
                role,
                classification_reason: classification_reason.to_string(),
            });
            if role != TableCoverageRole::Message {
                continue;
            }
            integrity.message_table_count += 1;
            let conversation = infer_conversation(table, &database.logical_path, names.values());
            *integrity
                .message_schema_counts
                .entry(schema_fingerprint.clone())
                .or_default() += 1;
            let quoted = quote_identifier(table);
            let count_sql = format!("SELECT count(*) FROM {quoted}");
            let row_count: i64 = connection.query_row(&count_sql, [], |row| row.get(0))?;
            let row_count = row_count.max(0) as u64;
            integrity.source_row_count += row_count;
            table_coverage.push(MessageTableCoverage {
                source_set_id: database.source_set_id.clone(),
                source_logical_path: database.logical_path.clone(),
                source_table_id: table_id.clone(),
                source_table_name: table.clone(),
                source_row_count: row_count,
                columns: columns.clone(),
                schema_fingerprint: Some(schema_fingerprint),
            });

            let select_sql = format!("SELECT rowid, * FROM {quoted} ORDER BY rowid");
            let mut statement = connection.prepare(&select_sql)?;
            let mut rows = statement.query([])?;
            let table_started = Instant::now();
            let mut table_processed_rows = 0_u64;
            let report_increment = (row_count / 100).max(1_000).max(1);
            let mut next_report = report_increment;
            let mut table_start = restoration_database_event(
                ProgressPhase::RecordRestoration,
                ProgressState::Started,
                "restoreMessageTable",
                0,
                row_count,
                overall_processed_rows,
                progress_plan.total_message_rows,
                database_index,
                catalog.databases.len(),
                database,
            );
            table_start.table_name = Some(table.clone());
            progress.observe(table_start);
            while let Some(row) = rows.next()? {
                let context = RowRestorationContext {
                    set_id: &database.source_set_id,
                    logical_path: &database.logical_path,
                    table_id: &table_id,
                    table_name: table,
                    account_id: &account_id,
                    conversation: &conversation,
                    names: &names,
                    self_source_identifier: account_binding.self_source_identifier.as_deref(),
                };
                match restore_row(row, &columns, &context) {
                    Ok(mut message) => {
                        if message.direction_evidence
                            == DirectionEvidence::SenderAccountConflictWithExplicitSourceColumn
                        {
                            integrity.direction_conflict_count += 1;
                        }
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
                        // The canonical NDJSON can be much larger than the
                        // source databases because it also preserves raw
                        // columns and typed projections. Compress only this
                        // private, ephemeral ordering spool so a restoration
                        // does not require enough free space for two complete
                        // uncompressed archive copies at once. The published
                        // message ledger remains ordinary lossless NDJSON.
                        let json = serde_json::to_vec(&message)?;
                        let compressed_json = compress_staging_payload(&json)?;
                        staging_storage.uncompressed_payload_byte_count = staging_storage
                            .uncompressed_payload_byte_count
                            .saturating_add(json.len() as u64);
                        staging_storage.compressed_payload_byte_count = staging_storage
                            .compressed_payload_byte_count
                            .saturating_add(compressed_json.len() as u64);
                        let inserted = staging.execute(
                            "INSERT OR IGNORE INTO staged_message(
                               canonical_id, conversation_id, sort_sequence, server_id,
                               created_at, local_id, source_logical_path, source_table_id,
                               source_row_id, message_json_zstd
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
                                compressed_json,
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
                table_processed_rows = table_processed_rows.saturating_add(1);
                overall_processed_rows = overall_processed_rows.saturating_add(1);
                if table_processed_rows >= next_report && table_processed_rows < row_count {
                    let mut event = restoration_database_event(
                        ProgressPhase::RecordRestoration,
                        ProgressState::Advanced,
                        "restoreMessageTable",
                        table_processed_rows,
                        row_count,
                        overall_processed_rows,
                        progress_plan.total_message_rows,
                        database_index,
                        catalog.databases.len(),
                        database,
                    );
                    event.table_name = Some(table.clone());
                    event.restored_record_count = Some(integrity.restored_row_count);
                    event.rejected_record_count = Some(integrity.rejected_row_count);
                    event.semantic_gap_count = Some(integrity.semantic_gap_count);
                    attach_staging_storage(
                        &mut event,
                        &storage_plan,
                        &mut staging_storage,
                        &staging,
                        &output_directory,
                        overall_processed_rows,
                    )?;
                    progress.observe(event);
                    next_report = table_processed_rows.saturating_add(report_increment);
                }
            }
            let mut table_finished = restoration_database_event(
                ProgressPhase::RecordRestoration,
                ProgressState::Completed,
                "restoreMessageTable",
                table_processed_rows,
                row_count,
                overall_processed_rows,
                progress_plan.total_message_rows,
                database_index,
                catalog.databases.len(),
                database,
            );
            table_finished.table_name = Some(table.clone());
            table_finished.restored_record_count = Some(integrity.restored_row_count);
            table_finished.rejected_record_count = Some(integrity.rejected_row_count);
            table_finished.semantic_gap_count = Some(integrity.semantic_gap_count);
            table_finished.elapsed_milliseconds = Some(elapsed_milliseconds(table_started));
            attach_staging_storage(
                &mut table_finished,
                &storage_plan,
                &mut staging_storage,
                &staging,
                &output_directory,
                overall_processed_rows,
            )?;
            progress.observe(table_finished);
        }
        let mut database_finished = restoration_database_event(
            ProgressPhase::RecordRestoration,
            ProgressState::Completed,
            "restoreDatabaseRecords",
            overall_processed_rows.saturating_sub(database_rows_before),
            database_plan.message_rows,
            overall_processed_rows,
            progress_plan.total_message_rows,
            database_index,
            catalog.databases.len(),
            database,
        );
        database_finished.table_count = Some(database.table_count);
        database_finished.message_table_count = Some(database_plan.message_table_count);
        database_finished.restored_record_count = Some(integrity.restored_row_count);
        database_finished.rejected_record_count = Some(integrity.rejected_row_count);
        database_finished.semantic_gap_count = Some(integrity.semantic_gap_count);
        database_finished.elapsed_milliseconds = Some(elapsed_milliseconds(database_started));
        attach_staging_storage(
            &mut database_finished,
            &storage_plan,
            &mut staging_storage,
            &staging,
            &output_directory,
            overall_processed_rows,
        )?;
        progress.observe(database_finished);
    }
    rejections.flush()?;

    if !integrity.row_equation_holds() {
        return Err(RestoreError::Manifest(
            "restoration row equation failed".to_string(),
        ));
    }

    let finalization_clock = Instant::now();
    let artifact_total = artifact_resolver.artifacts().count() as u64;
    let record_work = integrity.restored_row_count.saturating_add(artifact_total);
    // Reserve visible phase progress for entity reconstruction, cached-surface
    // finalization, coverage metadata, and the archive report. A one-item tail
    // after hundreds of thousands of records rounds to 100.0% too early in
    // human output even though meaningful work remains.
    let fixed_stage_work = record_work
        .saturating_add(progress_plan.total_cached_surface_rows)
        .div_ceil(100)
        .max(1);
    let cached_surface_work = progress_plan
        .total_cached_surface_rows
        .max(fixed_stage_work);
    let finalization_total = record_work
        .saturating_add(cached_surface_work)
        .saturating_add(fixed_stage_work.saturating_mul(3));
    let mut finalization_started = archive_finalization_event(
        ProgressState::Started,
        "orderLinkAndWriteArchive",
        ProgressUnit::Items,
        0,
        finalization_total,
        0,
        finalization_total,
        catalog,
        &progress_plan,
        &integrity,
    );
    let published_before_messages = archive_file_byte_count(&output_directory)?;
    attach_finalization_storage(
        &mut finalization_started,
        &storage_plan,
        &mut staging_storage,
        &staging,
        &output_directory,
        published_before_messages,
    )?;
    progress.observe(finalization_started);
    let mut message_finalization_started = archive_finalization_event(
        ProgressState::Started,
        "sortAndWriteMessages",
        ProgressUnit::Records,
        0,
        integrity.restored_row_count,
        0,
        finalization_total,
        catalog,
        &progress_plan,
        &integrity,
    );
    attach_finalization_storage(
        &mut message_finalization_started,
        &storage_plan,
        &mut staging_storage,
        &staging,
        &output_directory,
        published_before_messages,
    )?;
    progress.observe(message_finalization_started);

    let mut messages = ByteCountingWriter::new(owner_only_writer(&messages_path)?);
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
         SELECT message_json_zstd, conversation_basis.basis
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
    let mut finalized_messages = 0_u64;
    let mut message_progress = ProgressThrottle::new(integrity.restored_row_count);
    while let Some(row) = rows.next()? {
        let compressed_bytes: Vec<u8> = row.get(0)?;
        let basis: i64 = row.get(1)?;
        let bytes = decompress_staging_payload(&compressed_bytes)?;
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
        finalized_messages = finalized_messages.saturating_add(1);
        if message_progress.should_emit(finalized_messages) {
            let mut event = archive_finalization_event(
                ProgressState::Advanced,
                "sortAndWriteMessages",
                ProgressUnit::Records,
                finalized_messages,
                integrity.restored_row_count,
                finalized_messages,
                finalization_total,
                catalog,
                &progress_plan,
                &integrity,
            );
            attach_finalization_storage(
                &mut event,
                &storage_plan,
                &mut staging_storage,
                &staging,
                &output_directory,
                published_before_messages.saturating_add(messages.byte_count()),
            )?;
            progress.observe(event);
        }
    }
    messages.flush()?;
    let mut messages_finished = archive_finalization_event(
        ProgressState::Completed,
        "sortAndWriteMessages",
        ProgressUnit::Records,
        finalized_messages,
        integrity.restored_row_count,
        finalized_messages,
        finalization_total,
        catalog,
        &progress_plan,
        &integrity,
    );
    let published_after_messages = archive_file_byte_count(&output_directory)?;
    attach_finalization_storage(
        &mut messages_finished,
        &storage_plan,
        &mut staging_storage,
        &staging,
        &output_directory,
        published_after_messages,
    )?;
    progress.observe(messages_finished);

    let mut artifacts = ByteCountingWriter::new(owner_only_writer(&artifacts_path)?);
    let mut artifacts_started = archive_finalization_event(
        ProgressState::Started,
        "writeArtifactIndex",
        ProgressUnit::Records,
        0,
        artifact_total,
        finalized_messages,
        finalization_total,
        catalog,
        &progress_plan,
        &integrity,
    );
    attach_finalization_storage(
        &mut artifacts_started,
        &storage_plan,
        &mut staging_storage,
        &staging,
        &output_directory,
        published_after_messages,
    )?;
    progress.observe(artifacts_started);
    let mut finalized_artifacts = 0_u64;
    let mut artifact_progress = ProgressThrottle::new(artifact_total);
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
        finalized_artifacts = finalized_artifacts.saturating_add(1);
        if artifact_progress.should_emit(finalized_artifacts) {
            let mut event = archive_finalization_event(
                ProgressState::Advanced,
                "writeArtifactIndex",
                ProgressUnit::Records,
                finalized_artifacts,
                artifact_total,
                finalized_messages.saturating_add(finalized_artifacts),
                finalization_total,
                catalog,
                &progress_plan,
                &integrity,
            );
            attach_finalization_storage(
                &mut event,
                &storage_plan,
                &mut staging_storage,
                &staging,
                &output_directory,
                published_after_messages.saturating_add(artifacts.byte_count()),
            )?;
            progress.observe(event);
        }
    }
    artifacts.flush()?;
    let finalized_records = finalized_messages.saturating_add(finalized_artifacts);
    let mut artifacts_finished = archive_finalization_event(
        ProgressState::Completed,
        "writeArtifactIndex",
        ProgressUnit::Records,
        finalized_artifacts,
        artifact_total,
        finalized_records,
        finalization_total,
        catalog,
        &progress_plan,
        &integrity,
    );
    let published_after_artifacts = archive_file_byte_count(&output_directory)?;
    attach_finalization_storage(
        &mut artifacts_finished,
        &storage_plan,
        &mut staging_storage,
        &staging,
        &output_directory,
        published_after_artifacts,
    )?;
    progress.observe(artifacts_finished);

    let mut entities_started = archive_finalization_event(
        ProgressState::Started,
        "restoreEntityIndexes",
        ProgressUnit::Items,
        0,
        1,
        finalized_records,
        finalization_total,
        catalog,
        &progress_plan,
        &integrity,
    );
    attach_finalization_storage(
        &mut entities_started,
        &storage_plan,
        &mut staging_storage,
        &staging,
        &output_directory,
        published_after_artifacts,
    )?;
    progress.observe(entities_started);
    let entity_result = restore_entities(catalog, &account_id, entity_seeds, &output_directory)?;
    integrity.conversation_count = entity_result.conversation_count;
    integrity.participant_count = entity_result.participant_count;
    integrity.group_member_count = entity_result.group_member_count;
    integrity.entity_source_row_count = entity_result.source_row_count;
    integrity.entity_decode_gap_count = entity_result.decode_gap_count;
    integrity.missing_local_profile_count = entity_result.missing_local_profile_count;
    integrity.unresolved_conversation_count = entity_result.unresolved_conversation_count;
    let mut entities_finished = archive_finalization_event(
        ProgressState::Completed,
        "restoreEntityIndexes",
        ProgressUnit::Items,
        1,
        1,
        finalized_records.saturating_add(fixed_stage_work),
        finalization_total,
        catalog,
        &progress_plan,
        &integrity,
    );
    let published_after_entities = archive_file_byte_count(&output_directory)?;
    attach_finalization_storage(
        &mut entities_finished,
        &storage_plan,
        &mut staging_storage,
        &staging,
        &output_directory,
        published_after_entities,
    )?;
    progress.observe(entities_finished);
    let cached_phase_start = finalized_records.saturating_add(fixed_stage_work);
    let cached_surfaces = restore_cached_surfaces_with_progress(
        catalog,
        &account_id,
        &output_directory,
        progress,
        cached_phase_start,
        finalization_total,
        progress_plan.total_cached_surface_rows,
        cached_surface_work,
    )?;
    integrity.cached_moment_count = cached_surfaces.coverage.moment_count;
    integrity.cached_moment_interaction_count = cached_surfaces.coverage.interaction_count;
    integrity.cached_surface_semantic_gap_count = cached_surfaces.coverage.semantic_gap_count;
    let cached_phase_end = cached_phase_start.saturating_add(cached_surface_work);
    let published_after_cached_surfaces = archive_file_byte_count(&output_directory)?;
    let cached_remaining =
        storage_plan.remaining_finalization_requirement(cached_phase_end, finalization_total);
    storage_plan.ensure_remaining_space(&output_directory, cached_remaining)?;

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
    let schema_profile_fingerprint =
        schema_profile_fingerprint(all_table_coverage.iter().map(|table| {
            (
                table.source_logical_path.as_str(),
                table.source_table_name.as_str(),
                table.schema_fingerprint.as_deref(),
            )
        }));
    let coverage = RestorationCoverage {
        format_version: 4,
        decoder_name: "greenbubbles-restore".to_string(),
        decoder_version: env!("CARGO_PKG_VERSION").to_string(),
        snapshot_manifest_format_version: catalog.manifest.manifest_format_version,
        schema_profile_fingerprint,
        message_tables: table_coverage,
        all_tables: all_table_coverage,
        logical_type_counts: integrity.logical_type_counts.clone(),
        logical_sub_type_counts: integrity.logical_sub_type_counts.clone(),
        unknown_payload_reason_counts: integrity.unknown_payload_reason_counts.clone(),
        semantic_gap_reason_counts: integrity.semantic_gap_reason_counts.clone(),
    };
    let mut coverage_started = archive_finalization_event(
        ProgressState::Started,
        "writeCoverageMetadata",
        ProgressUnit::Items,
        0,
        1,
        cached_phase_end,
        finalization_total,
        catalog,
        &progress_plan,
        &integrity,
    );
    attach_finalization_storage(
        &mut coverage_started,
        &storage_plan,
        &mut staging_storage,
        &staging,
        &output_directory,
        published_after_cached_surfaces,
    )?;
    progress.observe(coverage_started);
    write_owner_only_json(&coverage_path, &coverage)?;
    let mut coverage_finished = archive_finalization_event(
        ProgressState::Completed,
        "writeCoverageMetadata",
        ProgressUnit::Items,
        1,
        1,
        cached_phase_end.saturating_add(fixed_stage_work),
        finalization_total,
        catalog,
        &progress_plan,
        &integrity,
    );
    let published_after_coverage = archive_file_byte_count(&output_directory)?;
    attach_finalization_storage(
        &mut coverage_finished,
        &storage_plan,
        &mut staging_storage,
        &staging,
        &output_directory,
        published_after_coverage,
    )?;
    progress.observe(coverage_finished);

    let client_build_compatibility = catalog.manifest.client_build_compatibility();
    let database_coverage = restoration_database_coverage(catalog);
    let mut completion = RestorationCompletion::evaluate(&integrity);
    if options.defer_media {
        completion.full_restoration_achieved = false;
    }
    if catalog.manifest.manifest_format_version >= 2
        && !client_build_compatibility.production_compatible
    {
        completion.full_restoration_achieved = false;
    }
    let archive_scope =
        if catalog.diagnostic_batch.is_some() || catalog.diagnostic_available_selection {
            completion.full_restoration_achieved = false;
            crate::RestorationArchiveScope::DiagnosticSubset
        } else if catalog
            .manifest
            .acquisition
            .as_ref()
            .is_some_and(|acquisition| !acquisition.is_full_scan())
        {
            completion.full_restoration_achieved = false;
            crate::RestorationArchiveScope::IncrementalFragment
        } else if !database_coverage.authoritative_database_coverage {
            completion.full_restoration_achieved = false;
            crate::RestorationArchiveScope::PartialDatabaseCoverage
        } else {
            crate::RestorationArchiveScope::Authoritative
        };
    let mut report_started = archive_finalization_event(
        ProgressState::Started,
        "writeArchiveReport",
        ProgressUnit::Items,
        0,
        1,
        cached_phase_end.saturating_add(fixed_stage_work),
        finalization_total,
        catalog,
        &progress_plan,
        &integrity,
    );
    attach_finalization_storage(
        &mut report_started,
        &storage_plan,
        &mut staging_storage,
        &staging,
        &output_directory,
        published_after_coverage,
    )?;
    progress.observe(report_started);
    let mut report = RestorationReport {
        format_version: if account_binding.self_participant_id.is_some() {
            6
        } else {
            5
        },
        account_id,
        self_participant_id: account_binding.self_participant_id,
        account_binding_evidence: account_binding.evidence,
        storage: Some(storage_plan.evidence(staging_storage, 0)),
        source_fingerprint: catalog.manifest.source_fingerprint.clone(),
        client_build_compatibility,
        acquisition: catalog.manifest.acquisition.clone(),
        archive_scope,
        database_coverage: Some(database_coverage),
        media_phase: if options.defer_media {
            crate::RestorationMediaPhase::Deferred
        } else {
            crate::RestorationMediaPhase::Resolved
        },
        messages_path: messages_path.display().to_string(),
        rejections_path: rejections_path.display().to_string(),
        artifacts_path: artifacts_path.display().to_string(),
        conversations_path: entity_result.conversations_path.display().to_string(),
        participants_path: entity_result.participants_path.display().to_string(),
        cached_moments_path: Some(cached_surfaces.moments_path.display().to_string()),
        cached_moment_interactions_path: Some(
            cached_surfaces.interactions_path.display().to_string(),
        ),
        cached_surfaces_path: Some(cached_surfaces.coverage_path.display().to_string()),
        coverage_path: coverage_path.display().to_string(),
        report_path: report_path.display().to_string(),
        integrity,
        completion,
    };
    let actual_archive_byte_count =
        write_report_with_exact_archive_size(&report_path, &mut report, published_after_coverage)?;
    let mut report_finished = archive_finalization_event(
        ProgressState::Completed,
        "writeArchiveReport",
        ProgressUnit::Items,
        1,
        1,
        finalization_total,
        finalization_total,
        catalog,
        &progress_plan,
        &report.integrity,
    );
    attach_finalization_storage(
        &mut report_finished,
        &storage_plan,
        &mut staging_storage,
        &staging,
        &output_directory,
        actual_archive_byte_count,
    )?;
    progress.observe(report_finished);
    let mut final_event = ProgressEvent::new(
        ProgressPhase::ArchiveFinalization,
        ProgressState::Completed,
        "finalizeArchive",
        ProgressUnit::Items,
        finalization_total,
        finalization_total,
        finalization_total,
        finalization_total,
    );
    final_event.database_count = Some(catalog.databases.len());
    final_event.table_count = Some(progress_plan.total_table_count);
    final_event.message_table_count = Some(progress_plan.total_message_table_count);
    final_event.restored_record_count = Some(report.integrity.restored_row_count);
    final_event.rejected_record_count = Some(report.integrity.rejected_row_count);
    final_event.semantic_gap_count = Some(report.integrity.semantic_gap_count);
    final_event.elapsed_milliseconds = Some(elapsed_milliseconds(finalization_clock));
    attach_finalization_storage(
        &mut final_event,
        &storage_plan,
        &mut staging_storage,
        &staging,
        &output_directory,
        actual_archive_byte_count,
    )?;
    progress.observe(final_event);
    Ok(report)
}

fn restoration_database_coverage(catalog: &PreparedCatalog) -> RestorationDatabaseCoverage {
    let snapshot_source_set_ids = catalog
        .manifest
        .acquisition
        .as_ref()
        .map(|acquisition| {
            acquisition
                .source_sets
                .iter()
                .filter(|source_set| {
                    source_set
                        .files
                        .iter()
                        .any(|file| file.role == SnapshotFileRole::Database)
                })
                .map(|source_set| source_set.source_set_id.clone())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_else(|| {
            catalog
                .manifest
                .database_entries()
                .map(|entry| entry.source_set_id.clone())
                .collect()
        });
    let fresh_source_set_ids = catalog
        .databases
        .iter()
        .map(|database| database.source_set_id.clone())
        .collect::<BTreeSet<_>>();
    let unavailable_source_set_ids = catalog
        .available_database_selection
        .as_ref()
        .map(|selection| {
            selection
                .unavailable_databases
                .iter()
                .map(|database| database.source_set_id.clone())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let unavailable_databases = catalog
        .available_database_selection
        .as_ref()
        .map(|selection| {
            selection
                .unavailable_databases
                .iter()
                .map(|database| RestorationUnavailableDatabase {
                    source_set_id: database.source_set_id.clone(),
                    logical_path: database.logical_path.clone(),
                    storage_family: match database.storage_family {
                        crate::StorageFamily::SQLite => "sqlite",
                        crate::StorageFamily::WcdbSqlcipher4 => "wcdbSqlcipher4",
                    }
                    .to_string(),
                    database_byte_count: database.database_byte_count,
                    write_ahead_log_byte_count: database.write_ahead_log_byte_count,
                    reason: database.reason.clone(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let attempted_source_set_ids = fresh_source_set_ids
        .union(&unavailable_source_set_ids)
        .cloned()
        .collect::<BTreeSet<_>>();
    let authoritative_database_coverage = attempted_source_set_ids == snapshot_source_set_ids
        && unavailable_source_set_ids.is_empty();
    RestorationDatabaseCoverage {
        format_version: 1,
        total_database_count: snapshot_source_set_ids.len(),
        attempted_database_count: attempted_source_set_ids.len(),
        restored_database_count: fresh_source_set_ids.len(),
        unavailable_database_count: unavailable_source_set_ids.len(),
        preserved_stale_database_count: 0,
        authoritative_database_coverage,
        snapshot_source_set_ids: snapshot_source_set_ids.into_iter().collect(),
        attempted_source_set_ids: attempted_source_set_ids.into_iter().collect(),
        fresh_source_set_ids: fresh_source_set_ids.into_iter().collect(),
        unavailable_source_set_ids: unavailable_source_set_ids.into_iter().collect(),
        preserved_stale_source_set_ids: Vec::new(),
        unavailable_databases,
    }
}

struct RestorationProgressPlan {
    databases: HashMap<String, DatabaseProgressPlan>,
    total_table_count: usize,
    total_message_table_count: u64,
    total_message_rows: u64,
    total_observed_table_rows: u64,
    total_cached_surface_rows: u64,
}

impl RestorationStoragePlan {
    fn estimate(catalog: &PreparedCatalog, plan: &RestorationProgressPlan) -> Self {
        let source_byte_count = catalog.databases.iter().fold(0_u64, |total, database| {
            total
                .saturating_add(database.database_byte_count)
                .saturating_add(database.write_ahead_log_byte_count)
        });
        let other_record_count = plan
            .total_observed_table_rows
            .saturating_sub(plan.total_message_rows);
        let estimated_archive_byte_count = source_byte_count
            .saturating_mul(ARCHIVE_SOURCE_BYTE_MULTIPLIER)
            .saturating_add(
                plan.total_message_rows
                    .saturating_mul(ARCHIVE_MESSAGE_RECORD_OVERHEAD),
            )
            .saturating_add(other_record_count.saturating_mul(ARCHIVE_OTHER_RECORD_OVERHEAD))
            .saturating_add(ARCHIVE_FIXED_OVERHEAD);
        let estimated_staging_byte_count = source_byte_count
            .saturating_mul(STAGING_SOURCE_BYTE_MULTIPLIER)
            .saturating_add(
                plan.total_message_rows
                    .saturating_mul(STAGING_MESSAGE_RECORD_OVERHEAD),
            )
            .saturating_add(STAGING_FIXED_OVERHEAD);
        let estimated_peak_byte_count =
            estimated_archive_byte_count.saturating_add(estimated_staging_byte_count);
        let reserve_byte_count = estimated_peak_byte_count
            .div_ceil(10)
            .max(MINIMUM_FREE_SPACE_RESERVE);
        let required_free_byte_count = estimated_peak_byte_count.saturating_add(reserve_byte_count);
        Self {
            source_byte_count,
            message_record_count: plan.total_message_rows,
            observed_table_record_count: plan.total_observed_table_rows,
            estimated_archive_byte_count,
            estimated_staging_byte_count,
            estimated_peak_byte_count,
            reserve_byte_count,
            required_free_byte_count,
            available_free_byte_count_at_start: 0,
        }
    }

    fn remaining_staging_requirement(&self, completed_records: u64) -> u64 {
        let remaining_records = self
            .message_record_count
            .saturating_sub(completed_records.min(self.message_record_count));
        let remaining_staging = if self.message_record_count == 0 {
            0
        } else {
            u64::try_from(
                self.estimated_staging_byte_count as u128 * remaining_records as u128
                    / self.message_record_count as u128,
            )
            .unwrap_or(u64::MAX)
        };
        self.estimated_archive_byte_count
            .saturating_add(remaining_staging)
    }

    fn remaining_finalization_requirement(&self, phase_completed: u64, phase_total: u64) -> u64 {
        if phase_total == 0 {
            return 0;
        }
        let remaining = phase_total.saturating_sub(phase_completed.min(phase_total));
        u64::try_from(
            self.estimated_archive_byte_count as u128 * remaining as u128 / phase_total as u128,
        )
        .unwrap_or(u64::MAX)
    }

    fn ensure_remaining_space(
        &self,
        output_path: &Path,
        remaining_work_byte_count: u64,
    ) -> Result<(u64, u64), RestoreError> {
        let available = available_free_bytes(output_path)?;
        let required = if remaining_work_byte_count == 0 {
            0
        } else {
            remaining_work_byte_count.saturating_add(self.reserve_byte_count)
        };
        if available < required {
            return Err(RestoreError::InsufficientDiskSpace {
                available_byte_count: available,
                required_free_byte_count: required,
                estimated_peak_byte_count: self.estimated_peak_byte_count,
            });
        }
        Ok((available, required))
    }

    fn evidence(
        &self,
        staging: StagingStorageStats,
        actual_archive_byte_count: u64,
    ) -> RestorationStorageEvidence {
        RestorationStorageEvidence {
            format_version: 1,
            source_byte_count: self.source_byte_count,
            message_record_count: self.message_record_count,
            observed_table_record_count: self.observed_table_record_count,
            estimated_archive_byte_count: self.estimated_archive_byte_count,
            estimated_staging_byte_count: self.estimated_staging_byte_count,
            estimated_peak_byte_count: self.estimated_peak_byte_count,
            required_free_byte_count: self.required_free_byte_count,
            available_free_byte_count_at_start: self.available_free_byte_count_at_start,
            peak_staging_file_byte_count: staging.peak_file_byte_count,
            staged_uncompressed_byte_count: staging.uncompressed_payload_byte_count,
            staged_compressed_byte_count: staging.compressed_payload_byte_count,
            actual_archive_byte_count,
        }
    }
}

fn preflight_restoration_storage(
    catalog: &PreparedCatalog,
    plan: &RestorationProgressPlan,
    output_path: &Path,
    progress: &dyn ProgressObserver,
) -> Result<RestorationStoragePlan, RestoreError> {
    let mut storage = RestorationStoragePlan::estimate(catalog, plan);
    let available = available_free_bytes(output_path)?;
    storage.available_free_byte_count_at_start = available;
    let mut event = ProgressEvent::new(
        ProgressPhase::RecordPlanning,
        ProgressState::Planned,
        "preflightRestorationStorage",
        ProgressUnit::Bytes,
        available.min(storage.required_free_byte_count),
        storage.required_free_byte_count,
        plan.total_message_rows,
        plan.total_message_rows,
    );
    attach_storage_plan(
        &mut event,
        &storage,
        available,
        storage.required_free_byte_count,
    );
    event.database_count = Some(catalog.databases.len());
    event.table_count = Some(plan.total_table_count);
    event.message_table_count = Some(plan.total_message_table_count);
    event.source_record_count = Some(plan.total_message_rows);
    progress.observe(event);
    if available < storage.required_free_byte_count {
        return Err(RestoreError::InsufficientDiskSpace {
            available_byte_count: available,
            required_free_byte_count: storage.required_free_byte_count,
            estimated_peak_byte_count: storage.estimated_peak_byte_count,
        });
    }
    let mut completed = ProgressEvent::new(
        ProgressPhase::RecordPlanning,
        ProgressState::Completed,
        "preflightRestorationStorage",
        ProgressUnit::Bytes,
        storage.required_free_byte_count,
        storage.required_free_byte_count,
        plan.total_message_rows,
        plan.total_message_rows,
    );
    attach_storage_plan(
        &mut completed,
        &storage,
        available,
        storage.required_free_byte_count,
    );
    completed.database_count = Some(catalog.databases.len());
    completed.table_count = Some(plan.total_table_count);
    completed.message_table_count = Some(plan.total_message_table_count);
    completed.source_record_count = Some(plan.total_message_rows);
    progress.observe(completed);
    Ok(storage)
}

fn attach_storage_plan(
    event: &mut ProgressEvent,
    storage: &RestorationStoragePlan,
    available_free_byte_count: u64,
    required_free_byte_count: u64,
) {
    event.source_byte_count = Some(storage.source_byte_count);
    event.estimated_archive_byte_count = Some(storage.estimated_archive_byte_count);
    event.estimated_staging_byte_count = Some(storage.estimated_staging_byte_count);
    event.estimated_peak_byte_count = Some(storage.estimated_peak_byte_count);
    event.required_free_byte_count = Some(required_free_byte_count);
    event.available_free_byte_count = Some(available_free_byte_count);
}

fn attach_staging_storage(
    event: &mut ProgressEvent,
    storage: &RestorationStoragePlan,
    stats: &mut StagingStorageStats,
    staging: &RestorationStaging,
    output_path: &Path,
    completed_records: u64,
) -> Result<(), RestoreError> {
    let staging_bytes = staging.file_byte_count()?;
    stats.peak_file_byte_count = stats.peak_file_byte_count.max(staging_bytes);
    let (available, required) = storage.ensure_remaining_space(
        output_path,
        storage.remaining_staging_requirement(completed_records),
    )?;
    attach_storage_plan(event, storage, available, required);
    event.staging_file_byte_count = Some(staging_bytes);
    event.staged_uncompressed_byte_count = Some(stats.uncompressed_payload_byte_count);
    event.staged_compressed_byte_count = Some(stats.compressed_payload_byte_count);
    Ok(())
}

fn attach_finalization_storage(
    event: &mut ProgressEvent,
    storage: &RestorationStoragePlan,
    stats: &mut StagingStorageStats,
    staging: &RestorationStaging,
    output_path: &Path,
    published_archive_byte_count: u64,
) -> Result<(), RestoreError> {
    let staging_bytes = staging.file_byte_count()?;
    stats.peak_file_byte_count = stats.peak_file_byte_count.max(staging_bytes);
    let remaining =
        storage.remaining_finalization_requirement(event.phase_completed, event.phase_total);
    let (available, required) = storage.ensure_remaining_space(output_path, remaining)?;
    attach_storage_plan(event, storage, available, required);
    event.staging_file_byte_count = Some(staging_bytes);
    event.staged_uncompressed_byte_count = Some(stats.uncompressed_payload_byte_count);
    event.staged_compressed_byte_count = Some(stats.compressed_payload_byte_count);
    event.published_archive_byte_count = Some(published_archive_byte_count);
    Ok(())
}

fn available_free_bytes(path: &Path) -> Result<u64, RestoreError> {
    let probe_path = nearest_existing_ancestor(path)?;
    let path_bytes = probe_path.as_os_str().as_bytes();
    let c_path = CString::new(path_bytes).map_err(|_| {
        RestoreError::Integrity("restoration output path contains a NUL byte".to_string())
    })?;
    let mut statistics = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c_path` is NUL-terminated and `statistics` points to writable,
    // correctly aligned storage that is read only after statvfs succeeds.
    if unsafe { libc::statvfs(c_path.as_ptr(), statistics.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: the successful statvfs call initialized the complete structure.
    let statistics = unsafe { statistics.assume_init() };
    let bytes = u128::from(statistics.f_bavail).saturating_mul(u128::from(statistics.f_frsize));
    Ok(u64::try_from(bytes).unwrap_or(u64::MAX))
}

fn nearest_existing_ancestor(path: &Path) -> Result<PathBuf, RestoreError> {
    let mut candidate = if path.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        path.to_path_buf()
    };
    while fs::symlink_metadata(&candidate).is_err() {
        if !candidate.pop() {
            candidate = PathBuf::from(".");
            break;
        }
    }
    Ok(fs::canonicalize(candidate)?)
}

fn compress_staging_payload(payload: &[u8]) -> Result<Vec<u8>, RestoreError> {
    Ok(zstd::stream::encode_all(
        payload,
        STAGING_COMPRESSION_LEVEL,
    )?)
}

fn decompress_staging_payload(payload: &[u8]) -> Result<Vec<u8>, RestoreError> {
    Ok(zstd::stream::decode_all(payload)?)
}

fn archive_file_byte_count(output_path: &Path) -> Result<u64, RestoreError> {
    let mut total = 0_u64;
    let entries = WalkDir::new(output_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            !(entry.depth() == 1
                && entry.file_type().is_dir()
                && entry.file_name().as_bytes().starts_with(b".staging-"))
        });
    for entry in entries {
        let entry = entry.map_err(|error| {
            RestoreError::Io(
                error
                    .into_io_error()
                    .unwrap_or_else(|| std::io::Error::other("could not inspect archive size")),
            )
        })?;
        if entry.file_type().is_symlink() {
            return Err(RestoreError::Integrity(
                "restoration output contains a symbolic link".to_string(),
            ));
        }
        if entry.file_type().is_file() {
            let metadata = entry.metadata().map_err(|error| {
                RestoreError::Io(error.into_io_error().unwrap_or_else(|| {
                    std::io::Error::other("could not inspect archive file size")
                }))
            })?;
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

struct DatabaseProgressPlan {
    message_table_count: u64,
    message_rows: u64,
    table_rows: HashMap<String, u64>,
}

#[allow(clippy::too_many_arguments)]
fn archive_finalization_event(
    state: ProgressState,
    operation: &str,
    unit: ProgressUnit,
    completed: u64,
    total: u64,
    phase_completed: u64,
    phase_total: u64,
    catalog: &PreparedCatalog,
    plan: &RestorationProgressPlan,
    integrity: &RestorationIntegrity,
) -> ProgressEvent {
    let mut event = ProgressEvent::new(
        ProgressPhase::ArchiveFinalization,
        state,
        operation,
        unit,
        completed,
        total,
        phase_completed,
        phase_total,
    );
    event.database_count = Some(catalog.databases.len());
    event.table_count = Some(plan.total_table_count);
    event.message_table_count = Some(plan.total_message_table_count);
    event.restored_record_count = Some(integrity.restored_row_count);
    event.rejected_record_count = Some(integrity.rejected_row_count);
    event.semantic_gap_count = Some(integrity.semantic_gap_count);
    event
}

struct ProgressThrottle {
    next_record: u64,
    record_increment: u64,
    last_report: Instant,
}

impl ProgressThrottle {
    fn new(total: u64) -> Self {
        let record_increment = (total / 100).max(10_000).max(1);
        Self {
            next_record: record_increment,
            record_increment,
            last_report: Instant::now(),
        }
    }

    fn should_emit(&mut self, completed: u64) -> bool {
        if completed < self.next_record && self.last_report.elapsed() < Duration::from_millis(500) {
            return false;
        }
        self.next_record = completed.saturating_add(self.record_increment);
        self.last_report = Instant::now();
        true
    }
}

fn plan_restoration(
    catalog: &PreparedCatalog,
    progress: &dyn ProgressObserver,
) -> Result<RestorationProgressPlan, RestoreError> {
    let started = Instant::now();
    let mut databases = HashMap::new();
    let mut total_table_count = 0_usize;
    let mut total_message_table_count = 0_u64;
    let mut total_message_rows = 0_u64;
    let mut total_observed_table_rows = 0_u64;
    let mut total_cached_surface_rows = 0_u64;
    let mut planned = ProgressEvent::new(
        ProgressPhase::RecordPlanning,
        ProgressState::Planned,
        "countMessageRecords",
        ProgressUnit::Items,
        0,
        catalog.databases.len() as u64,
        0,
        catalog.databases.len() as u64,
    );
    planned.database_count = Some(catalog.databases.len());
    progress.observe(planned);

    for (database_index, database) in catalog.databases.iter().enumerate() {
        let database_started = Instant::now();
        let mut start_event = restoration_database_event(
            ProgressPhase::RecordPlanning,
            ProgressState::Started,
            "countDatabaseRecords",
            0,
            database.table_count as u64,
            database_index as u64,
            catalog.databases.len() as u64,
            database_index,
            catalog.databases.len(),
            database,
        );
        start_event.table_count = Some(database.table_count);
        progress.observe(start_event);

        let connection =
            Connection::open_with_flags(&database.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.execute_batch("PRAGMA query_only = ON")?;
        let mut message_table_count = 0_u64;
        let mut message_rows = 0_u64;
        let mut table_rows = HashMap::new();
        for (table_index, table) in database.tables.iter().enumerate() {
            let mut table_started = restoration_database_event(
                ProgressPhase::RecordPlanning,
                ProgressState::Started,
                "inspectTable",
                table_index as u64,
                database.table_count as u64,
                database_index as u64,
                catalog.databases.len() as u64,
                database_index,
                catalog.databases.len(),
                database,
            );
            table_started.table_name = Some(table.clone());
            table_started.table_count = Some(database.table_count);
            progress.observe(table_started);
            let columns = table_columns(&connection, table)?;
            let schema_fingerprint = table_schema_fingerprint(&connection, table)?;
            let (role, _) = classify_table(table, &columns);
            let sql = format!("SELECT count(*) FROM {}", quote_identifier(table));
            let count: i64 = connection.query_row(&sql, [], |row| row.get(0))?;
            let source_rows = count.max(0) as u64;
            total_observed_table_rows = total_observed_table_rows.saturating_add(source_rows);
            if crate::cached::is_sns_database_path(&database.logical_path)
                && matches!(
                    crate::cached::classify_table(table, &columns).0,
                    crate::CachedSurfaceTableRole::MomentTimeline
                        | crate::CachedSurfaceTableRole::MomentInteraction
                )
            {
                total_cached_surface_rows = total_cached_surface_rows.saturating_add(source_rows);
            }
            table_rows.insert(table.clone(), source_rows);
            if role == TableCoverageRole::Message {
                message_table_count = message_table_count.saturating_add(1);
                message_rows = message_rows.saturating_add(source_rows);
            }
            let mut table_finished = restoration_database_event(
                ProgressPhase::RecordPlanning,
                ProgressState::Completed,
                "inspectTable",
                table_index as u64 + 1,
                database.table_count as u64,
                database_index as u64,
                catalog.databases.len() as u64,
                database_index,
                catalog.databases.len(),
                database,
            );
            table_finished.table_name = Some(table.clone());
            table_finished.table_role = Some(table_role_name(role).to_string());
            table_finished.table_columns = Some(columns);
            table_finished.table_schema_fingerprint = Some(schema_fingerprint);
            table_finished.table_count = Some(database.table_count);
            table_finished.source_record_count = Some(source_rows);
            progress.observe(table_finished);
        }
        total_table_count = total_table_count.saturating_add(database.table_count);
        total_message_table_count = total_message_table_count.saturating_add(message_table_count);
        total_message_rows = total_message_rows.saturating_add(message_rows);
        databases.insert(
            database.source_set_id.clone(),
            DatabaseProgressPlan {
                message_table_count,
                message_rows,
                table_rows,
            },
        );

        let mut finished = restoration_database_event(
            ProgressPhase::RecordPlanning,
            ProgressState::Completed,
            "countDatabaseRecords",
            database.table_count as u64,
            database.table_count as u64,
            database_index as u64 + 1,
            catalog.databases.len() as u64,
            database_index,
            catalog.databases.len(),
            database,
        );
        finished.table_count = Some(database.table_count);
        finished.message_table_count = Some(message_table_count);
        finished.source_record_count = Some(message_rows);
        finished.elapsed_milliseconds = Some(elapsed_milliseconds(database_started));
        progress.observe(finished);
    }

    let mut finished = ProgressEvent::new(
        ProgressPhase::RecordPlanning,
        ProgressState::Completed,
        "countMessageRecords",
        ProgressUnit::Records,
        total_message_rows,
        total_message_rows,
        total_message_rows,
        total_message_rows,
    );
    finished.database_count = Some(catalog.databases.len());
    finished.table_count = Some(total_table_count);
    finished.message_table_count = Some(total_message_table_count);
    finished.elapsed_milliseconds = Some(elapsed_milliseconds(started));
    progress.observe(finished);
    Ok(RestorationProgressPlan {
        databases,
        total_table_count,
        total_message_table_count,
        total_message_rows,
        total_observed_table_rows,
        total_cached_surface_rows,
    })
}

#[allow(clippy::too_many_arguments)]
fn restoration_database_event(
    phase: ProgressPhase,
    state: ProgressState,
    operation: &str,
    completed: u64,
    total: u64,
    overall_completed: u64,
    overall_total: u64,
    database_index: usize,
    database_count: usize,
    database: &crate::PreparedDatabase,
) -> ProgressEvent {
    let mut event = ProgressEvent::new(
        phase,
        state,
        operation,
        ProgressUnit::Records,
        completed,
        total,
        overall_completed,
        overall_total,
    );
    event.database_index = Some(database_index + 1);
    event.database_count = Some(database_count);
    event.source_set_id = Some(database.source_set_id.clone());
    event.logical_path = Some(database.logical_path.clone());
    event.storage_family = Some(
        match database.storage_family {
            crate::StorageFamily::SQLite => "sqlite",
            crate::StorageFamily::WcdbSqlcipher4 => "wcdbSqlcipher4",
        }
        .to_string(),
    );
    event.database_byte_count = Some(database.database_byte_count);
    event.write_ahead_log_byte_count = Some(database.write_ahead_log_byte_count);
    event
}

fn table_role_name(role: TableCoverageRole) -> &'static str {
    match role {
        TableCoverageRole::Message => "message",
        TableCoverageRole::KnownAuxiliary => "knownAuxiliary",
        TableCoverageRole::Other => "other",
        TableCoverageRole::UnhandledMessageCandidate => "unhandledMessageCandidate",
    }
}

fn elapsed_milliseconds(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

struct RowRestorationContext<'a> {
    set_id: &'a str,
    logical_path: &'a str,
    table_id: &'a str,
    table_name: &'a str,
    account_id: &'a str,
    conversation: &'a str,
    names: &'a HashMap<i64, String>,
    self_source_identifier: Option<&'a str>,
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
            "dialogue_id",
            "session_id",
            "biz_username",
            "username",
            "user_name",
            "user_name_",
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
        "svrid",
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
        "type_",
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
        "timestamp_",
    ])
    .and_then(|index| get_i64(row, index));
    let status = field(&["status", "message_status"]).and_then(|index| get_i64(row, index));
    let explicit_sender_flag = field(&["is_sender", "is_sender_", "is_send", "is_sent_by_self"])
        .and_then(|index| get_i64(row, index));
    let content = field(&[
        "message_content",
        "msg_content",
        "content",
        "content_",
        "message_data",
        "msg_data",
        "card_wraplist_buffer",
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
        .filter(|value| !value.is_empty())
        .cloned()
        .or_else(|| {
            field(&["sender", "sender_name", "from_user", "from_username"])
                .and_then(|index| get_bytes(row, index))
                .filter(|value| !value.is_empty())
                .and_then(|value| String::from_utf8(value).ok())
        });
    let mut decoded_sender = fallback_sender.clone();
    // WeChat stores an optional `compress_content` column on every message
    // shard.  On current clients that column is frequently present as an
    // empty blob while `WCDB_CT_message_content = 4` marks the primary
    // `message_content` blob as zstd-compressed.  Passing an empty optional
    // blob through wx-db would make it win over the real content and produce
    // an empty XML projection.  Treat empty compression blobs as absent so
    // the decoder can apply the column compression marker to message_content.
    let effective_compressed = compressed.as_deref().filter(|value| !value.is_empty());
    let (typed_payload, semantic_decode_state, semantic_gap_reason) =
        if context.table_name.eq_ignore_ascii_case("FMessageTable") {
            decode_friend_contact_event(raw_type, content.as_deref())
        } else {
            match raw_type {
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
                    effective_compressed,
                    row_conversation.ends_with("@chatroom"),
                ) {
                    Ok(decoded) => {
                        if !decoded.sender.is_empty() {
                            decoded_sender = Some(decoded.sender);
                        }
                        match decoded.content {
                            wx_db::MessageContent::Unknown { msg_type, raw }
                                if matches!(msg_type, 35 | 42 | 50 | 66) =>
                            {
                                decode_legacy_message_type(msg_type, &raw)
                            }
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
                            known => match crate::nested_xml::serialize_message_content(&known) {
                                Ok((value, partial_reason)) => (
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
                            },
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
                None => missing_type_projection(),
            }
        };

    if decoded_sender.as_deref().is_none_or(str::is_empty)
        && logical_type == Some(49)
        && sub_type == Some(62)
    {
        decoded_sender = typed_payload_raw_xml(&typed_payload).and_then(|raw_xml| {
            crate::nested_xml::unique_identifier_element_text(raw_xml, "fromusername")
        });
    }
    // An empty source sender is absence, not an identity. Normalize once so
    // direction, the opaque sender ID, and source-preserving evidence cannot
    // disagree about whether the row has a sender.
    let decoded_sender = decoded_sender.filter(|value| !value.is_empty());
    let identity = format!("{}:{}:{source_row_id}", context.set_id, context.table_id);
    let relationships = extract_relationships(
        logical_type,
        sub_type,
        &typed_payload,
        content.as_deref(),
        effective_compressed,
    );
    let (direction, direction_evidence) = infer_direction(
        explicit_sender_flag,
        decoded_sender.as_deref(),
        context.self_source_identifier,
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

fn decode_friend_contact_event(
    raw_type: Option<i64>,
    content: Option<&[u8]>,
) -> (TypedPayload, SemanticDecodeState, Option<String>) {
    let Some(raw_type) = raw_type else {
        return missing_type_projection();
    };
    let (event_code, sub_type) = wx_db::split_local_type(raw_type);
    if sub_type != 0 || !matches!(event_code, 37 | 65) {
        let reason = format!("unsupported friend-contact event type {event_code}:{sub_type}");
        return (
            TypedPayload::Unknown {
                reason: reason.clone(),
            },
            SemanticDecodeState::UnknownType,
            Some(reason),
        );
    }
    let content_text = match content {
        Some(value) => match std::str::from_utf8(value) {
            Ok(value) => Some(value),
            Err(_) => {
                let reason =
                    format!("friend-contact event type {event_code} content is not valid UTF-8");
                return (
                    TypedPayload::Unknown {
                        reason: reason.clone(),
                    },
                    SemanticDecodeState::Failed,
                    Some(reason),
                );
            }
        },
        None => None,
    };
    (
        TypedPayload::Decoded(serde_json::json!({
            "FriendContactEvent": {
                "eventCode": event_code,
                "contentText": content_text,
            }
        })),
        SemanticDecodeState::Complete,
        None,
    )
}

fn missing_type_projection() -> (TypedPayload, SemanticDecodeState, Option<String>) {
    let reason = "local_type column is absent or null".to_string();
    (
        TypedPayload::Unknown {
            reason: reason.clone(),
        },
        SemanticDecodeState::MissingType,
        Some(reason),
    )
}

fn decode_legacy_message_type(
    message_type: u32,
    raw_xml: &str,
) -> (TypedPayload, SemanticDecodeState, Option<String>) {
    let (variant, expected_marker, label) = match message_type {
        35 => ("PushMail", "pushmail", "push-mail"),
        42 => ("ContactCard", "username", "contact-card"),
        50 => ("VoipCall", "voipmsg", "VoIP call"),
        // Type 66 is an older contact-card encoding.  Its payloads use the
        // same username/nickname envelope as type 42, including self-closing
        // `<msg .../>` forms, so they share the stable ContactCard shape while
        // retaining the raw logical type below.
        66 => ("ContactCard", "username", "contact-card"),
        _ => unreachable!("legacy decoder called for an unsupported type"),
    };
    let normalized = if message_type == 50 {
        crate::nested_xml::normalize_voip_xml_projection(raw_xml)
    } else {
        crate::nested_xml::normalize_xml_projection(raw_xml)
    };
    match normalized {
        Ok(normalized_xml) => {
            let marker_present =
                crate::nested_xml::xml_has_element_or_attribute(raw_xml, expected_marker)
                    || (message_type == 50
                        && crate::nested_xml::xml_has_element_or_attribute(
                            raw_xml,
                            "voipinvitemsg",
                        ));
            // `serde_json::json!({ variant: ... })` treats `variant` as a
            // literal property name.  Build the object explicitly so the
            // on-disk discriminator is actually `ContactCard`/`VoipCall`.
            let mut value = serde_json::Map::new();
            value.insert(
                variant.to_string(),
                serde_json::json!({
                    "format_version": 1,
                    "message_type": message_type,
                    "raw_xml": raw_xml,
                    "normalized_xml": normalized_xml,
                    "expected_marker_present": marker_present,
                }),
            );
            let payload = serde_json::Value::Object(value);
            if marker_present {
                (
                    TypedPayload::Decoded(payload),
                    SemanticDecodeState::Complete,
                    None,
                )
            } else {
                let reason = format!("{label} XML lacks the expected {expected_marker} element");
                (
                    TypedPayload::Decoded(payload),
                    SemanticDecodeState::Partial,
                    Some(reason),
                )
            }
        }
        Err(error) => {
            let payload = serde_json::json!({
                "LegacyRaw": {
                    "message_type": message_type,
                    "raw_xml": raw_xml,
                }
            });
            let reason = format!("{label} XML could not be normalized: {error}");
            (
                TypedPayload::Decoded(payload),
                SemanticDecodeState::Partial,
                Some(reason),
            )
        }
    }
}

fn extract_relationships(
    logical_type: Option<u32>,
    sub_type: Option<u32>,
    typed_payload: &TypedPayload,
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
    // The source message blob can be WCDB/zstd-compressed. The typed decoder
    // has already produced and retained the exact decoded XML, so relationship
    // identifiers must be read from that representation instead of searching
    // compressed bytes. Fall back to the original columns for legacy payloads
    // that do not expose raw XML. Keep `raw_reference_base64` bound to the
    // original source column so this semantic fix does not rewrite provenance.
    let decoded_xml = typed_payload_raw_xml(typed_payload).map(str::as_bytes);
    let source_raw = compressed.or(content).unwrap_or_default();
    let identifier_source = decoded_xml.unwrap_or(source_raw);
    let server_tags: &[&str] = match kind {
        MessageRelationshipKind::Recall => &["newmsgid", "svrid", "msgid"],
        _ => &["refermsgsvrid", "svrid", "newmsgid"],
    };
    let local_tags = &["refermsglocalid", "localid", "msglocalid"];
    vec![MessageRelationship {
        kind,
        target_canonical_id: None,
        target_server_id: extract_tagged_i64(identifier_source, server_tags),
        target_local_id: extract_tagged_i64(identifier_source, local_tags),
        resolved: false,
        resolution_state: RelationshipResolutionState::Pending,
        raw_reference_base64: (!source_raw.is_empty())
            .then(|| base64::engine::general_purpose::STANDARD.encode(source_raw)),
    }]
}

pub(crate) fn typed_payload_raw_xml(payload: &TypedPayload) -> Option<&str> {
    let TypedPayload::Decoded(value) = payload else {
        return None;
    };
    value.as_object()?.values().find_map(|variant| {
        variant
            .as_object()?
            .get("raw_xml")
            .and_then(serde_json::Value::as_str)
    })
}

fn infer_direction(
    explicit_sender_flag: Option<i64>,
    sender: Option<&str>,
    self_source_identifier: Option<&str>,
) -> (MessageDirection, DirectionEvidence) {
    let explicit_direction = explicit_sender_flag.map(|flag| {
        if flag == 0 {
            MessageDirection::Incoming
        } else {
            MessageDirection::Outgoing
        }
    });
    if let (Some(sender), Some(account)) = (
        sender.filter(|value| !value.is_empty()),
        self_source_identifier.filter(|value| !value.is_empty()),
    ) {
        let (direction, evidence) = if sender == account {
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
        return if explicit_direction.is_some_and(|explicit| explicit != direction) {
            (
                direction,
                DirectionEvidence::SenderAccountConflictWithExplicitSourceColumn,
            )
        } else {
            (direction, evidence)
        };
    }
    explicit_direction.map_or(
        (MessageDirection::Unknown, DirectionEvidence::Unresolved),
        |direction| (direction, DirectionEvidence::ExplicitSourceColumn),
    )
}

fn resolve_account_binding(
    catalog: &PreparedCatalog,
    account_root: Option<&Path>,
) -> Result<ResolvedAccountBinding, RestoreError> {
    if let Some(binding) = &catalog.manifest.account_binding {
        if let Some(root) = account_root {
            let canonical = fs::canonicalize(root)?;
            let observed_account_id = opaque_id(canonical.to_string_lossy().as_bytes());
            if observed_account_id != binding.account_id {
                return Err(RestoreError::Integrity(
                    "media account root belongs to a different snapshot account".to_string(),
                ));
            }
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&binding.self_source_identifier_base64)
            .map_err(|_| RestoreError::Manifest("invalid account-holder binding".to_string()))?;
        let self_source_identifier = String::from_utf8(decoded)
            .map_err(|_| RestoreError::Manifest("invalid account-holder binding".to_string()))?;
        let self_participant_id =
            scoped_opaque_id(&binding.account_id, self_source_identifier.as_bytes());
        return Ok(ResolvedAccountBinding {
            account_id: binding.account_id.clone(),
            self_source_identifier: Some(self_source_identifier),
            self_participant_id: Some(self_participant_id),
            evidence: Some(crate::AccountHolderBindingEvidence::SnapshotManifest),
        });
    }

    if let Some(root) = account_root {
        let canonical = fs::canonicalize(root)?;
        let account_id = opaque_id(canonical.to_string_lossy().as_bytes());
        let self_source_identifier = legacy_account_root_self_identifier(&canonical);
        let self_participant_id = self_source_identifier
            .as_ref()
            .map(|value| scoped_opaque_id(&account_id, value.as_bytes()));
        let evidence = legacy_account_binding_evidence(self_participant_id.as_deref());
        return Ok(ResolvedAccountBinding {
            account_id,
            self_source_identifier,
            self_participant_id,
            evidence,
        });
    }

    Ok(ResolvedAccountBinding {
        account_id: opaque_id(catalog.manifest.source_fingerprint.as_bytes()),
        self_source_identifier: None,
        self_participant_id: None,
        evidence: None,
    })
}

fn legacy_account_root_self_identifier(account_root: &Path) -> Option<String> {
    let directory_name = account_root.file_name()?.to_str()?;
    let conservative = wx_media::extract_wxid(directory_name);
    if conservative != directory_name {
        return Some(conservative);
    }
    let Some((candidate, suffix)) = directory_name.rsplit_once('_') else {
        return (!conservative.is_empty()).then_some(conservative);
    };
    let safe_suffix = suffix.len() == 4
        && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && !candidate.is_empty();
    if !safe_suffix || directory_name.starts_with("wxid_") {
        return (!conservative.is_empty()).then_some(conservative);
    }
    let Some(parent) = account_root.parent() else {
        return (!conservative.is_empty()).then_some(conservative);
    };
    let login_candidate = parent.join("all_users").join("login").join(candidate);
    let independently_confirmed = fs::symlink_metadata(login_candidate)
        .ok()
        .is_some_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
    Some(if independently_confirmed {
        candidate.to_string()
    } else {
        conservative
    })
}

fn legacy_account_binding_evidence(
    self_participant_id: Option<&str>,
) -> Option<crate::AccountHolderBindingEvidence> {
    self_participant_id.map(|_| crate::AccountHolderBindingEvidence::LegacyAccountRoot)
}

pub(crate) fn extract_tagged_i64(raw: &[u8], tags: &[&str]) -> Option<i64> {
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
    if lower == "fmessagetable" {
        return [
            "user_name_",
            "type_",
            "timestamp_",
            "content_",
            "is_sender_",
        ]
        .iter()
        .all(|column| column_index(columns, &[*column]).is_some());
    }
    if lower == "chatbot_message" {
        return ["svrid", "create_time", "dialogue_id", "direction", "type"]
            .iter()
            .all(|column| column_index(columns, &[*column]).is_some())
            && column_index(
                columns,
                &[
                    "card_wraplist_buffer",
                    "extra_info_buffer",
                    "bypass_info_buffer",
                ],
            )
            .is_some();
    }
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
    if is_known_auxiliary_table(name, columns) {
        return (
            TableCoverageRole::KnownAuxiliary,
            "matched a known entity, resource, index, or metadata table family",
        );
    }
    if is_message_table(name, columns) {
        return (
            TableCoverageRole::Message,
            "matched the supported message-table name or column signature",
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

fn is_known_auxiliary_table(name: &str, columns: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    let key_value_metadata = matches!(lower.as_str(), "buff" | "config" | "imgtableinfo")
        && column_index(columns, &["key"]).is_some()
        && column_index(
            columns,
            &["valueint64", "valuedouble", "valuestdstr", "valueblob"],
        )
        .is_some();
    // SNS tables are a separate cached-moments surface.  They may contain
    // `type`, `content`, and timestamp-like columns, but are not chat message
    // shards; the dedicated cached adapter restores them into its own ledgers.
    key_value_metadata
        || lower.starts_with("sns")
        || lower.starts_with("fav_")
        || lower.ends_with("name2id")
        || lower.starts_with("openim_")
        || lower.starts_with("search_dict_")
        || lower.starts_with("solitaire")
        || lower.starts_with("sessionunreadlisttable_")
        || lower.starts_with("sessionunreadstattable_")
        || lower.starts_with("wcdb_builtin_")
        || matches!(
            lower.as_str(),
            "name2id"
                | "biz_info"
                | "biz_pay_status"
                | "biz_subscribe_status"
                | "brand_search_record"
                | "sessiontable"
                | "sessiondeletetable"
                | "sessiondraft"
                | "sessionnocontactinfotable"
                | "contact"
                | "chat_room"
                | "chat_room_info_detail"
                | "chat_group"
                | "chatroom_member"
                | "contact_label"
                | "encrypt_name2id"
                | "my_user_info"
                | "user_info"
                | "table_info"
                | "db_info"
                | "dir2id"
                | "timestamp"
                | "head_image"
                | "chatbot_session"
                | "revokebatchmessage"
                | "messageresourceinfo"
                | "voiceinfo"
                | "deleteinfo"
                | "deleteresinfo"
                | "forwardrecent"
                | "grouppaytable"
                | "handoff_remind_v0"
                | "historysysmsginfo"
                | "historyaddmsginfo"
                | "ilink_voip"
                | "imgrangev0"
                | "messagegrouptimeinfo"
                | "new_tips"
                | "oplog"
                | "reddot"
                | "reddot_last_notify"
                | "reddot_record"
                | "redenvelopetable"
                | "searchrecent"
                | "sendinfo"
                | "stranger"
                | "stranger_ticket_info"
                | "teenager_apply_access_agree_info"
                | "ticket_info"
                | "transfertable"
                | "wacontact"
                | "wcfinderlivestatus"
                | "wcfinderuserpage"
                | "weappbizattrsyncbuffertablev02"
                | "websearch_record"
        )
        || [
            "index",
            "metadata",
            "_meta",
            "resource",
            "media",
            "emoticon",
            "sticker",
            "attachment",
            "download",
            "fts",
            "hardlink",
            "_checkpoint_",
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

fn write_report_with_exact_archive_size(
    path: &Path,
    report: &mut RestorationReport,
    archive_byte_count_before_report: u64,
) -> Result<u64, RestoreError> {
    for _ in 0..8 {
        let bytes = serde_json::to_vec_pretty(&report)?;
        let exact_archive_byte_count = archive_byte_count_before_report
            .saturating_add(bytes.len() as u64)
            .saturating_add(1);
        let storage = report.storage.as_mut().ok_or_else(|| {
            RestoreError::Integrity("restoration report lost storage evidence".to_string())
        })?;
        if storage.actual_archive_byte_count == exact_archive_byte_count {
            let mut writer = owner_only_writer(path)?;
            writer.write_all(&bytes)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
            return Ok(exact_archive_byte_count);
        }
        storage.actual_archive_byte_count = exact_archive_byte_count;
    }
    Err(RestoreError::Integrity(
        "restoration report size did not converge".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_staging_payload_round_trip_is_lossless() {
        let payload = serde_json::to_vec(&serde_json::json!({
            "text": "多语言 transcript with embedded \\u{0} bytes",
            "binary": base64::engine::general_purpose::STANDARD.encode((0_u8..=255).collect::<Vec<_>>()),
            "nested": [1, 2, 3, 4],
        }))
        .unwrap();
        let compressed = compress_staging_payload(&payload).unwrap();
        let restored = decompress_staging_payload(&compressed).unwrap();
        assert_eq!(restored, payload);
    }

    #[test]
    fn large_synthetic_spool_is_compressed_measured_and_ephemeral() {
        let fixture = tempfile::tempdir().unwrap();
        let output = fixture.path().join("archive");
        fs::create_dir(&output).unwrap();
        let retained_archive_file = output.join("messages.ndjson");
        fs::write(&retained_archive_file, b"retained archive sentinel").unwrap();
        let staging_directory;
        let mut uncompressed_byte_count = 0_u64;
        let mut compressed_byte_count = 0_u64;
        let staging_file_byte_count;
        {
            let staging = RestorationStaging::create(&output).unwrap();
            staging_directory = staging.path.parent().unwrap().to_path_buf();
            let repeated = "synthetic repetitive source evidence ".repeat(256);
            for ordinal in 0..2_000_i64 {
                let payload = serde_json::to_vec(&serde_json::json!({
                    "ordinal": ordinal,
                    "sourceEvidence": repeated,
                }))
                .unwrap();
                let compressed = compress_staging_payload(&payload).unwrap();
                uncompressed_byte_count =
                    uncompressed_byte_count.saturating_add(payload.len() as u64);
                compressed_byte_count =
                    compressed_byte_count.saturating_add(compressed.len() as u64);
                staging
                    .execute(
                        "INSERT INTO staged_message(
                           canonical_id, conversation_id, sort_sequence, server_id,
                           created_at, local_id, source_logical_path, source_table_id,
                           source_row_id, message_json_zstd
                         ) VALUES (?1, 'conversation', ?2, ?2, ?2, ?2, 'message.db',
                                   'table', ?2, ?3)",
                        rusqlite::params![format!("message-{ordinal}"), ordinal, compressed],
                    )
                    .unwrap();
            }
            staging_file_byte_count = staging.file_byte_count().unwrap();
            assert!(compressed_byte_count.saturating_mul(10) < uncompressed_byte_count);
            assert!(staging_file_byte_count < uncompressed_byte_count);

            let mut statement = staging
                .prepare(
                    "SELECT source_row_id, message_json_zstd
                     FROM staged_message ORDER BY source_row_id",
                )
                .unwrap();
            let mut rows = statement.query([]).unwrap();
            let mut observed = 0_i64;
            while let Some(row) = rows.next().unwrap() {
                let ordinal: i64 = row.get(0).unwrap();
                let compressed: Vec<u8> = row.get(1).unwrap();
                let payload = decompress_staging_payload(&compressed).unwrap();
                let value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                assert_eq!(value["ordinal"], ordinal);
                assert_eq!(value["sourceEvidence"], repeated);
                observed += 1;
            }
            assert_eq!(observed, 2_000);
        }
        assert!(!staging_directory.exists());
        assert_eq!(
            fs::read(&retained_archive_file).unwrap(),
            b"retained archive sentinel"
        );
        assert!(staging_file_byte_count > 0);
    }

    #[test]
    fn disk_guard_fails_before_creating_the_output_directory() {
        let fixture = tempfile::tempdir().unwrap();
        let output = fixture.path().join("not-created");
        let storage = RestorationStoragePlan {
            source_byte_count: 1,
            message_record_count: 1,
            observed_table_record_count: 1,
            estimated_archive_byte_count: u64::MAX,
            estimated_staging_byte_count: u64::MAX,
            estimated_peak_byte_count: u64::MAX,
            reserve_byte_count: 1,
            required_free_byte_count: u64::MAX,
            available_free_byte_count_at_start: 0,
        };
        assert!(matches!(
            storage.ensure_remaining_space(&output, u64::MAX),
            Err(RestoreError::InsufficientDiskSpace { .. })
        ));
        assert!(!output.exists());
    }

    #[test]
    fn bound_account_holder_deterministically_controls_message_direction() {
        assert_eq!(
            infer_direction(None, Some("wxid_self"), Some("wxid_self")),
            (
                MessageDirection::Outgoing,
                DirectionEvidence::SenderMatchesAccount
            )
        );
        assert_eq!(
            infer_direction(None, Some("wxid_other"), Some("wxid_self")),
            (
                MessageDirection::Incoming,
                DirectionEvidence::SenderDiffersFromAccount
            )
        );
        assert_eq!(
            infer_direction(Some(1), None, Some("wxid_self")),
            (
                MessageDirection::Outgoing,
                DirectionEvidence::ExplicitSourceColumn
            )
        );
        assert_eq!(
            infer_direction(Some(0), Some("wxid_self"), Some("wxid_self")),
            (
                MessageDirection::Outgoing,
                DirectionEvidence::SenderAccountConflictWithExplicitSourceColumn
            )
        );
    }

    #[test]
    fn legacy_account_root_alias_requires_independent_login_confirmation() {
        let fixture = tempfile::tempdir().unwrap();
        let account = fixture.path().join("legacyuser_1662");
        fs::create_dir(&account).unwrap();
        assert_eq!(
            legacy_account_root_self_identifier(&account).as_deref(),
            Some("legacyuser_1662")
        );
        fs::create_dir_all(fixture.path().join("all_users/login/legacyuser")).unwrap();
        assert_eq!(
            legacy_account_root_self_identifier(&account).as_deref(),
            Some("legacyuser")
        );
    }

    #[test]
    fn legacy_binding_evidence_requires_a_derived_account_holder() {
        assert_eq!(legacy_account_binding_evidence(None), None);
        assert_eq!(
            legacy_account_binding_evidence(Some("opaque-self-participant")),
            Some(crate::AccountHolderBindingEvidence::LegacyAccountRoot)
        );
    }

    #[test]
    fn bizchat_entity_tables_are_auxiliary_not_message_candidates() {
        for table in ["chat_group", "my_user_info", "name2id", "user_info"] {
            assert_eq!(
                classify_table(table, &["type".to_string(), "id".to_string()]).0,
                TableCoverageRole::KnownAuxiliary
            );
        }
    }

    #[test]
    fn full_text_search_tables_are_indexes_even_with_message_like_columns() {
        for table in [
            "fav_fts_v1",
            "fav_fts_v1_config",
            "fav_fts_v1_content",
            "fav_fts_v1_data",
            "fav_fts_v1_docsize",
            "fav_fts_v1_idx",
            "table_info",
            "db_info",
            "search_dict_v1",
        ] {
            assert_eq!(
                classify_table(
                    table,
                    &[
                        "type".to_string(),
                        "content".to_string(),
                        "create_time".to_string(),
                    ]
                )
                .0,
                TableCoverageRole::KnownAuxiliary
            );
        }
    }

    #[test]
    fn chatbot_schema_has_a_raw_preserving_message_adapter() {
        let message_columns = [
            "svrid",
            "create_time",
            "dialogue_id",
            "ui_state_id",
            "app_ui_state",
            "direction",
            "type",
            "trace_msgid",
            "card_wraplist_buffer",
            "extra_info_buffer",
            "bypass_info_buffer",
        ]
        .map(str::to_string);
        assert_eq!(
            classify_table("chatbot_message", &message_columns).0,
            TableCoverageRole::Message
        );
        assert_eq!(
            classify_table(
                "chatbot_session",
                &["username".to_string(), "timestamp".to_string()]
            )
            .0,
            TableCoverageRole::KnownAuxiliary
        );
    }

    #[test]
    fn favorite_item_tables_are_saved_data_not_chat_messages() {
        assert_eq!(
            classify_table(
                "fav_db_item",
                &[
                    "local_id".to_string(),
                    "server_id".to_string(),
                    "type".to_string(),
                    "content".to_string(),
                    "update_time".to_string(),
                ]
            )
            .0,
            TableCoverageRole::KnownAuxiliary
        );
        assert_eq!(
            classify_table("fav_tag_db_item", &["local_id".to_string()]).0,
            TableCoverageRole::KnownAuxiliary
        );
    }

    #[test]
    fn friend_request_messages_and_revoke_batch_metadata_have_explicit_roles() {
        let friend_message_columns = [
            "user_name_",
            "type_",
            "timestamp_",
            "encrypt_user_name_",
            "content_",
            "is_sender_",
            "ticket_",
            "scene_",
            "fmessage_detail_buf_",
            "remark_",
            "label_ids_",
        ]
        .map(str::to_string);
        assert_eq!(
            classify_table("FMessageTable", &friend_message_columns).0,
            TableCoverageRole::Message
        );
        assert_eq!(
            classify_table(
                "revokebatchmessage",
                &[
                    "local_id".to_string(),
                    "batch_id".to_string(),
                    "msg_unique_id".to_string(),
                    "session_name".to_string(),
                    "msg_local_id".to_string(),
                    "msg_create_time".to_string(),
                ]
            )
            .0,
            TableCoverageRole::KnownAuxiliary
        );
    }

    #[test]
    fn session_state_tables_are_auxiliary_not_conversation_messages() {
        for table in [
            "SessionTable",
            "SessionDeleteTable",
            "SessionDraft",
            "SessionNoContactInfoTable",
            "SessionUnreadListTable_1",
            "SessionUnreadStatTable_1",
        ] {
            assert_eq!(
                classify_table(
                    table,
                    &[
                        "username".to_string(),
                        "message_local_id".to_string(),
                        "create_time".to_string(),
                    ]
                )
                .0,
                TableCoverageRole::KnownAuxiliary
            );
        }
    }

    #[test]
    fn media_hardlink_catalog_tables_are_resource_metadata() {
        for table in [
            "dir2id",
            "file_checkpoint_v4",
            "file_hardlink_info_v4",
            "image_hardlink_info_v4",
            "talker_checkpoint_v4",
            "video_checkpoint_v4",
            "video_hardlink_info_v4",
            "TimeStamp",
            "head_image",
            "ChatName2Id",
            "SenderName2Id",
        ] {
            assert_eq!(
                classify_table(
                    table,
                    &[
                        "type".to_string(),
                        "file_name".to_string(),
                        "file_size".to_string(),
                    ]
                )
                .0,
                TableCoverageRole::KnownAuxiliary
            );
        }
    }

    #[test]
    fn contact_graph_support_tables_are_entity_metadata() {
        for table in [
            "biz_info",
            "chat_room_info_detail",
            "chatroom_member",
            "contact_label",
            "encrypt_name2id",
            "openim_acct_type",
            "openim_appid",
            "openim_wording",
            "oplog",
            "stranger",
            "stranger_ticket_info",
            "ticket_info",
        ] {
            assert_eq!(
                classify_table(
                    table,
                    &[
                        "username".to_string(),
                        "type".to_string(),
                        "ext_buffer".to_string(),
                    ]
                )
                .0,
                TableCoverageRole::KnownAuxiliary
            );
        }
    }

    #[test]
    fn sns_cached_surface_tables_are_not_chat_message_candidates() {
        for table in [
            "SnsMessage_tmp3",
            "SnsTimeLine",
            "SnsDraft",
            "SnsMainTimeLineBreakFlag",
            "SnsTopItem_1",
        ] {
            assert_eq!(
                classify_table(
                    table,
                    &[
                        "local_id".to_string(),
                        "type".to_string(),
                        "content".to_string(),
                        "create_time".to_string(),
                    ]
                )
                .0,
                TableCoverageRole::KnownAuxiliary
            );
        }
    }

    #[test]
    fn message_history_metadata_is_not_a_chat_message_candidate() {
        assert_eq!(
            classify_table(
                "HistoryAddMsgInfo",
                &[
                    "session_name_id".to_string(),
                    "history_id".to_string(),
                    "server_id".to_string(),
                    "is_revoke".to_string(),
                ]
            )
            .0,
            TableCoverageRole::KnownAuxiliary
        );
    }

    #[test]
    fn message_resource_transport_and_feature_state_tables_are_auxiliary() {
        for table in [
            "DeleteResInfo",
            "SendInfo",
            "wcdb_builtin_compression_record",
            "SolitaireFold_29a6db07e8bbdb53f5d54cc3c309f3f1",
            "SolitaireValid_29a6db07e8bbdb53f5d54cc3c309f3f1",
        ] {
            assert_eq!(
                classify_table(
                    table,
                    &[
                        "local_id".to_string(),
                        "content".to_string(),
                        "create_time".to_string(),
                    ]
                )
                .0,
                TableCoverageRole::KnownAuxiliary
            );
        }
    }

    #[test]
    fn observed_general_database_feature_tables_are_auxiliary() {
        for table in [
            "biz_pay_status",
            "biz_subscribe_status",
            "brand_search_record",
            "ForwardRecent",
            "GroupPayTable",
            "handoff_remind_v0",
            "ilink_voip",
            "new_tips",
            "RedDot",
            "RedDot_Last_Notify",
            "RedDot_Record",
            "RedEnvelopeTable",
            "SearchRecent",
            "teenager_apply_access_agree_info",
            "TransferTable",
            "WAContact",
            "WCFinderLiveStatus",
            "WCFinderUserPage",
            "WeAppBizAttrSyncBufferTableV02",
            "websearch_record",
            "ImgRangeV0",
        ] {
            assert_eq!(
                classify_table(
                    table,
                    &[
                        "type".to_string(),
                        "content".to_string(),
                        "create_time".to_string(),
                    ]
                )
                .0,
                TableCoverageRole::KnownAuxiliary
            );
        }
        for table in ["Buff", "Config", "ImgTableInfo"] {
            assert_eq!(
                classify_table(
                    table,
                    &[
                        "key".to_string(),
                        "valueInt64".to_string(),
                        "valueBlob".to_string(),
                    ]
                )
                .0,
                TableCoverageRole::KnownAuxiliary
            );
            assert_ne!(
                classify_table(
                    table,
                    &[
                        "type".to_string(),
                        "content".to_string(),
                        "create_time".to_string(),
                    ]
                )
                .0,
                TableCoverageRole::KnownAuxiliary
            );
        }
    }

    #[test]
    fn friend_contact_decoder_supports_only_observed_text_event_codes() {
        for event_code in [37, 65] {
            let (payload, state, gap) =
                decode_friend_contact_event(Some(event_code), Some(b"synthetic text"));
            assert_eq!(state, SemanticDecodeState::Complete);
            assert!(gap.is_none());
            let TypedPayload::Decoded(payload) = payload else {
                panic!("observed friend-contact event was not decoded");
            };
            assert_eq!(
                payload["FriendContactEvent"]["eventCode"],
                serde_json::json!(event_code)
            );
        }
        assert_eq!(
            decode_friend_contact_event(Some(66), Some(b"synthetic")).1,
            SemanticDecodeState::UnknownType
        );
        assert_eq!(
            decode_friend_contact_event(Some(37), Some(&[0xff])).1,
            SemanticDecodeState::Failed
        );
    }

    #[test]
    fn quote_relationships_use_decoded_xml_when_source_columns_are_compressed() {
        let payload = TypedPayload::Decoded(serde_json::json!({
            "Quote": {
                "raw_xml": "<msg><appmsg><refermsg><svrid>4242</svrid><localid>17</localid></refermsg></appmsg></msg>"
            }
        }));
        let relationships = extract_relationships(
            Some(49),
            Some(57),
            &payload,
            Some(b"source-column-is-compressed"),
            Some(b"alternate-column-is-compressed"),
        );

        assert_eq!(relationships.len(), 1);
        assert_eq!(relationships[0].kind, MessageRelationshipKind::Quote);
        assert_eq!(relationships[0].target_server_id, Some(4242));
        assert_eq!(relationships[0].target_local_id, Some(17));
        let expected_source =
            base64::engine::general_purpose::STANDARD.encode(b"alternate-column-is-compressed");
        assert_eq!(
            relationships[0].raw_reference_base64.as_deref(),
            Some(expected_source.as_str())
        );
    }

    #[test]
    fn legacy_message_types_use_named_lossless_variants() {
        let contact_xml =
            r#"<msg><username>wxid_contact</username><nickname>Contact</nickname></msg>"#;
        let (payload, state, gap) = decode_legacy_message_type(42, contact_xml);
        assert_eq!(state, SemanticDecodeState::Complete);
        assert!(gap.is_none());
        let TypedPayload::Decoded(value) = payload else {
            panic!("contact-card payload was not decoded");
        };
        assert!(value.get("ContactCard").is_some());
        assert!(value.get("variant").is_none());
        assert_eq!(value["ContactCard"]["raw_xml"], contact_xml);

        let voip_xml = r#"<msg><voipmsg><caller_memberid>caller</caller_memberid></voipmsg></msg>"#;
        let (payload, state, gap) = decode_legacy_message_type(50, voip_xml);
        assert_eq!(state, SemanticDecodeState::Complete);
        assert!(gap.is_none());
        let TypedPayload::Decoded(value) = payload else {
            panic!("VoIP payload was not decoded");
        };
        assert!(value.get("VoipCall").is_some());
        assert!(value.get("variant").is_none());

        let fragmented_voip_xml = concat!(
            "<voipinvitemsg><roomid>fixture</roomid></voipinvitemsg>",
            "<voipextinfo><recvtime>1</recvtime></voipextinfo>",
            "<voiplocalinfo><duration>1</duration></voiplocalinfo>"
        );
        let (payload, state, gap) = decode_legacy_message_type(50, fragmented_voip_xml);
        assert_eq!(state, SemanticDecodeState::Complete);
        assert!(gap.is_none());
        let TypedPayload::Decoded(value) = payload else {
            panic!("fragmented VoIP payload was not decoded");
        };
        assert_eq!(value["VoipCall"]["raw_xml"], fragmented_voip_xml);
        assert!(value["VoipCall"]["normalized_xml"].is_object());

        let push_mail_xml = r#"<msg><pushmail><subject>subject</subject></pushmail></msg>"#;
        let (payload, state, gap) = decode_legacy_message_type(35, push_mail_xml);
        assert_eq!(state, SemanticDecodeState::Complete);
        assert!(gap.is_none());
        let TypedPayload::Decoded(value) = payload else {
            panic!("push-mail payload was not decoded");
        };
        assert_eq!(value["PushMail"]["message_type"], 35);

        let old_contact_xml = r#"<msg username="wxid_contact" nickname="Contact"/>"#;
        let (payload, state, gap) = decode_legacy_message_type(66, old_contact_xml);
        assert_eq!(state, SemanticDecodeState::Complete);
        assert!(gap.is_none());
        let TypedPayload::Decoded(value) = payload else {
            panic!("legacy contact-card payload was not decoded");
        };
        assert_eq!(value["ContactCard"]["message_type"], 66);
    }

    #[test]
    fn malformed_legacy_xml_is_retained_as_lossless_partial_payload() {
        let malformed = "<msg><username>";
        let (payload, state, gap) = decode_legacy_message_type(42, malformed);
        assert_eq!(state, SemanticDecodeState::Partial);
        assert!(gap
            .as_deref()
            .is_some_and(|reason| reason.contains("could not be normalized")));
        let TypedPayload::Decoded(value) = payload else {
            panic!("legacy malformed payload was dropped");
        };
        assert_eq!(value["LegacyRaw"]["message_type"], 42);
        assert_eq!(value["LegacyRaw"]["raw_xml"], malformed);
    }
}
