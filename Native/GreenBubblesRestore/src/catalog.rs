use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use zeroize::Zeroizing;

use crate::manifest::{SnapshotFileRole, SnapshotManifest};
use crate::wal::{apply_encrypted_wal_with_progress, WalProgressStage};
use crate::{
    DatabaseKeySet, DatabasePassphrase, NoProgress, ProgressEvent, ProgressObserver, ProgressPhase,
    ProgressState, ProgressUnit, RestoreError,
};

const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StorageFamily {
    SQLite,
    WcdbSqlcipher4,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedDatabase {
    pub source_set_id: String,
    pub logical_path: String,
    pub storage_family: StorageFamily,
    pub table_count: usize,
    pub tables: Vec<String>,
    pub database_byte_count: u64,
    pub write_ahead_log_byte_count: u64,
    #[serde(skip)]
    pub path: PathBuf,
}

pub struct PreparedCatalog {
    pub manifest: SnapshotManifest,
    pub databases: Vec<PreparedDatabase>,
    pub diagnostic_batch: Option<DiagnosticDatabaseBatch>,
    pub available_database_selection: Option<AvailableDatabaseSelection>,
    pub diagnostic_available_selection: bool,
    _temporary_directory: TempDir,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticDatabaseBatch {
    pub offset: usize,
    pub limit: usize,
    pub total_database_count: usize,
}

/// Privacy-safe accounting for fault-tolerant restoration with an exported
/// key set. Every database that authenticates continues through restoration;
/// unavailable databases remain explicit coverage evidence.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableDatabaseSelection {
    pub total_database_count: usize,
    pub selected_database_count: usize,
    pub unavailable_database_count: usize,
    pub selected_database_byte_count: u64,
    pub selected_write_ahead_log_byte_count: u64,
    pub unavailable_database_byte_count: u64,
    pub unavailable_write_ahead_log_byte_count: u64,
    #[serde(rename = "selectedSourceSetIDs")]
    pub selected_source_set_ids: Vec<String>,
    pub unavailable_databases: Vec<UnavailableDatabase>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnavailableDatabase {
    #[serde(rename = "sourceSetID")]
    pub source_set_id: String,
    pub logical_path: String,
    pub storage_family: StorageFamily,
    pub database_byte_count: u64,
    pub write_ahead_log_byte_count: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Copy)]
enum CatalogSelection {
    All,
    DiagnosticBatch { offset: usize, limit: usize },
    AvailableExportedKeys,
}

#[derive(Clone, Copy)]
pub enum DatabaseUnlockMaterial<'a> {
    None,
    Passphrase(&'a DatabasePassphrase),
    ExportedKeys(&'a DatabaseKeySet),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotStoragePreflightDatabase {
    #[serde(rename = "sourceSetID")]
    pub source_set_id: String,
    pub logical_path: String,
    pub byte_count: i64,
    pub storage_family: StorageFamily,
    pub passphrase_required: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotStoragePreflightReport {
    pub report_format_version: u32,
    #[serde(rename = "snapshotID")]
    pub snapshot_id: String,
    pub source_fingerprint: String,
    pub client_build_compatibility: crate::manifest::ClientBuildCompatibilityEvidence,
    pub acquisition_mode: Option<crate::manifest::SnapshotAcquisitionMode>,
    pub current_source_set_count: usize,
    pub copied_database_count: usize,
    pub copied_storage_family_counts: BTreeMap<&'static str, usize>,
    pub copied_database_passphrase_required: bool,
    pub databases: Vec<SnapshotStoragePreflightDatabase>,
}

pub fn preflight_snapshot(
    snapshot_dir: &Path,
) -> Result<SnapshotStoragePreflightReport, RestoreError> {
    preflight_snapshot_with_progress(snapshot_dir, &NoProgress)
}

pub fn preflight_snapshot_with_progress(
    snapshot_dir: &Path,
    progress: &dyn ProgressObserver,
) -> Result<SnapshotStoragePreflightReport, RestoreError> {
    let manifest = SnapshotManifest::load(snapshot_dir)?;
    let mut databases = Vec::new();
    let mut copied_storage_family_counts = BTreeMap::new();
    let verification_total = manifest
        .entries
        .iter()
        .map(|entry| entry.fingerprint.byte_count.max(0) as u64)
        .sum::<u64>();
    let file_count = manifest.entries.len();
    let database_count = manifest.database_entries().count();
    let verification_started = Instant::now();
    let mut planned = ProgressEvent::new(
        ProgressPhase::SnapshotVerification,
        ProgressState::Planned,
        "verifySnapshot",
        ProgressUnit::Bytes,
        0,
        verification_total,
        0,
        verification_total,
    );
    planned.database_count = Some(database_count);
    planned.file_count = Some(file_count);
    progress.observe(planned);

    let mut verified_bytes = 0_u64;
    for (file_index, entry) in manifest.entries.iter().enumerate() {
        verify_snapshot_entry_with_progress(
            snapshot_dir,
            entry,
            verified_bytes,
            verification_total,
            file_index + 1,
            file_count,
            progress,
        )?;
        verified_bytes = verified_bytes.saturating_add(entry.fingerprint.byte_count.max(0) as u64);
        if entry.role != SnapshotFileRole::Database {
            continue;
        }
        let source = entry.resolved_path(snapshot_dir)?;
        let header = read_header_safely(&source, &entry.source.opaque_id)?;
        let storage_family = if &header == SQLITE_HEADER {
            StorageFamily::SQLite
        } else {
            StorageFamily::WcdbSqlcipher4
        };
        let family_name = match storage_family {
            StorageFamily::SQLite => "sqlite",
            StorageFamily::WcdbSqlcipher4 => "wcdbSqlcipher4",
        };
        *copied_storage_family_counts.entry(family_name).or_insert(0) += 1;
        databases.push(SnapshotStoragePreflightDatabase {
            source_set_id: entry.source_set_id.clone(),
            logical_path: entry.logical_path.clone(),
            byte_count: entry.fingerprint.byte_count,
            storage_family,
            passphrase_required: storage_family == StorageFamily::WcdbSqlcipher4,
        });
    }
    let mut verification_finished = ProgressEvent::new(
        ProgressPhase::SnapshotVerification,
        ProgressState::Completed,
        "verifySnapshot",
        ProgressUnit::Bytes,
        verification_total,
        verification_total,
        verification_total,
        verification_total,
    );
    verification_finished.database_count = Some(database_count);
    verification_finished.file_count = Some(file_count);
    verification_finished.elapsed_milliseconds = Some(elapsed_milliseconds(verification_started));
    progress.observe(verification_finished);
    databases.sort_by(|left, right| {
        (&left.logical_path, &left.source_set_id).cmp(&(&right.logical_path, &right.source_set_id))
    });
    let copied_database_passphrase_required = databases
        .iter()
        .any(|database| database.passphrase_required);
    let current_source_set_count = manifest
        .acquisition
        .as_ref()
        .map_or(databases.len(), |value| value.source_sets.len());

    Ok(SnapshotStoragePreflightReport {
        report_format_version: 1,
        snapshot_id: manifest.snapshot_id.clone(),
        source_fingerprint: manifest.source_fingerprint.clone(),
        client_build_compatibility: manifest.client_build_compatibility(),
        acquisition_mode: manifest.acquisition.as_ref().map(|value| value.mode),
        current_source_set_count,
        copied_database_count: databases.len(),
        copied_storage_family_counts,
        copied_database_passphrase_required,
        databases,
    })
}

pub fn prepare_catalog(
    snapshot_dir: &Path,
    passphrase: Option<&DatabasePassphrase>,
) -> Result<PreparedCatalog, RestoreError> {
    let unlock = passphrase.map_or(
        DatabaseUnlockMaterial::None,
        DatabaseUnlockMaterial::Passphrase,
    );
    prepare_catalog_with_progress(snapshot_dir, unlock, &NoProgress)
}

pub fn prepare_catalog_with_unlock(
    snapshot_dir: &Path,
    unlock: DatabaseUnlockMaterial<'_>,
) -> Result<PreparedCatalog, RestoreError> {
    prepare_catalog_with_progress(snapshot_dir, unlock, &NoProgress)
}

pub fn prepare_catalog_with_progress(
    snapshot_dir: &Path,
    unlock: DatabaseUnlockMaterial<'_>,
    progress: &dyn ProgressObserver,
) -> Result<PreparedCatalog, RestoreError> {
    prepare_catalog_internal(snapshot_dir, unlock, progress, CatalogSelection::All)
}

pub fn prepare_available_catalog_with_progress(
    snapshot_dir: &Path,
    keys: &DatabaseKeySet,
    progress: &dyn ProgressObserver,
) -> Result<PreparedCatalog, RestoreError> {
    prepare_catalog_internal(
        snapshot_dir,
        DatabaseUnlockMaterial::ExportedKeys(keys),
        progress,
        CatalogSelection::AvailableExportedKeys,
    )
}

pub fn prepare_catalog_batch_with_progress(
    snapshot_dir: &Path,
    unlock: DatabaseUnlockMaterial<'_>,
    offset: usize,
    limit: usize,
    progress: &dyn ProgressObserver,
) -> Result<PreparedCatalog, RestoreError> {
    if limit == 0 {
        return Err(RestoreError::Manifest(
            "diagnostic database limit must be positive".to_string(),
        ));
    }
    prepare_catalog_internal(
        snapshot_dir,
        unlock,
        progress,
        CatalogSelection::DiagnosticBatch { offset, limit },
    )
}

fn prepare_catalog_internal(
    snapshot_dir: &Path,
    unlock: DatabaseUnlockMaterial<'_>,
    progress: &dyn ProgressObserver,
    selection: CatalogSelection,
) -> Result<PreparedCatalog, RestoreError> {
    let manifest = SnapshotManifest::load(snapshot_dir)?;
    let output = tempfile::Builder::new()
        .prefix("greenbubbles-plain-")
        .tempdir()?;
    set_owner_only(output.path())?;

    let mut database_entries = manifest.database_entries().collect::<Vec<_>>();
    if matches!(unlock, DatabaseUnlockMaterial::ExportedKeys(_))
        || matches!(
            selection,
            CatalogSelection::DiagnosticBatch { .. } | CatalogSelection::AvailableExportedKeys
        )
    {
        database_entries.sort_by(|left, right| {
            (
                left.fingerprint.byte_count,
                &left.logical_path,
                &left.source_set_id,
            )
                .cmp(&(
                    right.fingerprint.byte_count,
                    &right.logical_path,
                    &right.source_set_id,
                ))
        });
    }
    let total_database_count = database_entries.len();
    let diagnostic_batch = match selection {
        CatalogSelection::DiagnosticBatch { offset, limit } => Some(DiagnosticDatabaseBatch {
            offset,
            limit,
            total_database_count,
        }),
        CatalogSelection::All | CatalogSelection::AvailableExportedKeys => None,
    };
    if let Some(batch) = diagnostic_batch {
        if batch.offset >= batch.total_database_count {
            return Err(RestoreError::Manifest(
                "diagnostic database offset is past the catalog".to_string(),
            ));
        }
        database_entries = database_entries
            .into_iter()
            .skip(batch.offset)
            .take(batch.limit)
            .collect();
    }
    let verification_set_ids = database_entries
        .iter()
        .map(|entry| entry.source_set_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let verification_entries = manifest
        .entries
        .iter()
        .filter(|entry| verification_set_ids.contains(entry.source_set_id.as_str()))
        .collect::<Vec<_>>();

    let verification_total = verification_entries
        .iter()
        .map(|entry| entry.fingerprint.byte_count.max(0) as u64)
        .sum::<u64>();
    let verification_file_count = verification_entries.len();
    let verification_started = Instant::now();
    let verification_database_count = database_entries.len();
    let mut planned = ProgressEvent::new(
        ProgressPhase::SnapshotVerification,
        ProgressState::Planned,
        "verifySnapshot",
        ProgressUnit::Bytes,
        0,
        verification_total,
        0,
        verification_total,
    );
    planned.database_count = Some(verification_database_count);
    planned.file_count = Some(verification_file_count);
    progress.observe(planned);
    let mut verified_bytes = 0_u64;
    for (file_index, entry) in verification_entries.into_iter().enumerate() {
        verify_snapshot_entry_with_progress(
            snapshot_dir,
            entry,
            verified_bytes,
            verification_total,
            file_index + 1,
            verification_file_count,
            progress,
        )?;
        verified_bytes = verified_bytes.saturating_add(entry.fingerprint.byte_count.max(0) as u64);
    }
    let mut verification_finished = ProgressEvent::new(
        ProgressPhase::SnapshotVerification,
        ProgressState::Completed,
        "verifySnapshot",
        ProgressUnit::Bytes,
        verification_total,
        verification_total,
        verification_total,
        verification_total,
    );
    verification_finished.database_count = Some(verification_database_count);
    verification_finished.file_count = Some(verification_file_count);
    verification_finished.elapsed_milliseconds = Some(elapsed_milliseconds(verification_started));
    progress.observe(verification_finished);

    let diagnostic_available_selection =
        matches!(selection, CatalogSelection::AvailableExportedKeys);
    let available_database_selection = match unlock {
        DatabaseUnlockMaterial::ExportedKeys(keys) => {
            let (available, summary) = select_available_exported_keys(
                snapshot_dir,
                &manifest,
                database_entries,
                keys,
                progress,
            )?;
            database_entries = available;
            Some(summary)
        }
        _ if diagnostic_available_selection => {
            return Err(RestoreError::InvalidDatabaseKeyExport(
                "available-database restoration requires an exported-key set".to_string(),
            ));
        }
        _ => None,
    };

    let preparation_total = database_entries
        .iter()
        .map(|entry| {
            let database_bytes = entry.fingerprint.byte_count.max(0) as u64;
            let wal_bytes = manifest
                .sidecar(&entry.source_set_id, SnapshotFileRole::WriteAheadLog)
                .map_or(0, |wal| wal.fingerprint.byte_count.max(0) as u64);
            database_bytes.saturating_add(wal_bytes.saturating_mul(2))
        })
        .sum::<u64>();
    let database_count = database_entries.len();
    let preparation_started = Instant::now();
    let mut preparation_planned = ProgressEvent::new(
        ProgressPhase::DatabasePreparation,
        ProgressState::Planned,
        "prepareDatabases",
        ProgressUnit::Bytes,
        0,
        preparation_total,
        0,
        preparation_total,
    );
    preparation_planned.database_count = Some(database_count);
    progress.observe(preparation_planned);

    let mut databases = Vec::new();
    let mut preparation_completed = 0_u64;
    for (database_index, entry) in database_entries.into_iter().enumerate() {
        let source = entry.resolved_path(snapshot_dir)?;
        let header = read_header_safely(&source, &entry.source.opaque_id)?;
        let family = if &header == SQLITE_HEADER {
            StorageFamily::SQLite
        } else {
            StorageFamily::WcdbSqlcipher4
        };
        let destination = output.path().join(format!("{}.db", entry.source_set_id));
        let database_bytes = entry.fingerprint.byte_count.max(0) as u64;
        let wal = manifest.sidecar(&entry.source_set_id, SnapshotFileRole::WriteAheadLog);
        let wal_bytes = wal.map_or(0, |entry| entry.fingerprint.byte_count.max(0) as u64);
        let database_total = database_bytes.saturating_add(wal_bytes.saturating_mul(2));
        let database_started = Instant::now();
        progress.observe(database_event(
            ProgressState::Started,
            "prepareDatabase",
            0,
            database_total,
            preparation_completed,
            preparation_total,
            database_index,
            database_count,
            entry,
            family,
            database_bytes,
            wal_bytes,
        ));

        let database_operation = match family {
            StorageFamily::SQLite => "copyPlaintextDatabase",
            StorageFamily::WcdbSqlcipher4 => "decryptDatabase",
        };
        progress.observe(database_event(
            ProgressState::Started,
            database_operation,
            0,
            database_bytes,
            preparation_completed,
            preparation_total,
            database_index,
            database_count,
            entry,
            family,
            database_bytes,
            wal_bytes,
        ));

        match family {
            StorageFamily::SQLite => {
                monitor_output_growth(
                    database_operation,
                    &destination,
                    database_bytes,
                    preparation_completed,
                    preparation_total,
                    database_index,
                    database_count,
                    entry,
                    family,
                    database_bytes,
                    wal_bytes,
                    progress,
                    || {
                        fs::copy(&source, &destination)
                            .map(|_| ())
                            .map_err(RestoreError::from)
                    },
                )?;
                progress.observe(database_event(
                    ProgressState::Completed,
                    database_operation,
                    database_bytes,
                    database_bytes,
                    preparation_completed.saturating_add(database_bytes),
                    preparation_total,
                    database_index,
                    database_count,
                    entry,
                    family,
                    database_bytes,
                    wal_bytes,
                ));
                if let Some(wal) = wal {
                    apply_plaintext_wal_with_progress(
                        snapshot_dir,
                        &destination,
                        wal,
                        preparation_completed.saturating_add(database_bytes),
                        preparation_total,
                        database_index,
                        database_count,
                        entry,
                        family,
                        database_bytes,
                        wal_bytes,
                        progress,
                    )?;
                }
            }
            StorageFamily::WcdbSqlcipher4 => {
                monitor_output_growth(
                    database_operation,
                    &destination,
                    database_bytes,
                    preparation_completed,
                    preparation_total,
                    database_index,
                    database_count,
                    entry,
                    family,
                    database_bytes,
                    wal_bytes,
                    progress,
                    || decrypt_database(&source, &destination, entry, unlock),
                )?;
                progress.observe(database_event(
                    ProgressState::Completed,
                    database_operation,
                    database_bytes,
                    database_bytes,
                    preparation_completed.saturating_add(database_bytes),
                    preparation_total,
                    database_index,
                    database_count,
                    entry,
                    family,
                    database_bytes,
                    wal_bytes,
                ));
                if let Some(wal) = wal {
                    let wal_source = wal.resolved_path(snapshot_dir)?;
                    decrypt_write_ahead_log(
                        &source,
                        &wal_source,
                        &destination,
                        entry,
                        unlock,
                        |stage, state, completed, total, frame_count| {
                            let (operation, stage_offset) = match stage {
                                WalProgressStage::Scan => ("scanWriteAheadLog", 0),
                                WalProgressStage::Apply => ("applyWriteAheadLog", wal_bytes),
                            };
                            let mut event = database_event(
                                state,
                                operation,
                                completed,
                                total,
                                preparation_completed
                                    .saturating_add(database_bytes)
                                    .saturating_add(stage_offset)
                                    .saturating_add(completed.min(wal_bytes)),
                                preparation_total,
                                database_index,
                                database_count,
                                entry,
                                family,
                                database_bytes,
                                wal_bytes,
                            );
                            event.write_ahead_log_frame_count = Some(frame_count);
                            progress.observe(event);
                        },
                    )?;
                }
            }
        }
        set_owner_only_file(&destination)?;
        let tables = inspect_tables(&destination)?;
        preparation_completed = preparation_completed.saturating_add(database_total);
        let mut finished = database_event(
            ProgressState::Completed,
            "prepareDatabase",
            database_total,
            database_total,
            preparation_completed,
            preparation_total,
            database_index,
            database_count,
            entry,
            family,
            database_bytes,
            wal_bytes,
        );
        finished.table_count = Some(tables.len());
        finished.elapsed_milliseconds = Some(elapsed_milliseconds(database_started));
        progress.observe(finished);
        databases.push(PreparedDatabase {
            source_set_id: entry.source_set_id.clone(),
            logical_path: entry.logical_path.clone(),
            storage_family: family,
            table_count: tables.len(),
            tables,
            database_byte_count: database_bytes,
            write_ahead_log_byte_count: wal_bytes,
            path: destination,
        });
    }
    let mut preparation_finished = ProgressEvent::new(
        ProgressPhase::DatabasePreparation,
        ProgressState::Completed,
        "prepareDatabases",
        ProgressUnit::Bytes,
        preparation_total,
        preparation_total,
        preparation_total,
        preparation_total,
    );
    preparation_finished.database_count = Some(database_count);
    preparation_finished.elapsed_milliseconds = Some(elapsed_milliseconds(preparation_started));
    progress.observe(preparation_finished);
    databases.sort_by(|a, b| {
        (&a.logical_path, &a.source_set_id).cmp(&(&b.logical_path, &b.source_set_id))
    });
    Ok(PreparedCatalog {
        manifest,
        databases,
        diagnostic_batch,
        available_database_selection,
        diagnostic_available_selection,
        _temporary_directory: output,
    })
}

impl PreparedCatalog {
    pub fn storage_family_counts(&self) -> BTreeMap<&'static str, usize> {
        let mut result = BTreeMap::new();
        for database in &self.databases {
            let key = match database.storage_family {
                StorageFamily::SQLite => "sqlite",
                StorageFamily::WcdbSqlcipher4 => "wcdbSqlcipher4",
            };
            *result.entry(key).or_insert(0) += 1;
        }
        result
    }
}

fn select_available_exported_keys<'a>(
    snapshot_dir: &Path,
    manifest: &SnapshotManifest,
    database_entries: Vec<&'a crate::manifest::SnapshotEntry>,
    keys: &DatabaseKeySet,
    progress: &dyn ProgressObserver,
) -> Result<
    (
        Vec<&'a crate::manifest::SnapshotEntry>,
        AvailableDatabaseSelection,
    ),
    RestoreError,
> {
    let started = Instant::now();
    let total = database_entries.len();
    let mut planned = ProgressEvent::new(
        ProgressPhase::KeyValidation,
        ProgressState::Planned,
        "assessAvailableDatabaseKeys",
        ProgressUnit::Items,
        0,
        total as u64,
        0,
        total as u64,
    );
    planned.database_count = Some(total);
    planned.available_database_count = Some(0);
    planned.unavailable_database_count = Some(0);
    progress.observe(planned);

    let mut available = Vec::new();
    let mut unavailable = Vec::new();
    let mut selected_database_byte_count = 0_u64;
    let mut selected_write_ahead_log_byte_count = 0_u64;
    let mut unavailable_database_byte_count = 0_u64;
    let mut unavailable_write_ahead_log_byte_count = 0_u64;

    for (index, entry) in database_entries.into_iter().enumerate() {
        let source = entry.resolved_path(snapshot_dir)?;
        let header = read_header_safely(&source, &entry.source.opaque_id)?;
        let family = if &header == SQLITE_HEADER {
            StorageFamily::SQLite
        } else {
            StorageFamily::WcdbSqlcipher4
        };
        let database_bytes = entry.fingerprint.byte_count.max(0) as u64;
        let wal_bytes = manifest
            .sidecar(&entry.source_set_id, SnapshotFileRole::WriteAheadLog)
            .map_or(0, |wal| wal.fingerprint.byte_count.max(0) as u64);
        let (is_available, match_method, unlock_state) = match family {
            StorageFamily::SQLite => (true, None, "notRequired"),
            StorageFamily::WcdbSqlcipher4 => {
                let page = read_first_page_safely(&source, &entry.source.opaque_id)?;
                let authentication = keys.authenticate_database(&entry.logical_path, &page);
                (
                    authentication.encryption_key.is_some(),
                    authentication.method,
                    if authentication.encryption_key.is_some() {
                        "available"
                    } else {
                        "unavailable"
                    },
                )
            }
        };
        if is_available {
            selected_database_byte_count =
                selected_database_byte_count.saturating_add(database_bytes);
            selected_write_ahead_log_byte_count =
                selected_write_ahead_log_byte_count.saturating_add(wal_bytes);
            available.push(entry);
        } else {
            unavailable_database_byte_count =
                unavailable_database_byte_count.saturating_add(database_bytes);
            unavailable_write_ahead_log_byte_count =
                unavailable_write_ahead_log_byte_count.saturating_add(wal_bytes);
            unavailable.push(UnavailableDatabase {
                source_set_id: entry.source_set_id.clone(),
                logical_path: entry.logical_path.clone(),
                storage_family: family,
                database_byte_count: database_bytes,
                write_ahead_log_byte_count: wal_bytes,
                reason: "noExportedKeyAuthenticated".to_string(),
            });
        }

        let mut event = ProgressEvent::new(
            ProgressPhase::KeyValidation,
            ProgressState::Advanced,
            "assessAvailableDatabaseKey",
            ProgressUnit::Items,
            1,
            1,
            index as u64 + 1,
            total as u64,
        );
        event.database_index = Some(index + 1);
        event.database_count = Some(total);
        event.source_set_id = Some(entry.source_set_id.clone());
        event.logical_path = Some(entry.logical_path.clone());
        event.storage_family = Some(storage_family_name(family).to_string());
        event.database_byte_count = Some(database_bytes);
        event.write_ahead_log_byte_count = Some(wal_bytes);
        event.database_key_match_method = match_method.map(|method| method.name().to_string());
        event.database_unlock_state = Some(unlock_state.to_string());
        event.available_database_count = Some(available.len());
        event.unavailable_database_count = Some(unavailable.len());
        progress.observe(event);
    }

    unavailable.sort_by(|left, right| left.source_set_id.cmp(&right.source_set_id));
    let mut selected_source_set_ids = available
        .iter()
        .map(|entry| entry.source_set_id.clone())
        .collect::<Vec<_>>();
    selected_source_set_ids.sort();
    let summary = AvailableDatabaseSelection {
        total_database_count: total,
        selected_database_count: available.len(),
        unavailable_database_count: unavailable.len(),
        selected_database_byte_count,
        selected_write_ahead_log_byte_count,
        unavailable_database_byte_count,
        unavailable_write_ahead_log_byte_count,
        selected_source_set_ids,
        unavailable_databases: unavailable,
    };
    let mut finished = ProgressEvent::new(
        ProgressPhase::KeyValidation,
        ProgressState::Completed,
        "assessAvailableDatabaseKeys",
        ProgressUnit::Items,
        total as u64,
        total as u64,
        total as u64,
        total as u64,
    );
    finished.database_count = Some(total);
    finished.available_database_count = Some(summary.selected_database_count);
    finished.unavailable_database_count = Some(summary.unavailable_database_count);
    finished.elapsed_milliseconds = Some(elapsed_milliseconds(started));
    progress.observe(finished);

    if available.is_empty() {
        return Err(RestoreError::Integrity(
            "none of the snapshot databases can be restored with the supplied exported keys"
                .to_string(),
        ));
    }
    Ok((available, summary))
}

fn decrypt_database(
    source: &Path,
    destination: &Path,
    entry: &crate::manifest::SnapshotEntry,
    unlock: DatabaseUnlockMaterial<'_>,
) -> Result<(), RestoreError> {
    match unlock {
        DatabaseUnlockMaterial::None => Err(RestoreError::PassphraseRequired(
            entry.source_set_id.clone(),
        )),
        DatabaseUnlockMaterial::Passphrase(passphrase) => wx_decrypt::decrypt_db(
            source,
            destination,
            passphrase.expose_for_database_operation(),
            &wx_decrypt::MACOS_4_1_7_31,
        )
        .map_err(|error| RestoreError::Decryption {
            set_id: entry.source_set_id.clone(),
            reason: error.to_string(),
        }),
        DatabaseUnlockMaterial::ExportedKeys(keys) => {
            let page = read_first_page_safely(source, &entry.source.opaque_id)?;
            let mut database_salt = [0_u8; 16];
            database_salt.copy_from_slice(&page[..16]);
            let authentication = keys.authenticate_database(&entry.logical_path, &page);
            let encryption_key = authentication
                .encryption_key
                .ok_or_else(|| authentication.association_error(&entry.source_set_id))?;
            wx_decrypt::decrypt_db_direct(
                source,
                destination,
                encryption_key,
                &database_salt,
                &wx_decrypt::MACOS_4_1_7_31,
            )
            .map_err(|error| RestoreError::Decryption {
                set_id: entry.source_set_id.clone(),
                reason: error.to_string(),
            })
        }
    }
}

fn decrypt_write_ahead_log<F>(
    database_source: &Path,
    wal_source: &Path,
    destination: &Path,
    entry: &crate::manifest::SnapshotEntry,
    unlock: DatabaseUnlockMaterial<'_>,
    progress: F,
) -> Result<usize, RestoreError>
where
    F: FnMut(WalProgressStage, ProgressState, u64, u64, u64),
{
    let page = read_first_page_safely(database_source, &entry.source.opaque_id)?;
    let mut database_salt = [0_u8; 16];
    database_salt.copy_from_slice(&page[..16]);
    let (encryption_key, salt) = match unlock {
        DatabaseUnlockMaterial::None => {
            return Err(RestoreError::PassphraseRequired(
                entry.source_set_id.clone(),
            ));
        }
        DatabaseUnlockMaterial::Passphrase(passphrase) => (
            Zeroizing::new(wx_decrypt::kdf::derive_enc_key(
                passphrase.expose_for_database_operation(),
                &database_salt,
                &wx_decrypt::MACOS_4_1_7_31,
            )),
            database_salt,
        ),
        DatabaseUnlockMaterial::ExportedKeys(keys) => {
            let authentication = keys.authenticate_database(&entry.logical_path, &page);
            let encryption_key = authentication
                .encryption_key
                .ok_or_else(|| authentication.association_error(&entry.source_set_id))?;
            (Zeroizing::new(*encryption_key), database_salt)
        }
    };
    apply_encrypted_wal_with_progress(
        wal_source,
        destination,
        &encryption_key,
        &salt,
        &wx_decrypt::MACOS_4_1_7_31,
        progress,
    )
    .map_err(|error| RestoreError::Decryption {
        set_id: entry.source_set_id.clone(),
        reason: format!("WAL: {error}"),
    })
}

#[allow(clippy::too_many_arguments)]
fn apply_plaintext_wal_with_progress(
    snapshot_dir: &Path,
    destination: &Path,
    wal: &crate::manifest::SnapshotEntry,
    phase_before: u64,
    phase_total: u64,
    database_index: usize,
    database_count: usize,
    database_entry: &crate::manifest::SnapshotEntry,
    family: StorageFamily,
    database_bytes: u64,
    wal_bytes: u64,
    progress: &dyn ProgressObserver,
) -> Result<(), RestoreError> {
    let wal_source = wal.resolved_path(snapshot_dir)?;
    let wal_destination = destination.with_file_name(format!(
        "{}-wal",
        destination
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| RestoreError::Integrity("unsafe temporary database name".to_string()))?
    ));
    progress.observe(database_event(
        ProgressState::Started,
        "copyPlaintextWriteAheadLog",
        0,
        wal_bytes,
        phase_before,
        phase_total,
        database_index,
        database_count,
        database_entry,
        family,
        database_bytes,
        wal_bytes,
    ));
    monitor_output_growth(
        "copyPlaintextWriteAheadLog",
        &wal_destination,
        wal_bytes,
        phase_before,
        phase_total,
        database_index,
        database_count,
        database_entry,
        family,
        database_bytes,
        wal_bytes,
        progress,
        || {
            fs::copy(&wal_source, &wal_destination)
                .map(|_| ())
                .map_err(RestoreError::from)
        },
    )?;
    set_owner_only_file(&wal_destination)?;
    progress.observe(database_event(
        ProgressState::Completed,
        "copyPlaintextWriteAheadLog",
        wal_bytes,
        wal_bytes,
        phase_before.saturating_add(wal_bytes),
        phase_total,
        database_index,
        database_count,
        database_entry,
        family,
        database_bytes,
        wal_bytes,
    ));

    let checkpoint_started = Instant::now();
    progress.observe(database_event(
        ProgressState::Started,
        "applyPlaintextWriteAheadLog",
        0,
        wal_bytes,
        phase_before.saturating_add(wal_bytes),
        phase_total,
        database_index,
        database_count,
        database_entry,
        family,
        database_bytes,
        wal_bytes,
    ));
    let connection = Connection::open_with_flags(
        destination,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let (_, _, checkpointed_frames): (i64, i64, i64) =
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    drop(connection);
    let mut finished = database_event(
        ProgressState::Completed,
        "applyPlaintextWriteAheadLog",
        wal_bytes,
        wal_bytes,
        phase_before.saturating_add(wal_bytes.saturating_mul(2)),
        phase_total,
        database_index,
        database_count,
        database_entry,
        family,
        database_bytes,
        wal_bytes,
    );
    finished.write_ahead_log_frame_count = Some(checkpointed_frames.max(0) as u64);
    finished.elapsed_milliseconds = Some(elapsed_milliseconds(checkpoint_started));
    progress.observe(finished);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn monitor_output_growth<F>(
    operation_name: &str,
    destination: &Path,
    database_bytes: u64,
    overall_before: u64,
    overall_total: u64,
    database_index: usize,
    database_count: usize,
    entry: &crate::manifest::SnapshotEntry,
    family: StorageFamily,
    displayed_database_bytes: u64,
    wal_bytes: u64,
    progress: &dyn ProgressObserver,
    operation: F,
) -> Result<(), RestoreError>
where
    F: FnOnce() -> Result<(), RestoreError>,
{
    let finished = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let monitor = scope.spawn(|| {
            let mut last = 0_u64;
            while !finished.load(Ordering::Acquire) {
                std::thread::park_timeout(Duration::from_millis(500));
                let current = fs::metadata(destination)
                    .map(|metadata| metadata.len().min(database_bytes))
                    .unwrap_or(0);
                if current > last && !finished.load(Ordering::Acquire) {
                    last = current;
                    progress.observe(database_event(
                        ProgressState::Advanced,
                        operation_name,
                        current,
                        database_bytes,
                        overall_before.saturating_add(current),
                        overall_total,
                        database_index,
                        database_count,
                        entry,
                        family,
                        displayed_database_bytes,
                        wal_bytes,
                    ));
                }
            }
        });
        let result = operation();
        finished.store(true, Ordering::Release);
        monitor.thread().unpark();
        let _ = monitor.join();
        result
    })
}

#[allow(clippy::too_many_arguments)]
fn database_event(
    state: ProgressState,
    operation: &str,
    completed: u64,
    total: u64,
    overall_completed: u64,
    overall_total: u64,
    database_index: usize,
    database_count: usize,
    entry: &crate::manifest::SnapshotEntry,
    family: StorageFamily,
    database_bytes: u64,
    wal_bytes: u64,
) -> ProgressEvent {
    let mut event = ProgressEvent::new(
        ProgressPhase::DatabasePreparation,
        state,
        operation,
        ProgressUnit::Bytes,
        completed,
        total,
        overall_completed,
        overall_total,
    );
    event.database_index = Some(database_index + 1);
    event.database_count = Some(database_count);
    event.source_set_id = Some(entry.source_set_id.clone());
    event.logical_path = Some(entry.logical_path.clone());
    event.storage_family = Some(storage_family_name(family).to_string());
    event.database_byte_count = Some(database_bytes);
    event.write_ahead_log_byte_count = Some(wal_bytes);
    event
}

fn storage_family_name(family: StorageFamily) -> &'static str {
    match family {
        StorageFamily::SQLite => "sqlite",
        StorageFamily::WcdbSqlcipher4 => "wcdbSqlcipher4",
    }
}

fn inspect_tables(path: &Path) -> Result<Vec<String>, RestoreError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.execute_batch("PRAGMA query_only = ON")?;
    let mut statement = connection.prepare(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let values = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(values)
}

fn read_header_safely(path: &Path, source_id: &str) -> Result<[u8; 16], RestoreError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| RestoreError::Integrity(source_id.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|_| RestoreError::Integrity(source_id.to_string()))?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(RestoreError::Integrity(source_id.to_string()));
    }
    let mut result = [0_u8; 16];
    file.read_exact(&mut result)
        .map_err(|_| RestoreError::Integrity(source_id.to_string()))?;
    Ok(result)
}

fn read_first_page_safely(path: &Path, source_id: &str) -> Result<Vec<u8>, RestoreError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| RestoreError::Integrity(source_id.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|_| RestoreError::Integrity(source_id.to_string()))?;
    let page_size = wx_decrypt::MACOS_4_1_7_31.page_size;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() < page_size as u64 {
        return Err(RestoreError::Integrity(source_id.to_string()));
    }
    let mut page = vec![0_u8; page_size];
    file.read_exact(&mut page)
        .map_err(|_| RestoreError::Integrity(source_id.to_string()))?;
    Ok(page)
}

fn verify_snapshot_entry_with_progress(
    snapshot_dir: &Path,
    entry: &crate::manifest::SnapshotEntry,
    overall_before: u64,
    overall_total: u64,
    file_index: usize,
    file_count: usize,
    progress: &dyn ProgressObserver,
) -> Result<(), RestoreError> {
    let path = entry.resolved_path(snapshot_dir)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|_| RestoreError::Integrity(entry.source.opaque_id.clone()))?;
    let metadata = file
        .metadata()
        .map_err(|_| RestoreError::Integrity(entry.source.opaque_id.clone()))?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.len() != entry.fingerprint.byte_count.max(0) as u64
    {
        return Err(RestoreError::Integrity(entry.source.opaque_id.clone()));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    let total = metadata.len();
    let report_increment = (total / 100).max(8 * 1024 * 1024).max(1);
    let mut completed = 0_u64;
    let mut next_report = report_increment;
    let started = Instant::now();
    let mut started_event = ProgressEvent::new(
        ProgressPhase::SnapshotVerification,
        ProgressState::Started,
        snapshot_role_name(entry.role),
        ProgressUnit::Bytes,
        0,
        total,
        overall_before,
        overall_total,
    );
    started_event.source_set_id = Some(entry.source_set_id.clone());
    started_event.logical_path = Some(entry.logical_path.clone());
    started_event.file_index = Some(file_index);
    started_event.file_count = Some(file_count);
    started_event.file_byte_count = Some(total);
    progress.observe(started_event);
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        completed = completed.saturating_add(count as u64);
        if completed >= next_report && completed < total {
            let mut event = ProgressEvent::new(
                ProgressPhase::SnapshotVerification,
                ProgressState::Advanced,
                snapshot_role_name(entry.role),
                ProgressUnit::Bytes,
                completed,
                total,
                overall_before.saturating_add(completed),
                overall_total,
            );
            event.source_set_id = Some(entry.source_set_id.clone());
            event.logical_path = Some(entry.logical_path.clone());
            event.file_index = Some(file_index);
            event.file_count = Some(file_count);
            event.file_byte_count = Some(total);
            progress.observe(event);
            next_report = completed.saturating_add(report_increment);
        }
    }
    if !hex::encode(hasher.finalize()).eq_ignore_ascii_case(&entry.sha256) {
        return Err(RestoreError::Integrity(entry.source.opaque_id.clone()));
    }
    let mut finished_event = ProgressEvent::new(
        ProgressPhase::SnapshotVerification,
        ProgressState::Completed,
        snapshot_role_name(entry.role),
        ProgressUnit::Bytes,
        total,
        total,
        overall_before.saturating_add(total),
        overall_total,
    );
    finished_event.source_set_id = Some(entry.source_set_id.clone());
    finished_event.logical_path = Some(entry.logical_path.clone());
    finished_event.file_index = Some(file_index);
    finished_event.file_count = Some(file_count);
    finished_event.file_byte_count = Some(total);
    finished_event.elapsed_milliseconds = Some(elapsed_milliseconds(started));
    progress.observe(finished_event);
    Ok(())
}

fn snapshot_role_name(role: SnapshotFileRole) -> &'static str {
    match role {
        SnapshotFileRole::Database => "verifyDatabase",
        SnapshotFileRole::WriteAheadLog => "verifyWriteAheadLog",
        SnapshotFileRole::SharedMemory => "verifySharedMemory",
    }
}

fn elapsed_milliseconds(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<(), RestoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_file(path: &Path) -> Result<(), RestoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{PathReference, SnapshotEntry, SourceFileFingerprint};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn preflight_reports_every_storage_family_without_decryption() {
        let directory = tempfile::tempdir().unwrap();
        let mut plaintext = SQLITE_HEADER.to_vec();
        plaintext.extend([0_u8; 48]);
        let encrypted = vec![0x42_u8; 64];
        write_legacy_snapshot(directory.path(), &[plaintext, encrypted]);

        let report = preflight_snapshot(directory.path()).unwrap();
        assert_eq!(report.report_format_version, 1);
        assert_eq!(report.current_source_set_count, 2);
        assert_eq!(report.copied_database_count, 2);
        assert_eq!(report.copied_storage_family_counts.get("sqlite"), Some(&1));
        assert_eq!(
            report.copied_storage_family_counts.get("wcdbSqlcipher4"),
            Some(&1)
        );
        assert!(report.copied_database_passphrase_required);
        assert_eq!(report.databases[0].storage_family, StorageFamily::SQLite);
        assert!(!report.databases[0].passphrase_required);
        assert_eq!(
            report.databases[1].storage_family,
            StorageFamily::WcdbSqlcipher4
        );
        assert!(report.databases[1].passphrase_required);
    }

    #[test]
    fn preflight_rejects_a_symlinked_snapshot_entry() {
        let directory = tempfile::tempdir().unwrap();
        let mut plaintext = SQLITE_HEADER.to_vec();
        plaintext.extend([0_u8; 48]);
        write_legacy_snapshot(directory.path(), std::slice::from_ref(&plaintext));
        let database = directory.path().join("sets/0000/database.db");
        fs::remove_file(&database).unwrap();
        let outside = directory.path().join("outside.db");
        fs::write(&outside, plaintext).unwrap();
        std::os::unix::fs::symlink(outside, database).unwrap();

        let error = preflight_snapshot(directory.path()).unwrap_err();
        assert!(matches!(error, RestoreError::Integrity(value) if value == "opaque-0"));
    }

    #[test]
    fn available_key_selection_retains_explicit_missing_database_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let mut plaintext = SQLITE_HEADER.to_vec();
        plaintext.resize(wx_decrypt::MACOS_4_1_7_31.page_size, 0);
        let encrypted = vec![0x42_u8; wx_decrypt::MACOS_4_1_7_31.page_size];
        write_legacy_snapshot(directory.path(), &[plaintext, encrypted]);
        let key_file = directory.path().join("keys.json");
        fs::write(
            &key_file,
            serde_json::to_vec(&serde_json::json!({
                "unrelated/database.db": {
                    "enc_key": "11".repeat(32),
                    "salt": "42".repeat(16)
                }
            }))
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600)).unwrap();
        let keys = DatabaseKeySet::load(&key_file).unwrap();
        let manifest = SnapshotManifest::load(directory.path()).unwrap();
        let entries = manifest.database_entries().collect::<Vec<_>>();

        let (available, summary) = select_available_exported_keys(
            directory.path(),
            &manifest,
            entries,
            &keys,
            &NoProgress,
        )
        .unwrap();

        assert_eq!(available.len(), 1);
        assert_eq!(available[0].logical_path, "message/message_0.db");
        assert_eq!(summary.total_database_count, 2);
        assert_eq!(summary.selected_database_count, 1);
        assert_eq!(summary.unavailable_database_count, 1);
        assert_eq!(summary.selected_source_set_ids, ["set-0"]);
        assert_eq!(summary.unavailable_databases.len(), 1);
        assert_eq!(summary.unavailable_databases[0].source_set_id, "set-1");
        assert_eq!(
            summary.unavailable_databases[0].logical_path,
            "message/message_1.db"
        );
        assert_eq!(
            summary.unavailable_databases[0].reason,
            "noExportedKeyAuthenticated"
        );
    }

    fn write_legacy_snapshot(snapshot: &Path, databases: &[Vec<u8>]) {
        let entries = databases
            .iter()
            .enumerate()
            .map(|(index, bytes)| {
                let relative_path = format!("sets/{index:04}/database.db");
                let path = snapshot.join(&relative_path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, bytes).unwrap();
                SnapshotEntry {
                    source: PathReference {
                        opaque_id: format!("opaque-{index}"),
                        path: None,
                    },
                    source_set_id: format!("set-{index}"),
                    logical_path: format!("message/message_{index}.db"),
                    relative_path,
                    role: SnapshotFileRole::Database,
                    fingerprint: SourceFileFingerprint {
                        device_id: 1,
                        file_id: index as u64 + 1,
                        byte_count: bytes.len() as i64,
                        modified_seconds: 0,
                        modified_nanoseconds: 0,
                    },
                    sha256: hex::encode(Sha256::digest(bytes)),
                }
            })
            .collect();
        let manifest = SnapshotManifest {
            manifest_format_version: 1,
            snapshot_id: "synthetic-preflight".to_string(),
            created_at: "2026-08-27T00:00:00Z".to_string(),
            source_fingerprint: "synthetic-source".to_string(),
            account_binding: None,
            client_build: None,
            acquisition: None,
            entries,
        };
        fs::write(
            snapshot.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }
}
