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
