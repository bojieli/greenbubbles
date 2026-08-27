use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::manifest::{SnapshotFileRole, SnapshotManifest};
use crate::{DatabasePassphrase, RestoreError};

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
    #[serde(skip)]
    pub path: PathBuf,
}

pub struct PreparedCatalog {
    pub manifest: SnapshotManifest,
    pub databases: Vec<PreparedDatabase>,
    _temporary_directory: TempDir,
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
    let manifest = SnapshotManifest::load(snapshot_dir)?;
    let mut databases = Vec::new();
    let mut copied_storage_family_counts = BTreeMap::new();

    for entry in &manifest.entries {
        verify_snapshot_entry(snapshot_dir, entry)?;
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
    let manifest = SnapshotManifest::load(snapshot_dir)?;
    let output = tempfile::Builder::new()
        .prefix("greenbubbles-plain-")
        .tempdir()?;
    set_owner_only(output.path())?;

    for entry in &manifest.entries {
        verify_snapshot_entry(snapshot_dir, entry)?;
    }

    let mut databases = Vec::new();
    for entry in manifest.database_entries() {
        let source = entry.resolved_path(snapshot_dir)?;
        let header = read_header_safely(&source, &entry.source.opaque_id)?;
        let family = if &header == SQLITE_HEADER {
            StorageFamily::SQLite
        } else {
            StorageFamily::WcdbSqlcipher4
        };
        let destination = output.path().join(format!("{}.db", entry.source_set_id));

        match family {
            StorageFamily::SQLite => fs::copy(&source, &destination).map(|_| ())?,
            StorageFamily::WcdbSqlcipher4 => {
                let key = passphrase
                    .ok_or_else(|| RestoreError::PassphraseRequired(entry.source_set_id.clone()))?;
                wx_decrypt::decrypt_db(
                    &source,
                    &destination,
                    key.expose_for_database_operation(),
                    &wx_decrypt::MACOS_4_1_7_31,
                )
                .map_err(|e| RestoreError::Decryption {
                    set_id: entry.source_set_id.clone(),
                    reason: e.to_string(),
                })?;
                if let Some(wal) =
                    manifest.sidecar(&entry.source_set_id, SnapshotFileRole::WriteAheadLog)
                {
                    let wal_source = wal.resolved_path(snapshot_dir)?;
                    wx_decrypt::decrypt_wal(
                        &wal_source,
                        &destination,
                        key.expose_for_database_operation(),
                        &wx_decrypt::MACOS_4_1_7_31,
                    )
                    .map_err(|e| RestoreError::Decryption {
                        set_id: entry.source_set_id.clone(),
                        reason: format!("WAL: {e}"),
                    })?;
                }
            }
        }
        set_owner_only_file(&destination)?;
        let tables = inspect_tables(&destination)?;
        databases.push(PreparedDatabase {
            source_set_id: entry.source_set_id.clone(),
            logical_path: entry.logical_path.clone(),
            storage_family: family,
            table_count: tables.len(),
            tables,
            path: destination,
        });
    }
    databases.sort_by(|a, b| {
        (&a.logical_path, &a.source_set_id).cmp(&(&b.logical_path, &b.source_set_id))
    });
    Ok(PreparedCatalog {
        manifest,
        databases,
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

fn verify_snapshot_entry(
    snapshot_dir: &Path,
    entry: &crate::manifest::SnapshotEntry,
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
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    if !hex::encode(hasher.finalize()).eq_ignore_ascii_case(&entry.sha256) {
        return Err(RestoreError::Integrity(entry.source.opaque_id.clone()));
    }
    Ok(())
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
