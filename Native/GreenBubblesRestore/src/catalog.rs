use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
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
        if !source.is_file() {
            return Err(RestoreError::MissingEntry(source));
        }
        let header = read_header(&source)?;
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

fn read_header(path: &Path) -> Result<[u8; 16], RestoreError> {
    let mut file = fs::File::open(path)?;
    let mut result = [0u8; 16];
    file.read_exact(&mut result)?;
    Ok(result)
}

fn verify_snapshot_entry(
    snapshot_dir: &Path,
    entry: &crate::manifest::SnapshotEntry,
) -> Result<(), RestoreError> {
    let path = entry.resolved_path(snapshot_dir)?;
    let metadata = fs::metadata(&path).map_err(|_| RestoreError::MissingEntry(path.clone()))?;
    if metadata.len() != entry.fingerprint.byte_count.max(0) as u64 {
        return Err(RestoreError::Integrity(entry.source.opaque_id.clone()));
    }
    let mut file = fs::File::open(&path)?;
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
