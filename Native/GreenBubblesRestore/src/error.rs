use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    #[error("snapshot manifest is missing or invalid: {0}")]
    Manifest(String),
    #[error("unsafe path in snapshot manifest: {0}")]
    UnsafePath(String),
    #[error("database passphrase is required for encrypted database set {0}")]
    PassphraseRequired(String),
    #[error("database passphrase must be exactly 32 bytes or 64 hexadecimal characters")]
    InvalidPassphrase,
    #[error("exported database key file is invalid: {0}")]
    InvalidDatabaseKeyExport(String),
    #[error("no exported database key matches encrypted database set {0}")]
    DatabaseKeyRequired(String),
    #[error(
        "no exported database key authenticated encrypted database set {set_id} \
         (exported entries: {exported_key_count}, exact path entry: {exact_path_entry}, \
         matching salt entries: {matching_salt_entry_count}, authenticated entries: \
         {authenticated_entry_count})"
    )]
    DatabaseKeyAssociation {
        set_id: String,
        exported_key_count: usize,
        exact_path_entry: bool,
        matching_salt_entry_count: usize,
        authenticated_entry_count: usize,
    },
    #[error("replica key must be exactly 32 bytes or 64 hexadecimal characters")]
    InvalidReplicaKey,
    #[error("database decryption failed for set {set_id}: {reason}")]
    Decryption { set_id: String, reason: String },
    #[error("snapshot entry is missing: {0}")]
    MissingEntry(PathBuf),
    #[error("snapshot entry failed integrity verification: {0}")]
    Integrity(String),
    #[error("unsupported or incomplete message table {table}: {reason}")]
    UnsupportedTable { table: String, reason: String },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}
