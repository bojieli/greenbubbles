use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::backup::Backup;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use walkdir::WalkDir;
use zeroize::{Zeroize, Zeroizing};

use crate::catalog::preflight_snapshot;
use crate::live_query::{LiveQueryError, LiveQuerySource, QueryDatabaseAccess};
use crate::manifest::SnapshotManifest;
use crate::snapshot_protector::{
    unwrap_snapshot_database_key, unwrap_snapshot_database_key_with_local_credential,
    unwrap_snapshot_database_key_with_passphrase, validate_wrapped_snapshot_key,
    wrap_new_snapshot_database_key, wrap_snapshot_database_key_with_local_credential,
    wrap_snapshot_database_key_with_passphrase, wrap_snapshot_database_key_with_recovery_words,
    SnapshotLocalCredential, SnapshotPassphrase, SnapshotProtectorError, SnapshotRecoveryWords,
    WrappedSnapshotKey,
};
use crate::SnapshotKey;

pub const RECOVERABLE_SNAPSHOT_SCHEMA: &str = "greenbubbles.recoverable-snapshot.v1";
pub const RECOVERABLE_SNAPSHOT_FORMAT_VERSION: u32 = 1;
pub const RECOVERABLE_SNAPSHOT_WRAPPED_SCHEMA: &str = "greenbubbles.recoverable-snapshot.v2";
pub const RECOVERABLE_SNAPSHOT_WRAPPED_FORMAT_VERSION: u32 = 2;
pub const SNAPSHOT_RETENTION_SCHEMA: &str = "greenbubbles.snapshot-retention.v1";
pub const SNAPSHOT_RETENTION_FORMAT_VERSION: u32 = 1;

const MANIFEST_FILE_NAME: &str = "manifest.json";
const DATA_DIRECTORY_NAME: &str = "data";
const MAXIMUM_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

#[derive(Debug, Error)]
pub enum RecoverableSnapshotError {
    #[error("invalid recoverable snapshot request: {0}")]
    InvalidArgument(String),
    #[error("unsafe recoverable snapshot path: {0}")]
    UnsafePath(String),
    #[error("recoverable snapshot database {logical_path} failed: {reason}")]
    Database {
        logical_path: String,
        reason: String,
    },
    #[error("recoverable snapshot integrity failed: {0}")]
    Integrity(String),
    #[error("recoverable snapshot I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("recoverable snapshot JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("recoverable snapshot SQLite failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    LiveQuery(#[from] LiveQueryError),
    #[error(transparent)]
    Protector(#[from] SnapshotProtectorError),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SnapshotSourceMode {
    LiveEncrypted,
    StableAcquisitionSnapshot,
    RecoverableSnapshot,
    Decrypted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SnapshotConsistency {
    pub guarantee: String,
    pub database_count: usize,
    pub cross_database_atomic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SnapshotProtection {
    pub database_encryption: String,
    pub recovery_protector: String,
    pub independent_of_wechat_key: bool,
    pub plaintext_database_files: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protectors: Vec<WrappedSnapshotKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RecoverableSnapshotDatabase {
    pub relative_path: String,
    pub byte_count: u64,
    pub page_count: u64,
    pub sha256: String,
    pub sqlite_integrity_check: String,
    pub encrypted_at_rest: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RecoverableSnapshotManifest {
    pub schema: String,
    pub format_version: u32,
    pub snapshot_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_snapshot_id: Option<String>,
    pub created_at_unix_milliseconds: u64,
    pub source_identity: String,
    pub source_mode: SnapshotSourceMode,
    pub consistency: SnapshotConsistency,
    pub protection: SnapshotProtection,
    pub recovery_verified: bool,
    pub databases: Vec<RecoverableSnapshotDatabase>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverableSnapshotVerificationReport {
    pub schema: String,
    pub format_version: u32,
    pub snapshot_id: String,
    pub recovery_verified_without_wechat_key: bool,
    pub independent_of_wechat_key: bool,
    pub encrypted_at_rest: bool,
    pub database_count: usize,
    pub total_database_bytes: u64,
    pub sqlite_integrity_verified: bool,
    pub manifest_hashes_verified: bool,
    pub inventory_complete: bool,
    pub recovery_protector: String,
    pub protector_count: usize,
    pub portable_recovery_protector_verified: bool,
    pub local_convenience_protector_count: usize,
    pub passphrase_protector_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRetentionReport {
    pub schema: &'static str,
    pub format_version: u32,
    pub operation: &'static str,
    pub retiring_snapshot_id: String,
    pub replacement_snapshot_id: Option<String>,
    pub retiring_recovery_verified: bool,
    pub replacement_portable_recovery_verified: bool,
    pub whole_generation: bool,
    pub recoverable_move: bool,
}

pub fn create_recoverable_snapshot(
    source_root: &Path,
    source_access: QueryDatabaseAccess<'_>,
    output_directory: &Path,
    snapshot_key: &SnapshotKey,
) -> Result<RecoverableSnapshotManifest, RecoverableSnapshotError> {
    create_recoverable_snapshot_with_protection(
        source_root,
        source_access,
        output_directory,
        SnapshotOutputProtection::LegacyRawKey(snapshot_key),
    )
}

pub fn create_recoverable_snapshot_with_recovery_words(
    source_root: &Path,
    source_access: QueryDatabaseAccess<'_>,
    output_directory: &Path,
    recovery_words: &SnapshotRecoveryWords,
) -> Result<RecoverableSnapshotManifest, RecoverableSnapshotError> {
    create_recoverable_snapshot_with_protection(
        source_root,
        source_access,
        output_directory,
        SnapshotOutputProtection::RecoveryWords {
            words: recovery_words,
            local_credential: None,
            passphrase: None,
        },
    )
}

pub fn create_recoverable_snapshot_with_recovery_words_and_local_credential(
    source_root: &Path,
    source_access: QueryDatabaseAccess<'_>,
    output_directory: &Path,
    recovery_words: &SnapshotRecoveryWords,
    local_credential: &SnapshotLocalCredential,
) -> Result<RecoverableSnapshotManifest, RecoverableSnapshotError> {
    create_recoverable_snapshot_with_protection(
        source_root,
        source_access,
        output_directory,
        SnapshotOutputProtection::RecoveryWords {
            words: recovery_words,
            local_credential: Some(local_credential),
            passphrase: None,
        },
    )
}

pub fn create_recoverable_snapshot_with_recovery_words_and_optional_protectors(
    source_root: &Path,
    source_access: QueryDatabaseAccess<'_>,
    output_directory: &Path,
    recovery_words: &SnapshotRecoveryWords,
    local_credential: Option<&SnapshotLocalCredential>,
    passphrase: Option<&SnapshotPassphrase>,
) -> Result<RecoverableSnapshotManifest, RecoverableSnapshotError> {
    create_recoverable_snapshot_with_protection(
        source_root,
        source_access,
        output_directory,
        SnapshotOutputProtection::RecoveryWords {
            words: recovery_words,
            local_credential,
            passphrase,
        },
    )
}

fn create_recoverable_snapshot_with_protection(
    source_root: &Path,
    source_access: QueryDatabaseAccess<'_>,
    output_directory: &Path,
    output_protection: SnapshotOutputProtection<'_>,
) -> Result<RecoverableSnapshotManifest, RecoverableSnapshotError> {
    let source_mode = match source_access {
        QueryDatabaseAccess::LiveEncrypted(source_key) => {
            if output_protection.reuses_source_key(source_key) {
                return Err(RecoverableSnapshotError::InvalidArgument(
                    "source and snapshot recovery protector inputs must be distinct".into(),
                ));
            }
            SnapshotSourceMode::LiveEncrypted
        }
        QueryDatabaseAccess::Decrypted => SnapshotSourceMode::Decrypted,
        QueryDatabaseAccess::SnapshotEncrypted(source_key) => {
            if output_protection.reuses_source_key(source_key) {
                return Err(RecoverableSnapshotError::InvalidArgument(
                    "source and snapshot recovery protector inputs must be distinct".into(),
                ));
            }
            SnapshotSourceMode::RecoverableSnapshot
        }
    };
    let source = LiveQuerySource::open(source_root, source_access)?;
    let canonical_source = source_root.canonicalize().map_err(|_| {
        RecoverableSnapshotError::UnsafePath("source root could not be canonicalized".into())
    })?;
    let databases = inventory_source_databases(&canonical_source)?;
    let source_identity = source.identity().to_string();
    create_recoverable_snapshot_from_connections(
        &canonical_source,
        &source_identity,
        source_mode,
        "perDatabaseOnlineBackup",
        databases,
        output_directory,
        output_protection,
        |relative_path| source.open_database(relative_path).map_err(Into::into),
        || Ok(()),
    )
}

pub fn create_recoverable_snapshot_from_stable_capture(
    capture_directory: &Path,
    source_access: QueryDatabaseAccess<'_>,
    output_directory: &Path,
    snapshot_key: &SnapshotKey,
) -> Result<RecoverableSnapshotManifest, RecoverableSnapshotError> {
    create_recoverable_snapshot_from_stable_capture_with_protection(
        capture_directory,
        source_access,
        output_directory,
        SnapshotOutputProtection::LegacyRawKey(snapshot_key),
    )
}

pub fn create_recoverable_snapshot_from_stable_capture_with_recovery_words(
    capture_directory: &Path,
    source_access: QueryDatabaseAccess<'_>,
    output_directory: &Path,
    recovery_words: &SnapshotRecoveryWords,
) -> Result<RecoverableSnapshotManifest, RecoverableSnapshotError> {
    create_recoverable_snapshot_from_stable_capture_with_protection(
        capture_directory,
        source_access,
        output_directory,
        SnapshotOutputProtection::RecoveryWords {
            words: recovery_words,
            local_credential: None,
            passphrase: None,
        },
    )
}

pub fn create_recoverable_snapshot_from_stable_capture_with_recovery_words_and_local_credential(
    capture_directory: &Path,
    source_access: QueryDatabaseAccess<'_>,
    output_directory: &Path,
    recovery_words: &SnapshotRecoveryWords,
    local_credential: &SnapshotLocalCredential,
) -> Result<RecoverableSnapshotManifest, RecoverableSnapshotError> {
    create_recoverable_snapshot_from_stable_capture_with_protection(
        capture_directory,
        source_access,
        output_directory,
        SnapshotOutputProtection::RecoveryWords {
            words: recovery_words,
            local_credential: Some(local_credential),
            passphrase: None,
        },
    )
}

pub fn create_recoverable_snapshot_from_stable_capture_with_recovery_words_and_optional_protectors(
    capture_directory: &Path,
    source_access: QueryDatabaseAccess<'_>,
    output_directory: &Path,
    recovery_words: &SnapshotRecoveryWords,
    local_credential: Option<&SnapshotLocalCredential>,
    passphrase: Option<&SnapshotPassphrase>,
) -> Result<RecoverableSnapshotManifest, RecoverableSnapshotError> {
    create_recoverable_snapshot_from_stable_capture_with_protection(
        capture_directory,
        source_access,
        output_directory,
        SnapshotOutputProtection::RecoveryWords {
            words: recovery_words,
            local_credential,
            passphrase,
        },
    )
}

fn create_recoverable_snapshot_from_stable_capture_with_protection(
    capture_directory: &Path,
    source_access: QueryDatabaseAccess<'_>,
    output_directory: &Path,
    output_protection: SnapshotOutputProtection<'_>,
) -> Result<RecoverableSnapshotManifest, RecoverableSnapshotError> {
    match source_access {
        QueryDatabaseAccess::LiveEncrypted(source_key) => {
            if output_protection.reuses_source_key(source_key) {
                return Err(RecoverableSnapshotError::InvalidArgument(
                    "source and snapshot recovery protector inputs must be distinct".into(),
                ));
            }
        }
        QueryDatabaseAccess::Decrypted => {}
        QueryDatabaseAccess::SnapshotEncrypted(_) => {
            return Err(RecoverableSnapshotError::InvalidArgument(
                "stable capture conversion expects a WeChat key or explicit plaintext source"
                    .into(),
            ))
        }
    }
    let canonical_capture =
        validate_private_directory(capture_directory, "stable acquisition snapshot directory")?;
    let preflight = preflight_snapshot(&canonical_capture).map_err(|_| {
        RecoverableSnapshotError::Integrity(
            "stable acquisition snapshot failed complete manifest verification".into(),
        )
    })?;
    if preflight.copied_database_count != preflight.current_source_set_count {
        return Err(RecoverableSnapshotError::InvalidArgument(
            "stable acquisition snapshot is incremental or lacks the complete current database set"
                .into(),
        ));
    }
    let capture_inventory = capture_regular_file_inventory(&canonical_capture)?;
    let capture_manifest = SnapshotManifest::load(&canonical_capture).map_err(|_| {
        RecoverableSnapshotError::Integrity(
            "stable acquisition snapshot manifest could not be validated".into(),
        )
    })?;
    let mut captured_databases = BTreeMap::new();
    for entry in capture_manifest.database_entries() {
        let logical_path = validated_manifest_relative_path(&entry.logical_path)?;
        let source_path = entry.resolved_path(&canonical_capture).map_err(|_| {
            RecoverableSnapshotError::Integrity(
                "stable acquisition database path could not be resolved".into(),
            )
        })?;
        validate_private_capture_file(&source_path, "stable acquisition database")?;
        if captured_databases
            .insert(logical_path, source_path)
            .is_some()
        {
            return Err(RecoverableSnapshotError::Integrity(
                "stable acquisition snapshot contains duplicate logical database paths".into(),
            ));
        }
    }
    let databases = captured_databases.keys().cloned().collect::<Vec<_>>();
    let source_identity = stable_capture_identity(
        &preflight.snapshot_id,
        &preflight.source_fingerprint,
        databases.len(),
    );
    let expected_snapshot_id = preflight.snapshot_id;
    let expected_source_fingerprint = preflight.source_fingerprint;
    create_recoverable_snapshot_from_connections(
        &canonical_capture,
        &source_identity,
        SnapshotSourceMode::StableAcquisitionSnapshot,
        "stableAcquisitionSnapshotConversion",
        databases,
        output_directory,
        output_protection,
        |logical_path| {
            let source_path = captured_databases.get(logical_path).ok_or_else(|| {
                RecoverableSnapshotError::Integrity(
                    "stable acquisition database mapping changed during conversion".into(),
                )
            })?;
            open_stable_capture_database(source_path, source_access)
        },
        || {
            let verified = preflight_snapshot(&canonical_capture).map_err(|_| {
                RecoverableSnapshotError::Integrity(
                    "stable acquisition snapshot changed during conversion".into(),
                )
            })?;
            if verified.snapshot_id != expected_snapshot_id
                || verified.source_fingerprint != expected_source_fingerprint
                || verified.copied_database_count != verified.current_source_set_count
                || capture_regular_file_inventory(&canonical_capture)? != capture_inventory
            {
                return Err(RecoverableSnapshotError::Integrity(
                    "stable acquisition snapshot changed during conversion".into(),
                ));
            }
            Ok(())
        },
    )
}

fn open_stable_capture_database(
    path: &Path,
    source_access: QueryDatabaseAccess<'_>,
) -> Result<Connection, RecoverableSnapshotError> {
    validate_private_capture_file(path, "stable acquisition database")?;
    let connection = match source_access {
        QueryDatabaseAccess::LiveEncrypted(key) => wx_db::open_readonly_connection(path, Some(key))
            .map_err(|_| {
                RecoverableSnapshotError::Integrity(
                    "stable acquisition database could not be authenticated read-only".into(),
                )
            })?,
        QueryDatabaseAccess::Decrypted => {
            wx_db::open_readonly_connection(path, None).map_err(|_| {
                RecoverableSnapshotError::Integrity(
                    "stable acquisition plaintext database could not be opened read-only".into(),
                )
            })?
        }
        QueryDatabaseAccess::SnapshotEncrypted(_) => {
            return Err(RecoverableSnapshotError::InvalidArgument(
                "invalid stable acquisition source access mode".into(),
            ))
        }
    };
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute_batch("PRAGMA query_only = ON")?;
    if connection.query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))? != 1 {
        return Err(RecoverableSnapshotError::Integrity(
            "stable acquisition database query-only enforcement failed".into(),
        ));
    }
    wx_context::register_mm_fts_tokenizer(&connection).map_err(|_| {
        RecoverableSnapshotError::Integrity(
            "native WeChat FTS tokenizer registration failed for stable capture".into(),
        )
    })?;
    connection.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))?;
    Ok(connection)
}

fn validate_private_capture_file(
    path: &Path,
    description: &str,
) -> Result<(), RecoverableSnapshotError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        RecoverableSnapshotError::UnsafePath(format!("{description} is unavailable"))
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
    {
        return Err(RecoverableSnapshotError::UnsafePath(format!(
            "{description} must be a current-user-owned owner-only single-link regular file"
        )));
    }
    Ok(())
}

fn capture_regular_file_inventory(
    capture_root: &Path,
) -> Result<BTreeSet<PathBuf>, RecoverableSnapshotError> {
    let mut inventory = BTreeSet::new();
    for entry in WalkDir::new(capture_root).follow_links(false) {
        let entry = entry.map_err(|_| {
            RecoverableSnapshotError::UnsafePath(
                "stable acquisition snapshot inventory could not be traversed".into(),
            )
        })?;
        if entry.file_type().is_symlink() {
            return Err(RecoverableSnapshotError::UnsafePath(
                "stable acquisition snapshot contains a symbolic link".into(),
            ));
        }
        if !entry.file_type().is_file() {
            continue;
        }
        validate_private_capture_file(entry.path(), "stable acquisition file")?;
        let relative = entry.path().strip_prefix(capture_root).map_err(|_| {
            RecoverableSnapshotError::UnsafePath(
                "stable acquisition file escaped its snapshot root".into(),
            )
        })?;
        validate_relative_path(relative)?;
        inventory.insert(relative.to_path_buf());
    }
    Ok(inventory)
}

fn stable_capture_identity(snapshot_id: &str, source_fingerprint: &str, count: usize) -> String {
    let mut digest = Sha256::new();
    digest.update(b"greenbubbles-stable-acquisition-source-v1\0");
    digest.update(snapshot_id.as_bytes());
    digest.update([0]);
    digest.update(source_fingerprint.as_bytes());
    digest.update((count as u64).to_le_bytes());
    format!("sha256:{}", hex::encode(&digest.finalize()[..16]))
}

#[derive(Clone, Copy)]
enum SnapshotOutputProtection<'a> {
    LegacyRawKey(&'a SnapshotKey),
    RecoveryWords {
        words: &'a SnapshotRecoveryWords,
        local_credential: Option<&'a SnapshotLocalCredential>,
        passphrase: Option<&'a SnapshotPassphrase>,
    },
}

impl SnapshotOutputProtection<'_> {
    fn reuses_source_key(self, source_key: &[u8; 32]) -> bool {
        match self {
            Self::LegacyRawKey(snapshot_key) => {
                source_key == snapshot_key.expose_for_snapshot_operation()
            }
            Self::RecoveryWords { words, .. } => source_key == words.entropy(),
        }
    }

    fn prepare(
        self,
        snapshot_id: &str,
    ) -> Result<PreparedSnapshotOutputProtection, RecoverableSnapshotError> {
        match self {
            Self::LegacyRawKey(snapshot_key) => Ok(PreparedSnapshotOutputProtection {
                schema: RECOVERABLE_SNAPSHOT_SCHEMA,
                format_version: RECOVERABLE_SNAPSHOT_FORMAT_VERSION,
                database_key: SnapshotKey::from_bytes(
                    *snapshot_key.expose_for_snapshot_operation(),
                ),
                manifest: SnapshotProtection {
                    database_encryption: "sqlcipher4RawKey".into(),
                    recovery_protector: "portable256BitRecoveryKey".into(),
                    independent_of_wechat_key: true,
                    plaintext_database_files: false,
                    protectors: Vec::new(),
                },
            }),
            Self::RecoveryWords {
                words,
                local_credential,
                passphrase,
            } => {
                let (database_key, protector) = wrap_new_snapshot_database_key(snapshot_id, words)?;
                let mut protectors = vec![protector];
                if let Some(local_credential) = local_credential {
                    protectors.push(wrap_snapshot_database_key_with_local_credential(
                        snapshot_id,
                        &database_key,
                        local_credential,
                    )?);
                }
                if let Some(passphrase) = passphrase {
                    protectors.push(wrap_snapshot_database_key_with_passphrase(
                        snapshot_id,
                        &database_key,
                        passphrase,
                    )?);
                }
                Ok(PreparedSnapshotOutputProtection {
                    schema: RECOVERABLE_SNAPSHOT_WRAPPED_SCHEMA,
                    format_version: RECOVERABLE_SNAPSHOT_WRAPPED_FORMAT_VERSION,
                    database_key,
                    manifest: SnapshotProtection {
                        database_encryption: "sqlcipher4RawKey".into(),
                        recovery_protector: "multiProtectorEnvelopeV1".into(),
                        independent_of_wechat_key: true,
                        plaintext_database_files: false,
                        protectors,
                    },
                })
            }
        }
    }
}

struct PreparedSnapshotOutputProtection {
    schema: &'static str,
    format_version: u32,
    database_key: SnapshotKey,
    manifest: SnapshotProtection,
}

#[allow(clippy::too_many_arguments)]
fn create_recoverable_snapshot_from_connections<OpenDatabase, ValidateStableSource>(
    canonical_source: &Path,
    source_identity: &str,
    source_mode: SnapshotSourceMode,
    consistency_guarantee: &str,
    databases: Vec<PathBuf>,
    output_directory: &Path,
    output_protection: SnapshotOutputProtection<'_>,
    mut open_database: OpenDatabase,
    mut validate_stable_source: ValidateStableSource,
) -> Result<RecoverableSnapshotManifest, RecoverableSnapshotError>
where
    OpenDatabase: FnMut(&Path) -> Result<Connection, RecoverableSnapshotError>,
    ValidateStableSource: FnMut() -> Result<(), RecoverableSnapshotError>,
{
    if databases.is_empty() {
        return Err(RecoverableSnapshotError::InvalidArgument(
            "source contains no regular .db files".into(),
        ));
    }
    require_core_database(&databases, "contact/contact.db")?;
    require_core_database(&databases, "session/session.db")?;
    let (output_parent, final_output) =
        validate_new_output_directory(output_directory, canonical_source)?;
    let staging = tempfile::Builder::new()
        .prefix(".greenbubbles-recoverable-snapshot-")
        .tempdir_in(&output_parent)?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))?;
    let data_root = staging.path().join(DATA_DIRECTORY_NAME);
    create_private_directory(&data_root)?;

    let snapshot_id = generate_snapshot_id(source_identity, databases.len());
    let prepared_protection = output_protection.prepare(&snapshot_id)?;
    let mut manifest_databases = Vec::with_capacity(databases.len());
    for relative_path in &databases {
        let destination = data_root.join(relative_path);
        let parent = destination.parent().ok_or_else(|| {
            RecoverableSnapshotError::UnsafePath(
                "snapshot database has no destination parent".into(),
            )
        })?;
        create_private_directories_below(&data_root, parent)?;
        let source_connection = open_database(relative_path)?;
        copy_database_logically(
            &source_connection,
            &destination,
            &prepared_protection.database_key,
            &path_string(relative_path),
        )?;
        drop(source_connection);
        manifest_databases.push(verify_one_database(
            &data_root,
            relative_path,
            &prepared_protection.database_key,
        )?);
    }
    validate_stable_source()?;

    let manifest = RecoverableSnapshotManifest {
        schema: prepared_protection.schema.into(),
        format_version: prepared_protection.format_version,
        snapshot_id,
        parent_snapshot_id: None,
        created_at_unix_milliseconds: now_unix_milliseconds(),
        source_identity: source_identity.to_string(),
        source_mode,
        consistency: SnapshotConsistency {
            guarantee: consistency_guarantee.into(),
            database_count: manifest_databases.len(),
            cross_database_atomic: manifest_databases.len() <= 1,
        },
        protection: prepared_protection.manifest,
        recovery_verified: true,
        databases: manifest_databases,
    };
    write_manifest_create_new(staging.path(), &manifest)?;
    sync_directory_tree(&data_root)?;
    File::open(staging.path())?.sync_all()?;

    let staging_path = staging.keep();
    if let Err(error) = fs::rename(&staging_path, &final_output) {
        let _ = fs::remove_dir_all(&staging_path);
        return Err(error.into());
    }
    File::open(&output_parent)?.sync_all()?;

    let verification_result = match output_protection {
        SnapshotOutputProtection::LegacyRawKey(_) => {
            verify_recoverable_snapshot(&final_output, &prepared_protection.database_key)
        }
        SnapshotOutputProtection::RecoveryWords {
            words,
            local_credential,
            passphrase,
        } => (|| {
            let verification =
                verify_recoverable_snapshot_with_recovery_words(&final_output, words)?;
            if let Some(local_credential) = local_credential {
                let local_verification = verify_recoverable_snapshot_with_local_credential(
                    &final_output,
                    local_credential,
                )?;
                if local_verification.snapshot_id != verification.snapshot_id {
                    return Err(RecoverableSnapshotError::Integrity(
                        "local and portable protectors resolved different snapshots".into(),
                    ));
                }
            }
            if let Some(passphrase) = passphrase {
                let passphrase_verification =
                    verify_recoverable_snapshot_with_passphrase(&final_output, passphrase)?;
                if passphrase_verification.snapshot_id != verification.snapshot_id {
                    return Err(RecoverableSnapshotError::Integrity(
                        "passphrase and portable protectors resolved different snapshots".into(),
                    ));
                }
            }
            Ok(verification)
        })(),
    };
    let verification = match verification_result {
        Ok(verification) => verification,
        Err(error) => {
            let _ = fs::remove_dir_all(&final_output);
            let _ = File::open(&output_parent).and_then(|directory| directory.sync_all());
            return Err(error);
        }
    };
    if !verification.recovery_verified_without_wechat_key {
        let _ = fs::remove_dir_all(&final_output);
        let _ = File::open(&output_parent).and_then(|directory| directory.sync_all());
        return Err(RecoverableSnapshotError::Integrity(
            "published snapshot did not pass recovery verification".into(),
        ));
    }
    Ok(manifest)
}

pub fn rewrap_recoverable_snapshot_protectors(
    source_snapshot: &Path,
    existing_snapshot_key: &SnapshotKey,
    output_directory: &Path,
    new_recovery_words: &SnapshotRecoveryWords,
    new_local_credential: Option<&SnapshotLocalCredential>,
) -> Result<RecoverableSnapshotManifest, RecoverableSnapshotError> {
    rewrap_recoverable_snapshot_protectors_with_optional_protectors(
        source_snapshot,
        existing_snapshot_key,
        output_directory,
        new_recovery_words,
        new_local_credential,
        None,
    )
}

pub fn rewrap_recoverable_snapshot_protectors_with_optional_protectors(
    source_snapshot: &Path,
    existing_snapshot_key: &SnapshotKey,
    output_directory: &Path,
    new_recovery_words: &SnapshotRecoveryWords,
    new_local_credential: Option<&SnapshotLocalCredential>,
    new_passphrase: Option<&SnapshotPassphrase>,
) -> Result<RecoverableSnapshotManifest, RecoverableSnapshotError> {
    let source = validate_snapshot_directory(source_snapshot)?;
    let source_manifest = load_manifest_from_canonical_snapshot(&source)?;
    validate_manifest_contract(&source_manifest)?;
    if source_manifest.format_version != RECOVERABLE_SNAPSHOT_WRAPPED_FORMAT_VERSION {
        return Err(RecoverableSnapshotError::InvalidArgument(
            "protector rewrap requires a format-2 snapshot".into(),
        ));
    }
    let source_verification = verify_recoverable_snapshot(&source, existing_snapshot_key)?;
    if !source_verification.recovery_verified_without_wechat_key
        || !source_verification.portable_recovery_protector_verified
    {
        return Err(RecoverableSnapshotError::Integrity(
            "source snapshot did not pass independent recovery verification".into(),
        ));
    }

    let (output_parent, final_output) = validate_new_output_directory(output_directory, &source)?;
    let staging = tempfile::Builder::new()
        .prefix(".greenbubbles-protector-rewrap-")
        .tempdir_in(&output_parent)?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))?;
    let destination_data_root = staging.path().join(DATA_DIRECTORY_NAME);
    create_private_directory(&destination_data_root)?;
    let source_data_root = source.join(DATA_DIRECTORY_NAME);
    validate_private_directory(&source_data_root, "source snapshot data directory")?;

    let snapshot_id = generate_snapshot_id(
        &format!("protector-rewrap:{}", source_manifest.snapshot_id),
        source_manifest.databases.len(),
    );
    let mut protectors = vec![wrap_snapshot_database_key_with_recovery_words(
        &snapshot_id,
        existing_snapshot_key,
        new_recovery_words,
    )?];
    if let Some(local_credential) = new_local_credential {
        protectors.push(wrap_snapshot_database_key_with_local_credential(
            &snapshot_id,
            existing_snapshot_key,
            local_credential,
        )?);
    }
    if let Some(passphrase) = new_passphrase {
        protectors.push(wrap_snapshot_database_key_with_passphrase(
            &snapshot_id,
            existing_snapshot_key,
            passphrase,
        )?);
    }

    for database in &source_manifest.databases {
        let relative_path = validated_manifest_relative_path(&database.relative_path)?;
        let source_path = source_data_root.join(&relative_path);
        let destination_path = destination_data_root.join(&relative_path);
        let destination_parent = destination_path.parent().ok_or_else(|| {
            RecoverableSnapshotError::UnsafePath(
                "rewrapped database has no destination parent".into(),
            )
        })?;
        create_private_directories_below(&destination_data_root, destination_parent)?;
        copy_encrypted_database_bytes_unchanged(
            &source_path,
            &destination_path,
            database.byte_count,
            &database.sha256,
        )?;
    }

    let manifest = RecoverableSnapshotManifest {
        schema: RECOVERABLE_SNAPSHOT_WRAPPED_SCHEMA.into(),
        format_version: RECOVERABLE_SNAPSHOT_WRAPPED_FORMAT_VERSION,
        snapshot_id,
        parent_snapshot_id: Some(source_manifest.snapshot_id.clone()),
        created_at_unix_milliseconds: now_unix_milliseconds(),
        source_identity: format!("snapshot:{}", source_manifest.snapshot_id),
        source_mode: SnapshotSourceMode::RecoverableSnapshot,
        consistency: SnapshotConsistency {
            guarantee: "encryptedDatabaseByteCopyRewrap".into(),
            database_count: source_manifest.databases.len(),
            cross_database_atomic: source_manifest.databases.len() <= 1,
        },
        protection: SnapshotProtection {
            database_encryption: "sqlcipher4RawKey".into(),
            recovery_protector: "multiProtectorEnvelopeV1".into(),
            independent_of_wechat_key: true,
            plaintext_database_files: false,
            protectors,
        },
        recovery_verified: true,
        databases: source_manifest.databases.clone(),
    };
    validate_manifest_contract(&manifest)?;
    write_manifest_create_new(staging.path(), &manifest)?;
    sync_directory_tree(&destination_data_root)?;
    File::open(staging.path())?.sync_all()?;

    let staging_path = staging.keep();
    if let Err(error) = fs::rename(&staging_path, &final_output) {
        let _ = fs::remove_dir_all(&staging_path);
        return Err(error.into());
    }
    File::open(&output_parent)?.sync_all()?;

    let verification_result = (|| {
        let portable =
            verify_recoverable_snapshot_with_recovery_words(&final_output, new_recovery_words)?;
        if let Some(local_credential) = new_local_credential {
            let local =
                verify_recoverable_snapshot_with_local_credential(&final_output, local_credential)?;
            if portable.snapshot_id != local.snapshot_id {
                return Err(RecoverableSnapshotError::Integrity(
                    "new portable and local protectors resolved different snapshots".into(),
                ));
            }
        }
        if let Some(passphrase) = new_passphrase {
            let passphrase_verification =
                verify_recoverable_snapshot_with_passphrase(&final_output, passphrase)?;
            if portable.snapshot_id != passphrase_verification.snapshot_id {
                return Err(RecoverableSnapshotError::Integrity(
                    "new portable and passphrase protectors resolved different snapshots".into(),
                ));
            }
        }
        Ok(portable)
    })();
    if let Err(error) = verification_result {
        let _ = fs::remove_dir_all(&final_output);
        let _ = File::open(&output_parent).and_then(|directory| directory.sync_all());
        return Err(error);
    }
    Ok(manifest)
}

pub fn quarantine_recoverable_snapshot_generation(
    retiring_snapshot: &Path,
    retiring_snapshot_key: &SnapshotKey,
    replacement_snapshot: &Path,
    replacement_recovery_words: &SnapshotRecoveryWords,
    quarantine_directory: &Path,
) -> Result<SnapshotRetentionReport, RecoverableSnapshotError> {
    let retiring = validate_snapshot_directory(retiring_snapshot)?;
    let replacement = validate_snapshot_directory(replacement_snapshot)?;
    let quarantine =
        validate_private_directory(quarantine_directory, "snapshot quarantine directory")?;
    reject_nested_retention_paths(&retiring, &replacement, &quarantine)?;

    let retiring_verification = verify_recoverable_snapshot(&retiring, retiring_snapshot_key)?;
    let replacement_verification =
        verify_recoverable_snapshot_with_recovery_words(&replacement, replacement_recovery_words)?;
    let retiring_manifest = load_manifest_from_canonical_snapshot(&retiring)?;
    let replacement_manifest = load_manifest_from_canonical_snapshot(&replacement)?;
    validate_manifest_contract(&retiring_manifest)?;
    validate_manifest_contract(&replacement_manifest)?;
    let replacement_is_linked = replacement_manifest.parent_snapshot_id.as_deref()
        == Some(retiring_manifest.snapshot_id.as_str())
        || replacement_manifest.source_identity == retiring_manifest.source_identity;
    if !replacement_is_linked
        || replacement_manifest.created_at_unix_milliseconds
            <= retiring_manifest.created_at_unix_milliseconds
        || !replacement_verification.recovery_verified_without_wechat_key
        || !replacement_verification.portable_recovery_protector_verified
    {
        return Err(RecoverableSnapshotError::InvalidArgument(
            "replacement must be a newer linked generation verified through portable recovery"
                .into(),
        ));
    }
    if !retiring_verification.recovery_verified_without_wechat_key {
        return Err(RecoverableSnapshotError::Integrity(
            "retiring generation did not pass recovery verification".into(),
        ));
    }

    require_same_filesystem(&retiring, &quarantine)?;
    let destination = quarantine.join(format!(
        "retired-{:020}-{}",
        retiring_manifest.created_at_unix_milliseconds, retiring_manifest.snapshot_id
    ));
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(RecoverableSnapshotError::UnsafePath(
            "snapshot quarantine destination already exists".into(),
        ));
    }
    let original_parent = retiring.parent().ok_or_else(|| {
        RecoverableSnapshotError::UnsafePath("retiring snapshot has no parent".into())
    })?;
    fs::rename(&retiring, &destination)?;
    sync_retention_move_parents(original_parent, &quarantine)?;

    if let Err(error) = verify_recoverable_snapshot(&destination, retiring_snapshot_key) {
        if fs::rename(&destination, &retiring).is_err()
            || sync_retention_move_parents(&quarantine, original_parent).is_err()
        {
            return Err(RecoverableSnapshotError::Integrity(
                "quarantine verification failed and the automatic rollback also failed".into(),
            ));
        }
        return Err(error);
    }

    Ok(SnapshotRetentionReport {
        schema: SNAPSHOT_RETENTION_SCHEMA,
        format_version: SNAPSHOT_RETENTION_FORMAT_VERSION,
        operation: "quarantine",
        retiring_snapshot_id: retiring_manifest.snapshot_id,
        replacement_snapshot_id: Some(replacement_manifest.snapshot_id),
        retiring_recovery_verified: true,
        replacement_portable_recovery_verified: true,
        whole_generation: true,
        recoverable_move: true,
    })
}

pub fn restore_quarantined_snapshot_generation(
    quarantined_snapshot: &Path,
    snapshot_key: &SnapshotKey,
    restored_directory: &Path,
) -> Result<SnapshotRetentionReport, RecoverableSnapshotError> {
    let quarantined = validate_snapshot_directory(quarantined_snapshot)?;
    let verification = verify_recoverable_snapshot(&quarantined, snapshot_key)?;
    if !verification.recovery_verified_without_wechat_key {
        return Err(RecoverableSnapshotError::Integrity(
            "quarantined generation did not pass recovery verification".into(),
        ));
    }
    let manifest = load_manifest_from_canonical_snapshot(&quarantined)?;
    validate_manifest_contract(&manifest)?;
    let (restored_parent, restored) =
        validate_new_output_directory(restored_directory, &quarantined)?;
    require_same_filesystem(&quarantined, &restored_parent)?;
    let quarantine_parent = quarantined.parent().ok_or_else(|| {
        RecoverableSnapshotError::UnsafePath("quarantined snapshot has no parent".into())
    })?;
    fs::rename(&quarantined, &restored)?;
    sync_retention_move_parents(quarantine_parent, &restored_parent)?;

    if let Err(error) = verify_recoverable_snapshot(&restored, snapshot_key) {
        if fs::rename(&restored, &quarantined).is_err()
            || sync_retention_move_parents(&restored_parent, quarantine_parent).is_err()
        {
            return Err(RecoverableSnapshotError::Integrity(
                "restored-generation verification failed and the automatic rollback also failed"
                    .into(),
            ));
        }
        return Err(error);
    }

    Ok(SnapshotRetentionReport {
        schema: SNAPSHOT_RETENTION_SCHEMA,
        format_version: SNAPSHOT_RETENTION_FORMAT_VERSION,
        operation: "restore",
        retiring_snapshot_id: manifest.snapshot_id,
        replacement_snapshot_id: None,
        retiring_recovery_verified: true,
        replacement_portable_recovery_verified: false,
        whole_generation: true,
        recoverable_move: true,
    })
}

pub fn rekey_recoverable_snapshot(
    source_snapshot: &Path,
    old_snapshot_key: &SnapshotKey,
    output_directory: &Path,
    new_snapshot_key: &SnapshotKey,
) -> Result<RecoverableSnapshotManifest, RecoverableSnapshotError> {
    if old_snapshot_key.expose_for_snapshot_operation()
        == new_snapshot_key.expose_for_snapshot_operation()
    {
        return Err(RecoverableSnapshotError::InvalidArgument(
            "old and new snapshot recovery key inputs must be distinct".into(),
        ));
    }
    let source_verification = verify_recoverable_snapshot(source_snapshot, old_snapshot_key)?;
    if !source_verification.recovery_verified_without_wechat_key {
        return Err(RecoverableSnapshotError::Integrity(
            "source snapshot did not pass recovery verification".into(),
        ));
    }
    let source_data_root = recoverable_snapshot_data_root(source_snapshot)?;
    create_recoverable_snapshot(
        &source_data_root,
        QueryDatabaseAccess::SnapshotEncrypted(old_snapshot_key.expose_for_snapshot_operation()),
        output_directory,
        new_snapshot_key,
    )
}

pub fn verify_recoverable_snapshot(
    snapshot_directory: &Path,
    snapshot_key: &SnapshotKey,
) -> Result<RecoverableSnapshotVerificationReport, RecoverableSnapshotError> {
    let snapshot = validate_snapshot_directory(snapshot_directory)?;
    let manifest = load_manifest_from_canonical_snapshot(&snapshot)?;
    validate_manifest_contract(&manifest)?;
    let data_root = snapshot.join(DATA_DIRECTORY_NAME);
    validate_private_directory(&data_root, "snapshot data directory")?;

    let actual_inventory = inventory_snapshot_databases(&data_root)?;
    let declared_inventory = manifest
        .databases
        .iter()
        .map(|database| validated_manifest_relative_path(&database.relative_path))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if actual_inventory != declared_inventory {
        return Err(RecoverableSnapshotError::Integrity(
            "snapshot database inventory does not exactly match its manifest".into(),
        ));
    }

    let mut total_database_bytes = 0u64;
    for declared in &manifest.databases {
        let relative_path = validated_manifest_relative_path(&declared.relative_path)?;
        let verified = verify_one_database(&data_root, &relative_path, snapshot_key)?;
        if verified.byte_count != declared.byte_count
            || verified.page_count != declared.page_count
            || verified.sha256 != declared.sha256
            || verified.sqlite_integrity_check != declared.sqlite_integrity_check
            || !declared.encrypted_at_rest
        {
            return Err(RecoverableSnapshotError::Integrity(format!(
                "database {} disagrees with its recovery manifest",
                declared.relative_path
            )));
        }
        total_database_bytes = total_database_bytes
            .checked_add(verified.byte_count)
            .ok_or_else(|| {
                RecoverableSnapshotError::Integrity(
                    "snapshot database byte total overflowed".into(),
                )
            })?;
    }

    Ok(RecoverableSnapshotVerificationReport {
        schema: manifest.schema.clone(),
        format_version: manifest.format_version,
        snapshot_id: manifest.snapshot_id,
        recovery_verified_without_wechat_key: true,
        independent_of_wechat_key: manifest.protection.independent_of_wechat_key,
        encrypted_at_rest: !manifest.protection.plaintext_database_files
            && manifest
                .databases
                .iter()
                .all(|database| database.encrypted_at_rest),
        database_count: manifest.databases.len(),
        total_database_bytes,
        sqlite_integrity_verified: true,
        manifest_hashes_verified: true,
        inventory_complete: true,
        recovery_protector: manifest.protection.recovery_protector.clone(),
        protector_count: manifest.protection.protectors.len(),
        portable_recovery_protector_verified: manifest.format_version
            == RECOVERABLE_SNAPSHOT_FORMAT_VERSION
            || manifest
                .protection
                .protectors
                .iter()
                .any(|protector| protector.kind == "bip39English24" && protector.portable),
        local_convenience_protector_count: manifest
            .protection
            .protectors
            .iter()
            .filter(|protector| protector.kind == "localCredentialV1" && !protector.portable)
            .count(),
        passphrase_protector_count: manifest
            .protection
            .protectors
            .iter()
            .filter(|protector| protector.kind == "argon2idPassphraseV1" && protector.portable)
            .count(),
    })
}

pub fn unlock_recoverable_snapshot_with_recovery_words(
    snapshot_directory: &Path,
    recovery_words: &SnapshotRecoveryWords,
) -> Result<SnapshotKey, RecoverableSnapshotError> {
    let snapshot = validate_snapshot_directory(snapshot_directory)?;
    let manifest = load_manifest_from_canonical_snapshot(&snapshot)?;
    validate_manifest_contract(&manifest)?;
    if manifest.format_version != RECOVERABLE_SNAPSHOT_WRAPPED_FORMAT_VERSION {
        return Err(RecoverableSnapshotError::InvalidArgument(
            "snapshot does not contain a BIP-39 recovery-word protector".into(),
        ));
    }
    let portable = manifest
        .protection
        .protectors
        .iter()
        .find(|protector| protector.kind == "bip39English24" && protector.portable)
        .ok_or_else(|| {
            RecoverableSnapshotError::Integrity(
                "snapshot has no portable recovery-word protector".into(),
            )
        })?;
    Ok(unwrap_snapshot_database_key(
        &manifest.snapshot_id,
        portable,
        recovery_words,
    )?)
}

pub fn verify_recoverable_snapshot_with_recovery_words(
    snapshot_directory: &Path,
    recovery_words: &SnapshotRecoveryWords,
) -> Result<RecoverableSnapshotVerificationReport, RecoverableSnapshotError> {
    let snapshot_key =
        unlock_recoverable_snapshot_with_recovery_words(snapshot_directory, recovery_words)?;
    let report = verify_recoverable_snapshot(snapshot_directory, &snapshot_key)?;
    if report.format_version != RECOVERABLE_SNAPSHOT_WRAPPED_FORMAT_VERSION
        || report.protector_count == 0
        || !report.portable_recovery_protector_verified
    {
        return Err(RecoverableSnapshotError::Integrity(
            "portable recovery-word verification did not prove the wrapped snapshot contract"
                .into(),
        ));
    }
    Ok(report)
}

pub fn unlock_recoverable_snapshot_with_local_credential(
    snapshot_directory: &Path,
    local_credential: &SnapshotLocalCredential,
) -> Result<SnapshotKey, RecoverableSnapshotError> {
    let snapshot = validate_snapshot_directory(snapshot_directory)?;
    let manifest = load_manifest_from_canonical_snapshot(&snapshot)?;
    validate_manifest_contract(&manifest)?;
    if manifest.format_version != RECOVERABLE_SNAPSHOT_WRAPPED_FORMAT_VERSION {
        return Err(RecoverableSnapshotError::InvalidArgument(
            "snapshot does not contain a local convenience protector".into(),
        ));
    }
    manifest
        .protection
        .protectors
        .iter()
        .filter(|protector| protector.kind == "localCredentialV1" && !protector.portable)
        .find_map(|protector| {
            unwrap_snapshot_database_key_with_local_credential(
                &manifest.snapshot_id,
                protector,
                local_credential,
            )
            .ok()
        })
        .ok_or_else(|| {
            RecoverableSnapshotError::Integrity(
                "snapshot has no protector matching this local credential".into(),
            )
        })
}

pub fn verify_recoverable_snapshot_with_local_credential(
    snapshot_directory: &Path,
    local_credential: &SnapshotLocalCredential,
) -> Result<RecoverableSnapshotVerificationReport, RecoverableSnapshotError> {
    let snapshot_key =
        unlock_recoverable_snapshot_with_local_credential(snapshot_directory, local_credential)?;
    let report = verify_recoverable_snapshot(snapshot_directory, &snapshot_key)?;
    if report.format_version != RECOVERABLE_SNAPSHOT_WRAPPED_FORMAT_VERSION
        || report.local_convenience_protector_count == 0
        || !report.portable_recovery_protector_verified
    {
        return Err(RecoverableSnapshotError::Integrity(
            "local convenience verification did not preserve portable recovery".into(),
        ));
    }
    Ok(report)
}

pub fn unlock_recoverable_snapshot_with_passphrase(
    snapshot_directory: &Path,
    passphrase: &SnapshotPassphrase,
) -> Result<SnapshotKey, RecoverableSnapshotError> {
    let snapshot = validate_snapshot_directory(snapshot_directory)?;
    let manifest = load_manifest_from_canonical_snapshot(&snapshot)?;
    validate_manifest_contract(&manifest)?;
    if manifest.format_version != RECOVERABLE_SNAPSHOT_WRAPPED_FORMAT_VERSION {
        return Err(RecoverableSnapshotError::InvalidArgument(
            "snapshot does not contain an Argon2id passphrase protector".into(),
        ));
    }
    manifest
        .protection
        .protectors
        .iter()
        .filter(|protector| protector.kind == "argon2idPassphraseV1" && protector.portable)
        .find_map(|protector| {
            unwrap_snapshot_database_key_with_passphrase(
                &manifest.snapshot_id,
                protector,
                passphrase,
            )
            .ok()
        })
        .ok_or_else(|| {
            RecoverableSnapshotError::Integrity(
                "snapshot has no protector matching this passphrase".into(),
            )
        })
}

pub fn verify_recoverable_snapshot_with_passphrase(
    snapshot_directory: &Path,
    passphrase: &SnapshotPassphrase,
) -> Result<RecoverableSnapshotVerificationReport, RecoverableSnapshotError> {
    let snapshot_key = unlock_recoverable_snapshot_with_passphrase(snapshot_directory, passphrase)?;
    let report = verify_recoverable_snapshot(snapshot_directory, &snapshot_key)?;
    if report.format_version != RECOVERABLE_SNAPSHOT_WRAPPED_FORMAT_VERSION
        || report.passphrase_protector_count == 0
        || !report.portable_recovery_protector_verified
    {
        return Err(RecoverableSnapshotError::Integrity(
            "passphrase verification did not preserve mandatory 24-word recovery".into(),
        ));
    }
    Ok(report)
}

pub fn recoverable_snapshot_data_root(
    snapshot_directory: &Path,
) -> Result<PathBuf, RecoverableSnapshotError> {
    let snapshot = validate_snapshot_directory(snapshot_directory)?;
    let manifest = load_manifest_from_canonical_snapshot(&snapshot)?;
    validate_manifest_contract(&manifest)?;
    if !manifest.recovery_verified {
        return Err(RecoverableSnapshotError::Integrity(
            "snapshot manifest does not contain a successful recovery verification".into(),
        ));
    }
    let data_root = snapshot.join(DATA_DIRECTORY_NAME);
    validate_private_directory(&data_root, "snapshot data directory")?;
    Ok(data_root)
}

fn inventory_source_databases(root: &Path) -> Result<Vec<PathBuf>, RecoverableSnapshotError> {
    let mut result = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|_| {
            RecoverableSnapshotError::UnsafePath(
                "source database inventory could not be traversed safely".into(),
            )
        })?;
        let path = entry.path();
        if entry.file_type().is_symlink() {
            if path.extension().and_then(|value| value.to_str()) == Some("db") {
                return Err(RecoverableSnapshotError::UnsafePath(
                    "source database inventory contains a symbolic-link database".into(),
                ));
            }
            continue;
        }
        if !entry.file_type().is_file()
            || path.extension().and_then(|value| value.to_str()) != Some("db")
        {
            continue;
        }
        let metadata = fs::metadata(path)?;
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(RecoverableSnapshotError::UnsafePath(
                "source database is not owned by the current user".into(),
            ));
        }
        let relative = path.strip_prefix(root).map_err(|_| {
            RecoverableSnapshotError::UnsafePath("source database escaped the selected root".into())
        })?;
        validate_relative_path(relative)?;
        result.push(relative.to_path_buf());
    }
    result.sort();
    result.dedup();
    Ok(result)
}

fn inventory_snapshot_databases(
    data_root: &Path,
) -> Result<BTreeSet<PathBuf>, RecoverableSnapshotError> {
    let mut result = BTreeSet::new();
    for entry in WalkDir::new(data_root).follow_links(false) {
        let entry = entry.map_err(|_| {
            RecoverableSnapshotError::UnsafePath(
                "snapshot database inventory could not be traversed safely".into(),
            )
        })?;
        if entry.file_type().is_symlink() {
            return Err(RecoverableSnapshotError::UnsafePath(
                "snapshot data tree contains a symbolic link".into(),
            ));
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("db") {
            return Err(RecoverableSnapshotError::Integrity(
                "snapshot data tree contains an undeclared non-database file".into(),
            ));
        }
        let relative = path.strip_prefix(data_root).map_err(|_| {
            RecoverableSnapshotError::UnsafePath("snapshot database escaped data root".into())
        })?;
        validate_relative_path(relative)?;
        validate_private_regular_file(path, "snapshot database")?;
        result.insert(relative.to_path_buf());
    }
    Ok(result)
}

fn copy_encrypted_database_bytes_unchanged(
    source_path: &Path,
    destination_path: &Path,
    expected_byte_count: u64,
    expected_sha256: &str,
) -> Result<(), RecoverableSnapshotError> {
    validate_private_regular_file(source_path, "source snapshot database")?;
    reject_sqlite_sidecars(source_path)?;
    let mut source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(source_path)?;
    let before = source.metadata()?;
    if before.len() != expected_byte_count {
        return Err(RecoverableSnapshotError::Integrity(
            "source database size changed before protector rewrap".into(),
        ));
    }
    let result = (|| {
        let destination = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(destination_path)?;
        let mut writer = BufWriter::new(destination);
        let copied = std::io::copy(&mut source, &mut writer)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        if copied != expected_byte_count {
            return Err(RecoverableSnapshotError::Integrity(
                "encrypted database byte copy was incomplete".into(),
            ));
        }
        fs::set_permissions(destination_path, fs::Permissions::from_mode(0o600))?;
        let after = source.metadata()?;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
        {
            return Err(RecoverableSnapshotError::Integrity(
                "source database changed during protector rewrap".into(),
            ));
        }
        let (actual_byte_count, actual_sha256) = hash_private_file(destination_path)?;
        if actual_byte_count != expected_byte_count || actual_sha256 != expected_sha256 {
            return Err(RecoverableSnapshotError::Integrity(
                "encrypted database bytes changed during protector rewrap".into(),
            ));
        }
        reject_sqlite_sidecars(destination_path)?;
        Ok::<(), RecoverableSnapshotError>(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(destination_path);
        return Err(error);
    }
    Ok(())
}

fn copy_database_logically(
    source: &Connection,
    destination_path: &Path,
    snapshot_key: &SnapshotKey,
    logical_path: &str,
) -> Result<(), RecoverableSnapshotError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(destination_path)
        .map_err(|_| database_failure(logical_path, "destination could not be created"))?;
    let result = (|| {
        let canonical = destination_path.canonicalize()?;
        let mut destination = Connection::open_with_flags(
            canonical,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        configure_snapshot_key(
            &destination,
            snapshot_key.expose_for_snapshot_operation(),
            true,
        )?;
        destination.execute_batch(
            "PRAGMA journal_mode = DELETE;
             PRAGMA synchronous = FULL;
             PRAGMA secure_delete = ON;",
        )?;
        let backup = Backup::new(source, &mut destination)?;
        backup.run_to_completion(256, Duration::from_millis(1), None)?;
        drop(backup);
        destination.execute_batch(
            "PRAGMA wal_checkpoint(TRUNCATE);
             PRAGMA journal_mode = DELETE;
             PRAGMA synchronous = FULL;",
        )?;
        verify_sqlite_integrity(&destination)?;
        drop(destination);
        fs::set_permissions(destination_path, fs::Permissions::from_mode(0o600))?;
        reject_sqlite_sidecars(destination_path)?;
        Ok::<(), RecoverableSnapshotError>(())
    })();
    if let Err(error) = result {
        remove_sqlite_namespace(destination_path);
        return Err(database_failure(
            logical_path,
            &safe_database_reason(&error),
        ));
    }
    Ok(())
}

fn verify_one_database(
    data_root: &Path,
    relative_path: &Path,
    snapshot_key: &SnapshotKey,
) -> Result<RecoverableSnapshotDatabase, RecoverableSnapshotError> {
    validate_relative_path(relative_path)?;
    let path = data_root.join(relative_path);
    validate_private_regular_file(&path, "snapshot database")?;
    reject_sqlite_sidecars(&path)?;
    let logical_path = path_string(relative_path);
    let header = read_prefix(&path, SQLITE_HEADER.len())?;
    if header.as_slice() == SQLITE_HEADER {
        return Err(database_failure(
            &logical_path,
            "database has a plaintext SQLite header",
        ));
    }

    let connection = open_snapshot_database_read_only(&path, snapshot_key)
        .map_err(|error| database_failure(&logical_path, &safe_database_reason(&error)))?;
    verify_sqlite_integrity(&connection)
        .map_err(|error| database_failure(&logical_path, &safe_database_reason(&error)))?;
    let page_count = connection
        .pragma_query_value(None, "page_count", |row| row.get::<_, i64>(0))
        .map_err(|_| {
            database_failure(
                &logical_path,
                "SQLite rejected the recovery page-count check",
            )
        })?;
    if page_count <= 0 {
        return Err(database_failure(
            &logical_path,
            "database contains no logical pages",
        ));
    }
    drop(connection);
    let (byte_count, sha256) = hash_private_file(&path)?;
    Ok(RecoverableSnapshotDatabase {
        relative_path: logical_path,
        byte_count,
        page_count: page_count as u64,
        sha256,
        sqlite_integrity_check: "ok".into(),
        encrypted_at_rest: true,
    })
}

fn configure_snapshot_key(
    connection: &Connection,
    key: &[u8; 32],
    writable: bool,
) -> Result<(), RecoverableSnapshotError> {
    connection.execute_batch(
        "PRAGMA cipher_compatibility = 4;
         PRAGMA cipher_memory_security = ON;",
    )?;
    let mut key_hex = hex::encode(key);
    let key_statement = Zeroizing::new(format!("PRAGMA key = \"x'{key_hex}'\";"));
    key_hex.zeroize();
    connection.execute_batch(&key_statement)?;
    wx_context::register_mm_fts_tokenizer(connection).map_err(|_| {
        RecoverableSnapshotError::Integrity(
            "native WeChat FTS tokenizer registration failed during snapshot access".into(),
        )
    })?;
    connection.execute_batch(
        "PRAGMA foreign_keys = OFF;
         PRAGMA trusted_schema = OFF;
         PRAGMA temp_store = MEMORY;",
    )?;
    if writable {
        connection.execute_batch("PRAGMA secure_delete = ON;")?;
    } else {
        connection.execute_batch("PRAGMA query_only = ON;")?;
    }
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))?;
    Ok(())
}

fn open_snapshot_database_read_only(
    path: &Path,
    snapshot_key: &SnapshotKey,
) -> Result<Connection, RecoverableSnapshotError> {
    validate_private_regular_file(path, "snapshot database")?;
    let connection = Connection::open_with_flags(
        path.canonicalize()?,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    configure_snapshot_key(
        &connection,
        snapshot_key.expose_for_snapshot_operation(),
        false,
    )?;
    Ok(connection)
}

fn verify_sqlite_integrity(connection: &Connection) -> Result<(), RecoverableSnapshotError> {
    let mut statement = connection.prepare("PRAGMA integrity_check")?;
    let results = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if results.as_slice() != ["ok"] {
        return Err(RecoverableSnapshotError::Integrity(
            "SQLite integrity_check did not return ok".into(),
        ));
    }
    Ok(())
}

fn validate_new_output_directory(
    output: &Path,
    canonical_source: &Path,
) -> Result<(PathBuf, PathBuf), RecoverableSnapshotError> {
    if fs::symlink_metadata(output).is_ok() {
        return Err(RecoverableSnapshotError::UnsafePath(
            "snapshot output already exists".into(),
        ));
    }
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let canonical_parent = validate_private_directory(parent, "snapshot output parent")?;
    let name = output.file_name().ok_or_else(|| {
        RecoverableSnapshotError::UnsafePath("snapshot output has no final component".into())
    })?;
    let final_output = canonical_parent.join(name);
    if final_output.starts_with(canonical_source) {
        return Err(RecoverableSnapshotError::UnsafePath(
            "snapshot output must be outside the live database root".into(),
        ));
    }
    Ok((canonical_parent, final_output))
}

fn reject_nested_retention_paths(
    retiring: &Path,
    replacement: &Path,
    quarantine: &Path,
) -> Result<(), RecoverableSnapshotError> {
    for (left, right) in [
        (retiring, replacement),
        (retiring, quarantine),
        (replacement, quarantine),
    ] {
        if left == right || left.starts_with(right) || right.starts_with(left) {
            return Err(RecoverableSnapshotError::UnsafePath(
                "retiring, replacement, and quarantine directories must not contain one another"
                    .into(),
            ));
        }
    }
    Ok(())
}

fn require_same_filesystem(
    source: &Path,
    destination_parent: &Path,
) -> Result<(), RecoverableSnapshotError> {
    if fs::metadata(source)?.dev() != fs::metadata(destination_parent)?.dev() {
        return Err(RecoverableSnapshotError::UnsafePath(
            "recoverable snapshot retention requires one filesystem".into(),
        ));
    }
    Ok(())
}

fn sync_retention_move_parents(
    source_parent: &Path,
    destination_parent: &Path,
) -> Result<(), RecoverableSnapshotError> {
    File::open(source_parent)?.sync_all()?;
    if source_parent != destination_parent {
        File::open(destination_parent)?.sync_all()?;
    }
    Ok(())
}

fn validate_snapshot_directory(path: &Path) -> Result<PathBuf, RecoverableSnapshotError> {
    validate_private_directory(path, "recoverable snapshot directory")
}

fn validate_private_directory(
    path: &Path,
    description: &str,
) -> Result<PathBuf, RecoverableSnapshotError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        RecoverableSnapshotError::UnsafePath(format!("{description} is unavailable"))
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(RecoverableSnapshotError::UnsafePath(format!(
            "{description} must be a current-user-owned owner-only real directory"
        )));
    }
    Ok(path.canonicalize()?)
}

fn validate_private_regular_file(
    path: &Path,
    description: &str,
) -> Result<(), RecoverableSnapshotError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        RecoverableSnapshotError::UnsafePath(format!("{description} is unavailable"))
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
    {
        return Err(RecoverableSnapshotError::UnsafePath(format!(
            "{description} must be a current-user-owned owner-only single-link regular file"
        )));
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), RecoverableSnapshotError> {
    fs::create_dir(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn create_private_directories_below(
    root: &Path,
    target: &Path,
) -> Result<(), RecoverableSnapshotError> {
    let relative = target.strip_prefix(root).map_err(|_| {
        RecoverableSnapshotError::UnsafePath(
            "snapshot destination directory escaped data root".into(),
        )
    })?;
    validate_relative_path_allow_empty(relative)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(RecoverableSnapshotError::UnsafePath(
                "snapshot destination directory is invalid".into(),
            ));
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(_) => {
                validate_private_directory(&current, "snapshot data subdirectory")?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_private_directory(&current)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> Result<(), RecoverableSnapshotError> {
    if path.as_os_str().is_empty() {
        return Err(RecoverableSnapshotError::UnsafePath(
            "database relative path is empty".into(),
        ));
    }
    validate_relative_path_allow_empty(path)
}

fn validate_relative_path_allow_empty(path: &Path) -> Result<(), RecoverableSnapshotError> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(RecoverableSnapshotError::UnsafePath(
            "database relative path is not confined to the snapshot".into(),
        ));
    }
    Ok(())
}

fn validated_manifest_relative_path(path: &str) -> Result<PathBuf, RecoverableSnapshotError> {
    if path.contains('\\') || path.contains('\0') {
        return Err(RecoverableSnapshotError::Integrity(
            "manifest database path is malformed".into(),
        ));
    }
    let path = PathBuf::from(path);
    validate_relative_path(&path)?;
    if path.extension().and_then(|value| value.to_str()) != Some("db") {
        return Err(RecoverableSnapshotError::Integrity(
            "manifest contains a non-database data entry".into(),
        ));
    }
    Ok(path)
}

fn require_core_database(
    databases: &[PathBuf],
    expected: &str,
) -> Result<(), RecoverableSnapshotError> {
    if !databases.iter().any(|path| path == Path::new(expected)) {
        return Err(RecoverableSnapshotError::InvalidArgument(format!(
            "source is missing required core database {expected}"
        )));
    }
    Ok(())
}

fn validate_manifest_contract(
    manifest: &RecoverableSnapshotManifest,
) -> Result<(), RecoverableSnapshotError> {
    let legacy_protection = manifest.schema == RECOVERABLE_SNAPSHOT_SCHEMA
        && manifest.format_version == RECOVERABLE_SNAPSHOT_FORMAT_VERSION
        && manifest.protection.recovery_protector == "portable256BitRecoveryKey"
        && manifest.protection.protectors.is_empty();
    let wrapped_protection = manifest.schema == RECOVERABLE_SNAPSHOT_WRAPPED_SCHEMA
        && manifest.format_version == RECOVERABLE_SNAPSHOT_WRAPPED_FORMAT_VERSION
        && manifest.protection.recovery_protector == "multiProtectorEnvelopeV1"
        && !manifest.protection.protectors.is_empty();
    if (!legacy_protection && !wrapped_protection)
        || manifest.snapshot_id.len() != 64
        || !manifest
            .snapshot_id
            .bytes()
            .all(|value| value.is_ascii_hexdigit())
        || manifest.parent_snapshot_id.as_ref().is_some_and(|parent| {
            parent.len() != 64
                || !parent.bytes().all(|value| value.is_ascii_hexdigit())
                || parent == &manifest.snapshot_id
        })
        || !manifest.recovery_verified
        || !manifest.protection.independent_of_wechat_key
        || manifest.protection.plaintext_database_files
        || manifest.protection.database_encryption != "sqlcipher4RawKey"
        || !matches!(
            manifest.consistency.guarantee.as_str(),
            "perDatabaseOnlineBackup"
                | "stableAcquisitionSnapshotConversion"
                | "encryptedDatabaseByteCopyRewrap"
        )
        || manifest.consistency.database_count != manifest.databases.len()
        || manifest.consistency.cross_database_atomic != (manifest.databases.len() <= 1)
        || manifest.databases.is_empty()
    {
        return Err(RecoverableSnapshotError::Integrity(
            "recoverable snapshot manifest contract is invalid".into(),
        ));
    }
    if wrapped_protection {
        let mut protector_ids = BTreeSet::new();
        let mut recovery_word_count = 0usize;
        for protector in &manifest.protection.protectors {
            validate_wrapped_snapshot_key(protector).map_err(|_| {
                RecoverableSnapshotError::Integrity(
                    "recoverable snapshot contains an invalid key protector".into(),
                )
            })?;
            if !protector_ids.insert(&protector.protector_id) {
                return Err(RecoverableSnapshotError::Integrity(
                    "recoverable snapshot contains duplicate key protectors".into(),
                ));
            }
            recovery_word_count +=
                usize::from(protector.kind == "bip39English24" && protector.portable);
        }
        if recovery_word_count == 0 {
            return Err(RecoverableSnapshotError::Integrity(
                "recoverable snapshot has no mandatory 24-word recovery protector".into(),
            ));
        }
    }
    let mut paths = BTreeSet::new();
    for database in &manifest.databases {
        let path = validated_manifest_relative_path(&database.relative_path)?;
        if !paths.insert(path)
            || database.byte_count == 0
            || database.page_count == 0
            || database.sha256.len() != 64
            || !database
                .sha256
                .bytes()
                .all(|value| value.is_ascii_hexdigit())
            || database.sqlite_integrity_check != "ok"
            || !database.encrypted_at_rest
        {
            return Err(RecoverableSnapshotError::Integrity(
                "recoverable snapshot database manifest entry is invalid".into(),
            ));
        }
    }
    Ok(())
}

fn write_manifest_create_new(
    snapshot_directory: &Path,
    manifest: &RecoverableSnapshotManifest,
) -> Result<(), RecoverableSnapshotError> {
    let path = snapshot_directory.join(MANIFEST_FILE_NAME);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, manifest)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}

fn load_manifest_from_canonical_snapshot(
    snapshot: &Path,
) -> Result<RecoverableSnapshotManifest, RecoverableSnapshotError> {
    let path = snapshot.join(MANIFEST_FILE_NAME);
    validate_private_regular_file(&path, "recoverable snapshot manifest")?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() == 0 || metadata.len() > MAXIMUM_MANIFEST_BYTES {
        return Err(RecoverableSnapshotError::Integrity(
            "recoverable snapshot manifest size is outside safe limits".into(),
        ));
    }
    let reader = BufReader::new(file.take(MAXIMUM_MANIFEST_BYTES + 1));
    Ok(serde_json::from_reader(reader)?)
}

fn hash_private_file(path: &Path) -> Result<(u64, String), RecoverableSnapshotError> {
    validate_private_regular_file(path, "snapshot database")?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let before = file.metadata()?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
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
        return Err(RecoverableSnapshotError::Integrity(
            "snapshot database changed while it was hashed".into(),
        ));
    }
    Ok((before.len(), hex::encode(digest.finalize())))
}

fn read_prefix(path: &Path, byte_count: usize) -> Result<Vec<u8>, RecoverableSnapshotError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let mut bytes = vec![0u8; byte_count];
    let mut offset = 0usize;
    while offset < bytes.len() {
        let count = file.read(&mut bytes[offset..])?;
        if count == 0 {
            bytes.truncate(offset);
            break;
        }
        offset += count;
    }
    Ok(bytes)
}

fn reject_sqlite_sidecars(path: &Path) -> Result<(), RecoverableSnapshotError> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            RecoverableSnapshotError::UnsafePath("database filename is invalid".into())
        })?;
    let parent = path.parent().ok_or_else(|| {
        RecoverableSnapshotError::UnsafePath("database path has no parent".into())
    })?;
    for suffix in ["-wal", "-shm", "-journal"] {
        if fs::symlink_metadata(parent.join(format!("{file_name}{suffix}"))).is_ok() {
            return Err(RecoverableSnapshotError::Integrity(
                "published snapshot database retains transient SQLite sidecars".into(),
            ));
        }
    }
    Ok(())
}

fn remove_sqlite_namespace(path: &Path) {
    let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    for candidate in [
        path.to_path_buf(),
        parent.join(format!("{file_name}-wal")),
        parent.join(format!("{file_name}-shm")),
        parent.join(format!("{file_name}-journal")),
    ] {
        let _ = fs::remove_file(candidate);
    }
}

fn sync_directory_tree(root: &Path) -> Result<(), RecoverableSnapshotError> {
    let mut directories = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|_| {
            RecoverableSnapshotError::UnsafePath(
                "snapshot directory tree could not be traversed for durable publication".into(),
            )
        })?;
        if entry.file_type().is_dir() {
            directories.push(entry.into_path());
        }
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        File::open(directory)?.sync_all()?;
    }
    Ok(())
}

fn generate_snapshot_id(source_identity: &str, database_count: usize) -> String {
    let mut digest = Sha256::new();
    digest.update(b"greenbubbles-recoverable-snapshot-id-v1\0");
    digest.update(source_identity.as_bytes());
    digest.update(now_unix_milliseconds().to_le_bytes());
    digest.update(std::process::id().to_le_bytes());
    digest.update(database_count.to_le_bytes());
    hex::encode(digest.finalize())
}

fn now_unix_milliseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn path_string(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn database_failure(logical_path: &str, reason: &str) -> RecoverableSnapshotError {
    RecoverableSnapshotError::Database {
        logical_path: logical_path.into(),
        reason: reason.into(),
    }
}

fn safe_database_reason(error: &RecoverableSnapshotError) -> String {
    match error {
        RecoverableSnapshotError::Integrity(reason) => reason.clone(),
        RecoverableSnapshotError::Sqlite(_) => {
            "SQLite rejected the logical backup or recovery check".into()
        }
        RecoverableSnapshotError::Io(_) => "database storage operation failed".into(),
        _ => "database recovery operation failed".into(),
    }
}
