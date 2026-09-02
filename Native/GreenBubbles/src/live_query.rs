use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Cursor, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rusqlite::types::ValueRef;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

use crate::tools::summarize_decoded_payload;
use crate::ConversationKind;

pub const QUERY_SCHEMA: &str = "greenbubbles.query.v1";
pub const QUERY_FORMAT_VERSION: u32 = 1;
pub const DEFAULT_PAGE_LIMIT: usize = 100;
pub const MAX_PAGE_LIMIT: usize = 500;
pub const DEFAULT_SEARCH_LIMIT: usize = 50;
pub const MAX_SEARCH_LIMIT: usize = 200;
pub const MAX_SEARCH_QUERY_BYTES: usize = 16 * 1024;
pub const MAX_PROJECTED_TEXT_BYTES: usize = 16 * 1024;
pub const MAX_SERIALIZED_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

const CURSOR_FORMAT_VERSION: u32 = 1;
const MAXIMUM_SOURCE_DATABASE_FILES: usize = 4_096;
const MAXIMUM_SOURCE_INVENTORY_ENTRIES: usize = 100_000;
const MAXIMUM_CONTACT_INVENTORY_ENTRIES: usize = 250_000;
const CORPUS_METADATA_PAGE_ROWS: usize = 10_000;
const CORPUS_HYDRATION_ROWS: usize = 400;
const MAX_CURSOR_BYTES: usize = 4096;
const MAX_CONVERSATION_ID_BYTES: usize = 4096;
const MAX_SOURCE_IDENTIFIER_BYTES: usize = 512;
const MAX_FALLBACK_SEARCH_MESSAGES_PER_PAGE: usize = 500;
const MAX_FALLBACK_SEARCH_CONVERSATIONS_PER_PAGE: usize = 16;
const FALLBACK_SEARCH_CURSOR_KIND: &str = "messages.search.fallback";
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_millis(2_000);
const MAXIMUM_SQL_STATEMENT_DURATION: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub enum LiveQueryError {
    #[error("invalid query: {0}")]
    InvalidArgument(String),
    #[error("unsafe database source: {0}")]
    UnsafeSource(String),
    #[error("database query failed: {0}")]
    Database(String),
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),
    #[error("query resource was not found: {0}")]
    NotFound(String),
    #[error("bounded search is unavailable: {0}")]
    SearchUnavailable(String),
    #[error("serialized response exceeds the {maximum_bytes}-byte safety limit")]
    ResponseTooLarge { maximum_bytes: usize },
}

#[derive(Debug, Clone, Copy)]
pub enum QueryDatabaseAccess<'a> {
    LiveEncrypted(&'a [u8; 32]),
    SnapshotEncrypted(&'a [u8; 32]),
    Decrypted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QuerySourceMode {
    LiveEncrypted,
    SnapshotEncrypted,
    Decrypted,
}

#[derive(Debug)]
pub struct LiveQuerySource<'a> {
    root: PathBuf,
    identity: String,
    access: QueryDatabaseAccess<'a>,
    account_holder_source_id: Option<String>,
}

impl<'a> LiveQuerySource<'a> {
    pub fn open(root: &Path, access: QueryDatabaseAccess<'a>) -> Result<Self, LiveQueryError> {
        let input_metadata = fs::symlink_metadata(root)
            .map_err(|_| LiveQueryError::UnsafeSource("database root is unavailable".into()))?;
        if input_metadata.file_type().is_symlink() || !input_metadata.is_dir() {
            return Err(LiveQueryError::UnsafeSource(
                "database root must be a real directory, not a symbolic link".into(),
            ));
        }
        if input_metadata.uid() != unsafe { libc::geteuid() } {
            return Err(LiveQueryError::UnsafeSource(
                "database root must be owned by the current user".into(),
            ));
        }

        let canonical = root.canonicalize().map_err(|_| {
            LiveQueryError::UnsafeSource("database root could not be canonicalized".into())
        })?;
        let metadata = fs::metadata(&canonical).map_err(|_| {
            LiveQueryError::UnsafeSource("database root could not be inspected".into())
        })?;
        let mut hasher = Sha256::new();
        hasher.update(b"greenbubbles-live-source-v1\0");
        hasher.update(match access {
            QueryDatabaseAccess::LiveEncrypted(_) => b"liveEncrypted".as_slice(),
            QueryDatabaseAccess::SnapshotEncrypted(_) => b"snapshotEncrypted".as_slice(),
            QueryDatabaseAccess::Decrypted => b"decrypted".as_slice(),
        });
        hasher.update([0]);
        hasher.update(canonical.as_os_str().as_encoded_bytes());
        hasher.update(metadata.dev().to_le_bytes());
        hasher.update(metadata.ino().to_le_bytes());
        let digest = hasher.finalize();
        let identity = format!("sha256:{}", hex::encode(&digest[..16]));

        let account_holder_source_id = live_account_holder_source_id(&canonical)?;

        Ok(Self {
            root: canonical,
            identity,
            access,
            account_holder_source_id,
        })
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub fn mode(&self) -> QuerySourceMode {
        match self.access {
            QueryDatabaseAccess::LiveEncrypted(_) => QuerySourceMode::LiveEncrypted,
            QueryDatabaseAccess::SnapshotEncrypted(_) => QuerySourceMode::SnapshotEncrypted,
            QueryDatabaseAccess::Decrypted => QuerySourceMode::Decrypted,
        }
    }

    /// The source-level account identifier bound by the selected live account
    /// directory. This value stays inside the direct-query boundary; callers
    /// should release only derived attribution when policy permits sender data.
    pub(crate) fn account_holder_source_id(&self) -> Option<&str> {
        self.account_holder_source_id.as_deref()
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn open_optional_database(
        &self,
        relative_path: &Path,
    ) -> Result<Option<Connection>, LiveQueryError> {
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(LiveQueryError::UnsafeSource(
                "database path is outside the selected root".into(),
            ));
        }
        match fs::symlink_metadata(self.root.join(relative_path)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(LiveQueryError::UnsafeSource(
                "optional database could not be inspected".into(),
            )),
            Ok(_) => self.open_database(relative_path).map(Some),
        }
    }

    pub(crate) fn open_database(&self, relative_path: &Path) -> Result<Connection, LiveQueryError> {
        let path = self.safe_database_path(relative_path)?;
        let connection = match self.access {
            QueryDatabaseAccess::LiveEncrypted(key) => {
                wx_db::open_readonly_connection(&path, Some(key))
                    .map_err(|error| database_error(&error.to_string()))?
            }
            QueryDatabaseAccess::SnapshotEncrypted(key) => {
                open_snapshot_readonly_connection(&path, key)?
            }
            QueryDatabaseAccess::Decrypted => wx_db::open_readonly_connection(&path, None)
                .map_err(|error| database_error(&error.to_string()))?,
        };
        connection
            .busy_timeout(SQLITE_BUSY_TIMEOUT)
            .map_err(|error| database_error(&error.to_string()))?;
        connection
            .execute_batch("PRAGMA query_only = ON")
            .map_err(|error| database_error(&error.to_string()))?;
        let query_only = connection
            .query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))
            .map_err(|error| database_error(&error.to_string()))?;
        if query_only != 1 {
            return Err(LiveQueryError::Database(
                "read-only enforcement could not be verified".into(),
            ));
        }
        let deadline = Instant::now() + MAXIMUM_SQL_STATEMENT_DURATION;
        connection
            .progress_handler(10_000, Some(move || Instant::now() >= deadline))
            .map_err(|error| database_error(&error.to_string()))?;
        Ok(connection)
    }

    fn safe_database_path(&self, relative_path: &Path) -> Result<PathBuf, LiveQueryError> {
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(LiveQueryError::UnsafeSource(
                "database path is outside the selected root".into(),
            ));
        }
        let path = self.root.join(relative_path);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| LiveQueryError::UnsafeSource("required database is unavailable".into()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(LiveQueryError::UnsafeSource(
                "database must be a real regular file".into(),
            ));
        }
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(LiveQueryError::UnsafeSource(
                "database must be owned by the current user".into(),
            ));
        }
        let canonical = path.canonicalize().map_err(|_| {
            LiveQueryError::UnsafeSource("database path could not be canonicalized".into())
        })?;
        if canonical != path || !canonical.starts_with(&self.root) {
            return Err(LiveQueryError::UnsafeSource(
                "database path contains a symbolic link or escapes the selected root".into(),
            ));
        }
        Ok(canonical)
    }

    fn safe_database_directory(
        &self,
        relative_path: &Path,
        description: &str,
    ) -> Result<PathBuf, LiveQueryError> {
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(LiveQueryError::UnsafeSource(format!(
                "{description} is outside the selected root"
            )));
        }
        let path = self.root.join(relative_path);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| LiveQueryError::UnsafeSource(format!("{description} is unavailable")))?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(LiveQueryError::UnsafeSource(format!(
                "{description} is not a current-user-owned real directory"
            )));
        }
        let canonical = path.canonicalize().map_err(|_| {
            LiveQueryError::UnsafeSource(format!("{description} could not be canonicalized"))
        })?;
        if canonical != path || !canonical.starts_with(&self.root) {
            return Err(LiveQueryError::UnsafeSource(format!(
                "{description} contains a symbolic link or escapes the selected root"
            )));
        }
        Ok(canonical)
    }

    fn message_shards(&self) -> Result<Vec<MessageShard>, LiveQueryError> {
        let relative_directory = Path::new("message");
        let directory =
            self.safe_database_directory(relative_directory, "message database directory")?;

        let mut shards = Vec::new();
        for entry in fs::read_dir(&directory).map_err(|_| {
            LiveQueryError::UnsafeSource("message database directory cannot be read".into())
        })? {
            let entry = entry.map_err(|_| {
                LiveQueryError::UnsafeSource("message database inventory changed while read".into())
            })?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(number) = name
                .strip_prefix("message_")
                .and_then(|value| value.strip_suffix(".db"))
            else {
                continue;
            };
            if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            let shard_id = number.parse::<u32>().map_err(|_| {
                LiveQueryError::UnsafeSource(
                    "message shard identifier is outside safe limits".into(),
                )
            })?;
            let relative_path = relative_directory.join(&name);
            self.safe_database_path(&relative_path)?;
            shards.push(MessageShard {
                shard_id,
                relative_path,
            });
            if shards.len() > MAXIMUM_SOURCE_DATABASE_FILES {
                return Err(LiveQueryError::UnsafeSource(
                    "message shard count exceeds the fixed safety limit".into(),
                ));
            }
        }
        shards.sort_by_key(|shard| shard.shard_id);
        if shards
            .windows(2)
            .any(|pair| pair[0].shard_id == pair[1].shard_id)
        {
            return Err(LiveQueryError::UnsafeSource(
                "message shard inventory contains duplicate identifiers".into(),
            ));
        }
        if shards.is_empty() {
            return Err(LiveQueryError::Database(
                "no numbered message shard databases were found".into(),
            ));
        }
        Ok(shards)
    }

    pub(crate) fn media_databases(&self) -> Result<Vec<PathBuf>, LiveQueryError> {
        let relative_directory = Path::new("media");
        let candidate = self.root.join(relative_directory);
        let metadata = match fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => {
                return Err(LiveQueryError::UnsafeSource(
                    "media database directory is unavailable".into(),
                ))
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(LiveQueryError::UnsafeSource(
                "media database directory is not a real directory".into(),
            ));
        }
        let directory =
            self.safe_database_directory(relative_directory, "media database directory")?;

        let mut databases = Vec::new();
        for entry in fs::read_dir(&directory).map_err(|_| {
            LiveQueryError::UnsafeSource("media database directory cannot be read".into())
        })? {
            let entry = entry.map_err(|_| {
                LiveQueryError::UnsafeSource(
                    "media database inventory changed while it was read".into(),
                )
            })?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let numbered = name
                .strip_prefix("media_")
                .and_then(|value| value.strip_suffix(".db"))
                .is_some_and(|value| {
                    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
                });
            if name != "media.db" && !numbered {
                continue;
            }
            if !entry
                .file_type()
                .map_err(|_| {
                    LiveQueryError::UnsafeSource(
                        "media database file type could not be inspected".into(),
                    )
                })?
                .is_file()
            {
                return Err(LiveQueryError::UnsafeSource(
                    "media database inventory contains a non-regular candidate".into(),
                ));
            }
            let relative_path = relative_directory.join(name);
            self.safe_database_path(&relative_path)?;
            databases.push(relative_path);
            if databases.len() > MAXIMUM_SOURCE_DATABASE_FILES {
                return Err(LiveQueryError::UnsafeSource(
                    "media database count exceeds the fixed safety limit".into(),
                ));
            }
        }
        databases.sort();
        Ok(databases)
    }
}

fn live_account_holder_source_id(root: &Path) -> Result<Option<String>, LiveQueryError> {
    if root.file_name().and_then(|value| value.to_str()) != Some("db_storage") {
        return Ok(None);
    }
    let account_root = root.parent().ok_or_else(|| {
        LiveQueryError::UnsafeSource("database root has no account directory".into())
    })?;
    let metadata = fs::symlink_metadata(account_root).map_err(|_| {
        LiveQueryError::UnsafeSource("account directory could not be inspected".into())
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(LiveQueryError::UnsafeSource(
            "account directory must be a current-user-owned real directory".into(),
        ));
    }
    let directory_name = account_root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            LiveQueryError::UnsafeSource("account directory name is not valid UTF-8".into())
        })?;
    if directory_name.is_empty()
        || directory_name == "."
        || directory_name == ".."
        || directory_name.chars().any(char::is_control)
    {
        return Err(LiveQueryError::UnsafeSource(
            "account directory has no usable account identifier".into(),
        ));
    }

    let Some((candidate, suffix)) = directory_name.rsplit_once('_') else {
        return Ok(Some(directory_name.to_string()));
    };
    let removable_suffix = suffix.len() == 4
        && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && !candidate.is_empty();
    if !removable_suffix {
        return Ok(Some(directory_name.to_string()));
    }
    if directory_name.starts_with("wxid_") {
        return Ok((candidate.starts_with("wxid_") && candidate.len() > 5)
            .then(|| candidate.to_string())
            .or_else(|| Some(directory_name.to_string())));
    }

    let Some(xwechat_root) = account_root.parent() else {
        return Ok(Some(directory_name.to_string()));
    };
    let login_candidate = xwechat_root.join("all_users").join("login").join(candidate);
    let independently_confirmed =
        fs::symlink_metadata(&login_candidate)
            .ok()
            .is_some_and(|value| {
                value.is_dir()
                    && !value.file_type().is_symlink()
                    && value.uid() == unsafe { libc::geteuid() }
            });
    Ok(Some(if independently_confirmed {
        candidate.to_string()
    } else {
        directory_name.to_string()
    }))
}

fn open_snapshot_readonly_connection(
    path: &Path,
    key: &[u8; 32],
) -> Result<Connection, LiveQueryError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|error| database_error(&error.to_string()))?;
    connection
        .execute_batch(
            "PRAGMA cipher_compatibility = 4;
             PRAGMA cipher_memory_security = ON;",
        )
        .map_err(|error| database_error(&error.to_string()))?;
    let mut key_hex = hex::encode(key);
    let key_statement = Zeroizing::new(format!("PRAGMA key = \"x'{key_hex}'\";"));
    key_hex.zeroize();
    connection
        .execute_batch(&key_statement)
        .map_err(|error| database_error(&error.to_string()))?;
    connection
        .query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))
        .map_err(|_| {
            LiveQueryError::Database(
                "snapshot could not be opened with the supplied recovery key".into(),
            )
        })?;
    Ok(connection)
}

#[derive(Debug)]
struct MessageShard {
    shard_id: u32,
    relative_path: PathBuf,
}

struct OpenMessageShard {
    shard_id: u32,
    connection: Connection,
}

struct OpenMessageShards {
    shards: Vec<OpenMessageShard>,
    warnings: Vec<QueryWarning>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuerySourceDescription {
    pub mode: QuerySourceMode,
    pub identity: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryConsistency {
    pub guarantee: &'static str,
    pub database_count: usize,
    pub cross_database_atomic: bool,
    pub coverage_complete: bool,
    pub observed_at_unix_milliseconds: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryPage {
    pub limit: usize,
    pub returned: usize,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryWarning {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shard_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryErrorBody {
    pub code: &'static str,
    pub message: &'static str,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryErrorEnvelope {
    pub schema: &'static str,
    pub format_version: u32,
    pub operation: &'static str,
    pub ok: bool,
    pub error: QueryErrorBody,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryEnvelope<T> {
    pub schema: &'static str,
    pub format_version: u32,
    pub operation: &'static str,
    pub ok: bool,
    pub source: QuerySourceDescription,
    pub consistency: QueryConsistency,
    pub page: QueryPage,
    pub warnings: Vec<QueryWarning>,
    pub items: Vec<T>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResourceEnvelope<T> {
    pub schema: &'static str,
    pub format_version: u32,
    pub operation: &'static str,
    pub ok: bool,
    pub source: QuerySourceDescription,
    pub consistency: QueryConsistency,
    pub warnings: Vec<QueryWarning>,
    pub item: T,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationItem {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub summary: Option<String>,
    pub summary_decode_state: &'static str,
    pub summary_truncated: bool,
    pub sort_timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message_type: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message_sender: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sender_display_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContactKind {
    AccountHolder,
    Person,
    Group,
    Official,
    Service,
    Unknown,
}

impl ContactKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::AccountHolder => "accountHolder",
            Self::Person => "person",
            Self::Group => "group",
            Self::Official => "official",
            Self::Service => "service",
            Self::Unknown => "unknown",
        }
    }
}

impl FromStr for ContactKind {
    type Err = LiveQueryError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "account-holder" | "accountHolder" => Ok(Self::AccountHolder),
            "person" => Ok(Self::Person),
            "group" => Ok(Self::Group),
            "official" => Ok(Self::Official),
            "service" => Ok(Self::Service),
            "unknown" => Ok(Self::Unknown),
            _ => Err(LiveQueryError::InvalidArgument(
                "contact kind must be account-holder, person, group, official, service, or unknown"
                    .into(),
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactItem {
    pub id: String,
    pub display_name: String,
    pub kind: ContactKind,
    pub is_account_holder: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remark: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageItem {
    pub id: String,
    pub conversation_id: String,
    pub sort_sequence: i64,
    pub server_id: i64,
    pub message_type: u32,
    pub message_type_label: &'static str,
    pub message_subtype: u32,
    pub message_subtype_label: &'static str,
    pub sender: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_display_name: Option<String>,
    pub created_at_unix: i64,
    pub status: i32,
    pub content: Value,
    pub content_decode_state: &'static str,
    pub content_truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchItem {
    pub id: String,
    pub conversation_id: String,
    pub sender: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_display_name: Option<String>,
    pub created_at_unix: i64,
    pub sort_sequence: i64,
    pub message_local_id: i64,
    pub message_type: u32,
    pub message_type_label: &'static str,
    pub message_subtype: u32,
    pub message_subtype_label: &'static str,
    pub snippet: String,
    pub snippet_truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDatabaseSize {
    pub relative_path: String,
    pub database_bytes: u64,
    pub write_ahead_log_bytes: u64,
    pub shared_memory_bytes: u64,
    pub rollback_journal_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    pub schema: &'static str,
    pub format_version: u32,
    pub operation: &'static str,
    pub ok: bool,
    pub source: QuerySourceDescription,
    pub observed_at_unix_milliseconds: u64,
    pub database_count: usize,
    pub database_bytes: u64,
    pub write_ahead_log_count: usize,
    pub write_ahead_log_bytes: u64,
    pub shared_memory_count: usize,
    pub shared_memory_bytes: u64,
    pub rollback_journal_count: usize,
    pub rollback_journal_bytes: u64,
    pub total_sqlite_storage_bytes: u64,
    pub entries: Vec<SourceDatabaseSize>,
}

#[derive(Debug, Clone)]
struct ConversationRow {
    username: String,
    summary: Option<String>,
    summary_decode_state: &'static str,
    summary_truncated: bool,
    sort_timestamp: i64,
    last_message_type: Option<u32>,
    last_message_sender: Option<String>,
    last_sender_display_name: Option<String>,
}

#[derive(Debug)]
struct ContactDisplayNameEnrichment {
    display_names: BTreeMap<String, String>,
    database_read: bool,
    coverage_complete: bool,
    warnings: Vec<QueryWarning>,
}

#[derive(Debug, Clone)]
struct ContactRecord {
    id: String,
    remark: Option<String>,
    nickname: Option<String>,
    alias: Option<String>,
    in_contact_table: bool,
    in_chat_room_table: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ConversationCursor {
    version: u32,
    kind: String,
    source_identity: String,
    sort_timestamp: i64,
    username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ContactCursor {
    version: u32,
    kind: String,
    source_identity: String,
    contact_kind: Option<String>,
    include_details: bool,
    identifier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MessageCursor {
    version: u32,
    kind: String,
    source_identity: String,
    conversation_digest: String,
    sort_sequence: i64,
    create_time: i64,
    server_id: i64,
    shard_id: u32,
    row_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MessageKey {
    sort_sequence: i64,
    create_time: i64,
    server_id: i64,
    shard_id: u32,
    row_id: i64,
}

#[derive(Debug)]
struct RawMessage {
    key: MessageKey,
    local_type: i64,
    sender: String,
    raw_content: Vec<u8>,
    packed_info: Option<Vec<u8>>,
    status: i32,
    compression_type: Option<i32>,
    compressed_content: Option<Vec<u8>>,
}

fn raw_message_from_row(
    row: &rusqlite::Row<'_>,
    shard_id: u32,
) -> Result<RawMessage, rusqlite::Error> {
    let row_id = row.get::<_, i64>(0)?;
    let sort_sequence = row.get::<_, i64>(1)?;
    let server_id = row.get::<_, i64>(2)?;
    let local_type = row.get::<_, i64>(3)?;
    let sender = row.get::<_, String>(4)?;
    let create_time = row.get::<_, i64>(5)?;
    let raw_content = sqlite_value_bytes(row.get_ref(6)?);
    let packed_info = sqlite_optional_bytes(row.get_ref(7)?);
    let status = row.get::<_, Option<i64>>(8)?.unwrap_or_default() as i32;
    let compression_type = row.get::<_, Option<i64>>(9)?.map(|value| value as i32);
    let compressed_content = sqlite_optional_bytes(row.get_ref(10)?);
    Ok(RawMessage {
        key: MessageKey {
            sort_sequence,
            create_time,
            server_id,
            shard_id,
            row_id,
        },
        local_type,
        sender,
        raw_content,
        packed_info,
        status,
        compression_type,
        compressed_content,
    })
}

fn normalized_source_identifier(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= MAX_SOURCE_IDENTIFIER_BYTES
        && value.trim() == value
        && !value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        && !value.contains('<')
        && !value.contains('>'))
    .then(|| value.to_string())
}

/// `Name2Id` is the source's explicit sender relation and therefore wins over
/// the content-prefix fallback. The upstream decoder also recognizes the
/// historical `sender:\ncontent` group format, but an XML payload can contain
/// the same delimiter and must never become a sender identifier.
fn resolved_message_sender(source_sender: &str, decoded_sender: &str) -> String {
    normalized_source_identifier(source_sender)
        .or_else(|| normalized_source_identifier(decoded_sender))
        .unwrap_or_default()
}

fn project_message(
    source: &LiveQuerySource<'_>,
    conversation: &str,
    conversation_digest: &str,
    raw: RawMessage,
) -> Result<(MessageItem, bool), LiveQueryError> {
    let (message_type, message_subtype) = wx_db::split_local_type(raw.local_type);
    let decoded = wx_db::decode_message_for_test(
        raw.key.sort_sequence,
        raw.key.server_id,
        raw.local_type,
        &raw.sender,
        conversation,
        raw.key.create_time,
        &raw.raw_content,
        raw.packed_info.as_deref(),
        raw.status,
        raw.compression_type,
        raw.compressed_content.as_deref(),
        wx_db::is_group_chat(conversation),
    );
    let (decoded_sender, mut content, decode_state, content_decode_failed) = match decoded {
        Ok(message) => (
            message.sender,
            serde_json::to_value(message.content)
                .unwrap_or_else(|_| json!({"unavailable": "projectionFailed"})),
            "complete",
            false,
        ),
        Err(_) => (
            raw.sender.clone(),
            json!({"unavailable": "decodeFailed"}),
            "failed",
            true,
        ),
    };
    let mut truncated_field_count = 0usize;
    truncate_json_strings(
        &mut content,
        MAX_PROJECTED_TEXT_BYTES,
        &mut truncated_field_count,
    );
    let sender = resolved_message_sender(&raw.sender, &decoded_sender);
    let (sender, sender_truncated) = truncate_utf8(sender, MAX_PROJECTED_TEXT_BYTES);
    let id = encode_message_cursor(source, conversation_digest, "message.identity", &raw.key)?;
    Ok((
        MessageItem {
            id,
            conversation_id: conversation.to_string(),
            sort_sequence: raw.key.sort_sequence,
            server_id: raw.key.server_id,
            message_type,
            message_type_label: wx_db::msg_type_label(message_type),
            message_subtype,
            message_subtype_label: wx_db::msg_sub_type_label(message_type, message_subtype),
            sender,
            sender_display_name: None,
            created_at_unix: raw.key.create_time,
            status: raw.status,
            content,
            content_decode_state: decode_state,
            content_truncated: sender_truncated || truncated_field_count > 0,
        },
        content_decode_failed,
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SearchCursor {
    version: u32,
    kind: String,
    source_identity: String,
    query_digest: String,
    conversation_digest: Option<String>,
    create_time: i64,
    sort_sequence: i64,
    message_local_id: i64,
    table_ordinal: u32,
    row_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FallbackSearchCursor {
    version: u32,
    kind: String,
    source_identity: String,
    query_digest: String,
    conversation_digest: Option<String>,
    conversation_id: String,
    inner_message_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CursorKind {
    kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SearchKey {
    create_time: i64,
    sort_sequence: i64,
    message_local_id: i64,
    table_ordinal: u32,
    row_id: i64,
}

#[derive(Debug)]
struct RawSearchHit {
    key: SearchKey,
    local_type: i64,
    conversation_id: String,
    sender: String,
    snippet: String,
}

#[derive(Debug)]
struct NativeSearchTable {
    name: String,
    ordinal: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct CorpusContact {
    pub source_id: String,
    pub display_name: String,
    pub remark: Option<String>,
    pub nickname: Option<String>,
    pub alias: Option<String>,
    pub kind: ContactKind,
    pub is_account_holder: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CorpusConversation {
    pub source_id: String,
    pub display_name: String,
    pub kind: ConversationKind,
    pub contact_kind: ContactKind,
    table_name: String,
    shard_ids: Vec<u32>,
}

#[derive(Debug, Clone)]
pub(crate) struct CorpusInventory {
    pub contacts: Vec<CorpusContact>,
    pub conversations: Vec<CorpusConversation>,
    pub unmatched_message_table_count: usize,
    pub warnings: Vec<QueryWarning>,
    pub coverage_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CorpusMessageLocation {
    pub sort_sequence: i64,
    pub create_time: i64,
    pub server_id: i64,
    pub shard_id: u32,
    pub row_id: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct CorpusMessageMetadata {
    pub location: CorpusMessageLocation,
    pub local_type: i64,
    pub sender: Option<String>,
    pub is_account_holder: Option<bool>,
}

#[derive(Debug, Clone)]
pub(crate) struct CorpusMetadataScan {
    pub messages: Vec<CorpusMessageMetadata>,
    pub warnings: Vec<QueryWarning>,
    pub coverage_complete: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CorpusHydratedMessage {
    pub location: CorpusMessageLocation,
    pub canonical_id: String,
    pub sender: Option<String>,
    pub sender_display_name: Option<String>,
    pub is_account_holder: Option<bool>,
    pub message_type: u32,
    pub message_subtype: u32,
    pub payload_kind: String,
    pub text: Option<String>,
    pub text_truncated: bool,
    pub content_decode_failed: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CorpusHydration {
    pub messages: Vec<CorpusHydratedMessage>,
    pub warnings: Vec<QueryWarning>,
    pub coverage_complete: bool,
}

pub(crate) struct LiveCorpusReader<'source, 'key> {
    source: &'source LiveQuerySource<'key>,
    open_shards: OpenMessageShards,
    inventory: CorpusInventory,
    contact_display_names: BTreeMap<String, String>,
}

pub fn source_status(source: &LiveQuerySource<'_>) -> Result<SourceStatus, LiveQueryError> {
    // Authenticate both required core files with the selected access material.
    // Connections are closed before the filesystem inventory begins.
    drop(source.open_database(Path::new("contact/contact.db"))?);
    drop(source.open_database(Path::new("session/session.db"))?);

    let mut database_paths = Vec::new();
    let mut inventory_entries = 0usize;
    for entry in walkdir::WalkDir::new(&source.root).follow_links(false) {
        let entry = entry.map_err(|_| {
            LiveQueryError::UnsafeSource("source storage inventory could not be traversed".into())
        })?;
        inventory_entries = inventory_entries.saturating_add(1);
        if inventory_entries > MAXIMUM_SOURCE_INVENTORY_ENTRIES {
            return Err(LiveQueryError::UnsafeSource(
                "source storage inventory exceeds the fixed entry limit".into(),
            ));
        }
        let path = entry.path();
        let is_database_name = path.extension().and_then(|value| value.to_str()) == Some("db");
        if entry.file_type().is_symlink() {
            if is_database_name {
                return Err(LiveQueryError::UnsafeSource(
                    "source storage contains a symbolic-link database".into(),
                ));
            }
            continue;
        }
        if !entry.file_type().is_file() || !is_database_name {
            continue;
        }
        let relative_path = path.strip_prefix(&source.root).map_err(|_| {
            LiveQueryError::UnsafeSource("source database escaped the selected root".into())
        })?;
        source.safe_database_path(relative_path)?;
        database_paths.push(relative_path.to_path_buf());
        if database_paths.len() > MAXIMUM_SOURCE_DATABASE_FILES {
            return Err(LiveQueryError::UnsafeSource(
                "source database count exceeds the fixed status limit".into(),
            ));
        }
    }
    database_paths.sort();
    if database_paths.is_empty() {
        return Err(LiveQueryError::Database(
            "source contains no database files".into(),
        ));
    }

    let mut entries = Vec::with_capacity(database_paths.len());
    let mut database_bytes = 0u64;
    let mut write_ahead_log_count = 0usize;
    let mut write_ahead_log_bytes = 0u64;
    let mut shared_memory_count = 0usize;
    let mut shared_memory_bytes = 0u64;
    let mut rollback_journal_count = 0usize;
    let mut rollback_journal_bytes = 0u64;
    for relative_path in database_paths {
        let path = source.safe_database_path(&relative_path)?;
        let metadata = fs::metadata(&path).map_err(|_| {
            LiveQueryError::UnsafeSource("source database changed during status inventory".into())
        })?;
        let database_size = metadata.len();
        database_bytes = checked_storage_sum(database_bytes, database_size)?;
        let (wal_exists, wal_size) = sqlite_sidecar_size(&path, "-wal")?;
        let (shm_exists, shm_size) = sqlite_sidecar_size(&path, "-shm")?;
        let (journal_exists, journal_size) = sqlite_sidecar_size(&path, "-journal")?;
        write_ahead_log_count += usize::from(wal_exists);
        write_ahead_log_bytes = checked_storage_sum(write_ahead_log_bytes, wal_size)?;
        shared_memory_count += usize::from(shm_exists);
        shared_memory_bytes = checked_storage_sum(shared_memory_bytes, shm_size)?;
        rollback_journal_count += usize::from(journal_exists);
        rollback_journal_bytes = checked_storage_sum(rollback_journal_bytes, journal_size)?;
        let relative_path = relative_path.to_str().ok_or_else(|| {
            LiveQueryError::UnsafeSource("source database path is not valid UTF-8".into())
        })?;
        entries.push(SourceDatabaseSize {
            relative_path: relative_path.to_string(),
            database_bytes: database_size,
            write_ahead_log_bytes: wal_size,
            shared_memory_bytes: shm_size,
            rollback_journal_bytes: journal_size,
        });
    }
    let total_sqlite_storage_bytes = checked_storage_sum(
        checked_storage_sum(
            checked_storage_sum(database_bytes, write_ahead_log_bytes)?,
            shared_memory_bytes,
        )?,
        rollback_journal_bytes,
    )?;
    Ok(SourceStatus {
        schema: QUERY_SCHEMA,
        format_version: QUERY_FORMAT_VERSION,
        operation: "source.status",
        ok: true,
        source: source_description(source),
        observed_at_unix_milliseconds: now_unix_milliseconds(),
        database_count: entries.len(),
        database_bytes,
        write_ahead_log_count,
        write_ahead_log_bytes,
        shared_memory_count,
        shared_memory_bytes,
        rollback_journal_count,
        rollback_journal_bytes,
        total_sqlite_storage_bytes,
        entries,
    })
}

fn sqlite_sidecar_size(database_path: &Path, suffix: &str) -> Result<(bool, u64), LiveQueryError> {
    let file_name = database_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| LiveQueryError::UnsafeSource("database filename is invalid".into()))?;
    let path = database_path.with_file_name(format!("{file_name}{suffix}"));
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((false, 0)),
        Err(_) => {
            return Err(LiveQueryError::UnsafeSource(
                "SQLite sidecar could not be inspected".into(),
            ))
        }
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(LiveQueryError::UnsafeSource(
            "SQLite sidecar must be a current-user-owned regular file".into(),
        ));
    }
    Ok((true, metadata.len()))
}

fn checked_storage_sum(left: u64, right: u64) -> Result<u64, LiveQueryError> {
    left.checked_add(right)
        .ok_or_else(|| LiveQueryError::Database("source storage byte accounting overflowed".into()))
}

struct LoadedContactRecords {
    records: BTreeMap<String, ContactRecord>,
    warnings: Vec<QueryWarning>,
    coverage_complete: bool,
}

fn load_contact_records(
    source: &LiveQuerySource<'_>,
) -> Result<LoadedContactRecords, LiveQueryError> {
    let connection = source.open_database(Path::new("contact/contact.db"))?;
    let mut records = BTreeMap::<String, ContactRecord>::new();
    let mut warnings = Vec::new();
    let mut coverage_complete = true;
    let mut decode_failures = 0usize;

    let contact_columns = table_columns(&connection, "contact")?;
    let identifier_column = ["username", "user_name"]
        .into_iter()
        .find(|column| contact_columns.contains(*column))
        .ok_or_else(|| {
            LiveQueryError::Database(
                "contact schema has no compatible account identifier column".into(),
            )
        })?;
    let remark_column = ["remark", "remark_name"]
        .into_iter()
        .find(|column| contact_columns.contains(*column));
    let nickname_column = ["nick_name", "nickname"]
        .into_iter()
        .find(|column| contact_columns.contains(*column));
    let alias_column = ["alias"]
        .into_iter()
        .find(|column| contact_columns.contains(*column));
    let selected_column = |column: Option<&str>| {
        column
            .map(|column| format!("[{column}]"))
            .unwrap_or_else(|| "NULL".to_string())
    };
    let contact_sql = format!(
        "SELECT [{identifier_column}], {}, {}, {} FROM [contact] ORDER BY [{identifier_column}] ASC",
        selected_column(remark_column),
        selected_column(nickname_column),
        selected_column(alias_column),
    );
    let mut statement = connection
        .prepare(&contact_sql)
        .map_err(|error| database_error(&error.to_string()))?;
    let mut rows = statement
        .query([])
        .map_err(|error| database_error(&error.to_string()))?;
    while let Some(row) = rows
        .next()
        .map_err(|error| database_error(&error.to_string()))?
    {
        let id = match row
            .get_ref(0)
            .ok()
            .and_then(|value| decode_sqlite_text(value).ok())
        {
            Some(id)
                if !id.is_empty()
                    && id.len() <= MAX_CONVERSATION_ID_BYTES
                    && !id.contains('\0') =>
            {
                id
            }
            _ => {
                decode_failures = decode_failures.saturating_add(1);
                continue;
            }
        };
        let read_optional = |index: usize| -> Option<String> {
            row.get_ref(index)
                .ok()
                .and_then(|value| decode_sqlite_text(value).ok())
                .map(|value| truncate_utf8(value, MAX_PROJECTED_TEXT_BYTES).0)
                .filter(|value| !value.trim().is_empty())
        };
        records.insert(
            id.clone(),
            ContactRecord {
                id,
                remark: read_optional(1),
                nickname: read_optional(2),
                alias: read_optional(3),
                in_contact_table: true,
                in_chat_room_table: false,
            },
        );
        if records.len() > MAXIMUM_CONTACT_INVENTORY_ENTRIES {
            return Err(LiveQueryError::Database(format!(
                "contact inventory exceeds the fixed {MAXIMUM_CONTACT_INVENTORY_ENTRIES}-row safety limit"
            )));
        }
    }
    drop(rows);
    drop(statement);

    let has_chat_room = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'chat_room')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or_default()
        == 1;
    if has_chat_room {
        match table_columns(&connection, "chat_room") {
            Ok(columns) => {
                if let Some(identifier_column) = ["username", "user_name"]
                    .into_iter()
                    .find(|column| columns.contains(*column))
                {
                    let sql = format!(
                        "SELECT [{identifier_column}] FROM [chat_room] ORDER BY [{identifier_column}] ASC"
                    );
                    match connection.prepare(&sql) {
                        Ok(mut statement) => match statement.query([]) {
                            Ok(mut rows) => loop {
                                match rows.next() {
                                    Ok(Some(row)) => {
                                        let id = row
                                            .get_ref(0)
                                            .ok()
                                            .and_then(|value| decode_sqlite_text(value).ok());
                                        let Some(id) = id.filter(|id| {
                                            !id.is_empty()
                                                && id.len() <= MAX_CONVERSATION_ID_BYTES
                                                && !id.contains('\0')
                                        }) else {
                                            decode_failures = decode_failures.saturating_add(1);
                                            continue;
                                        };
                                        records
                                            .entry(id.clone())
                                            .and_modify(|record| {
                                                record.in_chat_room_table = true;
                                            })
                                            .or_insert(ContactRecord {
                                                id,
                                                remark: None,
                                                nickname: None,
                                                alias: None,
                                                in_contact_table: false,
                                                in_chat_room_table: true,
                                            });
                                        if records.len() > MAXIMUM_CONTACT_INVENTORY_ENTRIES {
                                            return Err(LiveQueryError::Database(format!(
                                                "contact inventory exceeds the fixed {MAXIMUM_CONTACT_INVENTORY_ENTRIES}-row safety limit"
                                            )));
                                        }
                                    }
                                    Ok(None) => break,
                                    Err(_) => {
                                        coverage_complete = false;
                                        warnings.push(QueryWarning {
                                            code: "chatRoomInventoryIncomplete",
                                            message: "the chat-room inventory became unreadable during the bounded contact read".into(),
                                            shard_id: None,
                                            count: None,
                                        });
                                        break;
                                    }
                                }
                            },
                            Err(_) => {
                                coverage_complete = false;
                                warnings.push(QueryWarning {
                                    code: "chatRoomInventoryUnavailable",
                                    message: "the chat-room inventory could not be queried with this source schema".into(),
                                    shard_id: None,
                                    count: None,
                                });
                            }
                        },
                        Err(_) => {
                            coverage_complete = false;
                            warnings.push(QueryWarning {
                                code: "chatRoomInventoryUnavailable",
                                message: "the chat-room inventory could not be prepared with this source schema".into(),
                                shard_id: None,
                                count: None,
                            });
                        }
                    }
                } else {
                    coverage_complete = false;
                    warnings.push(QueryWarning {
                        code: "chatRoomInventoryUnavailable",
                        message: "the chat-room table has no compatible account identifier column"
                            .into(),
                        shard_id: None,
                        count: None,
                    });
                }
            }
            Err(_) => {
                coverage_complete = false;
                warnings.push(QueryWarning {
                    code: "chatRoomInventoryUnavailable",
                    message: "the chat-room schema could not be inspected".into(),
                    shard_id: None,
                    count: None,
                });
            }
        }
    }
    if decode_failures > 0 {
        coverage_complete = false;
        warnings.push(QueryWarning {
            code: "contactDecodeFailed",
            message: "one or more contact identifiers could not be decoded safely".into(),
            shard_id: None,
            count: Some(decode_failures),
        });
    }
    coalesce_warnings(&mut warnings);
    Ok(LoadedContactRecords {
        records,
        warnings,
        coverage_complete,
    })
}

pub(crate) fn classify_contact_id(
    identifier: &str,
    account_holder: Option<&str>,
    in_contact_table: bool,
    in_chat_room_table: bool,
) -> ContactKind {
    if account_holder == Some(identifier) {
        return ContactKind::AccountHolder;
    }
    if in_chat_room_table || wx_db::is_group_chat(identifier) {
        return ContactKind::Group;
    }
    if identifier.starts_with("gh_") {
        return ContactKind::Official;
    }
    const SERVICE_IDENTIFIERS: &[&str] = &[
        "blogapp",
        "brandsessionholder",
        "facebookapp",
        "feedsapp",
        "filehelper",
        "floatbottle",
        "fmessage",
        "lbsapp",
        "masssendapp",
        "medianote",
        "newsapp",
        "notification_messages",
        "qmessage",
        "qqfriend",
        "qqmail",
        "readerapp",
        "recommendhelper",
        "shakeapp",
        "tmessage",
        "voiceinputapp",
        "weixin",
    ];
    if SERVICE_IDENTIFIERS.contains(&identifier) {
        ContactKind::Service
    } else if in_contact_table {
        ContactKind::Person
    } else {
        ContactKind::Unknown
    }
}

fn contact_display_name(record: &ContactRecord, kind: ContactKind) -> String {
    if kind == ContactKind::AccountHolder {
        return "You".to_string();
    }
    record
        .remark
        .as_ref()
        .or(record.nickname.as_ref())
        .or(record.alias.as_ref())
        .cloned()
        .unwrap_or_else(|| record.id.clone())
}

pub fn list_contacts(
    source: &LiveQuerySource<'_>,
    kind: Option<ContactKind>,
    include_details: bool,
    limit: usize,
    cursor: Option<&str>,
) -> Result<QueryEnvelope<ContactItem>, LiveQueryError> {
    validate_limit(limit)?;
    let cursor = cursor
        .map(decode_cursor::<ContactCursor>)
        .transpose()?
        .map(|cursor| validate_contact_cursor(source, kind, include_details, cursor))
        .transpose()?;
    let mut loaded = load_contact_records(source)?;
    if let Some(account_holder) = source.account_holder_source_id() {
        loaded
            .records
            .entry(account_holder.to_string())
            .or_insert(ContactRecord {
                id: account_holder.to_string(),
                remark: None,
                nickname: None,
                alias: None,
                in_contact_table: false,
                in_chat_room_table: false,
            });
    }
    let account_holder = source.account_holder_source_id();
    let mut items = loaded
        .records
        .values()
        .filter_map(|record| {
            let item_kind = classify_contact_id(
                &record.id,
                account_holder,
                record.in_contact_table,
                record.in_chat_room_table,
            );
            if kind.is_some_and(|kind| kind != item_kind)
                || cursor
                    .as_ref()
                    .is_some_and(|cursor| record.id <= cursor.identifier)
            {
                return None;
            }
            Some(ContactItem {
                id: record.id.clone(),
                display_name: contact_display_name(record, item_kind),
                kind: item_kind,
                is_account_holder: item_kind == ContactKind::AccountHolder,
                remark: include_details.then(|| record.remark.clone()).flatten(),
                nickname: include_details.then(|| record.nickname.clone()).flatten(),
                alias: include_details.then(|| record.alias.clone()).flatten(),
            })
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.id.cmp(&right.id));
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if has_more {
        items
            .last()
            .map(|item| {
                encode_cursor(&ContactCursor {
                    version: CURSOR_FORMAT_VERSION,
                    kind: "contacts.list".into(),
                    source_identity: source.identity.clone(),
                    contact_kind: kind.map(|kind| kind.as_str().to_string()),
                    include_details,
                    identifier: item.id.clone(),
                })
            })
            .transpose()?
    } else {
        None
    };
    Ok(QueryEnvelope {
        schema: QUERY_SCHEMA,
        format_version: QUERY_FORMAT_VERSION,
        operation: "contacts.list",
        ok: true,
        source: source_description(source),
        consistency: QueryConsistency {
            guarantee: "singleDatabaseReadStatementSeries",
            database_count: 1,
            cross_database_atomic: true,
            coverage_complete: loaded.coverage_complete,
            observed_at_unix_milliseconds: now_unix_milliseconds(),
        },
        page: QueryPage {
            limit,
            returned: items.len(),
            has_more,
            next_cursor,
        },
        warnings: loaded.warnings,
        items,
    })
}

fn validate_contact_cursor(
    source: &LiveQuerySource<'_>,
    kind: Option<ContactKind>,
    include_details: bool,
    cursor: ContactCursor,
) -> Result<ContactCursor, LiveQueryError> {
    if cursor.version != CURSOR_FORMAT_VERSION
        || cursor.kind != "contacts.list"
        || cursor.source_identity != source.identity
        || cursor.contact_kind.as_deref() != kind.map(ContactKind::as_str)
        || cursor.include_details != include_details
        || cursor.identifier.is_empty()
        || cursor.identifier.len() > MAX_CONVERSATION_ID_BYTES
    {
        return Err(LiveQueryError::InvalidCursor(
            "cursor does not belong to this contact filter and source".into(),
        ));
    }
    Ok(cursor)
}

impl<'source, 'key> LiveCorpusReader<'source, 'key> {
    pub(crate) fn open(source: &'source LiveQuerySource<'key>) -> Result<Self, LiveQueryError> {
        let mut loaded_contacts = load_contact_records(source)?;
        if let Some(account_holder) = source.account_holder_source_id() {
            loaded_contacts
                .records
                .entry(account_holder.to_string())
                .or_insert(ContactRecord {
                    id: account_holder.to_string(),
                    remark: None,
                    nickname: None,
                    alias: None,
                    in_contact_table: false,
                    in_chat_room_table: false,
                });
        }
        let account_holder = source.account_holder_source_id();
        let mut contacts = loaded_contacts
            .records
            .values()
            .map(|record| {
                let kind = classify_contact_id(
                    &record.id,
                    account_holder,
                    record.in_contact_table,
                    record.in_chat_room_table,
                );
                CorpusContact {
                    source_id: record.id.clone(),
                    // Personal memory preserves source identity instead of replacing
                    // the authenticated owner with the presentation label `You`.
                    display_name: if kind == ContactKind::AccountHolder {
                        record
                            .remark
                            .as_ref()
                            .or(record.nickname.as_ref())
                            .or(record.alias.as_ref())
                            .cloned()
                            .unwrap_or_else(|| record.id.clone())
                    } else {
                        contact_display_name(record, kind)
                    },
                    remark: record.remark.clone(),
                    nickname: record.nickname.clone(),
                    alias: record.alias.clone(),
                    kind,
                    is_account_holder: kind == ContactKind::AccountHolder,
                }
            })
            .collect::<Vec<_>>();
        contacts.sort_by(|left, right| left.source_id.cmp(&right.source_id));
        let contact_display_names = contacts
            .iter()
            .map(|contact| (contact.source_id.clone(), contact.display_name.clone()))
            .collect::<BTreeMap<_, _>>();

        let mut candidate_records = loaded_contacts.records;
        let session = source.open_database(Path::new("session/session.db"))?;
        reset_query_deadline(&session)?;
        let session_columns = table_columns(&session, "SessionTable")?;
        if !session_columns.contains("username") {
            return Err(LiveQueryError::Database(
                "session schema is missing required column username".into(),
            ));
        }
        let mut statement = session
            .prepare("SELECT username FROM SessionTable ORDER BY username ASC")
            .map_err(|error| database_error(&error.to_string()))?;
        let mut rows = statement
            .query([])
            .map_err(|error| database_error(&error.to_string()))?;
        let mut session_decode_failures = 0usize;
        while let Some(row) = rows
            .next()
            .map_err(|error| database_error(&error.to_string()))?
        {
            let id = row
                .get_ref(0)
                .ok()
                .and_then(|value| decode_sqlite_text(value).ok());
            let Some(id) = id.filter(|id| {
                !id.is_empty() && id.len() <= MAX_CONVERSATION_ID_BYTES && !id.contains('\0')
            }) else {
                session_decode_failures = session_decode_failures.saturating_add(1);
                continue;
            };
            candidate_records
                .entry(id.clone())
                .or_insert(ContactRecord {
                    id,
                    remark: None,
                    nickname: None,
                    alias: None,
                    in_contact_table: false,
                    in_chat_room_table: false,
                });
            if candidate_records.len() > MAXIMUM_CONTACT_INVENTORY_ENTRIES {
                return Err(LiveQueryError::Database(format!(
                    "conversation candidate inventory exceeds the fixed {MAXIMUM_CONTACT_INVENTORY_ENTRIES}-row safety limit"
                )));
            }
        }
        drop(rows);
        drop(statement);
        drop(session);

        let open_shards = open_message_shards(source)?;
        let mut warnings = loaded_contacts.warnings;
        warnings.extend(open_shards.warnings.clone());
        if session_decode_failures > 0 {
            warnings.push(QueryWarning {
                code: "sessionIdentifierDecodeFailed",
                message: "one or more session identifiers could not be decoded safely".into(),
                shard_id: None,
                count: Some(session_decode_failures),
            });
        }

        let mut actual_tables = BTreeMap::<String, (String, BTreeSet<u32>)>::new();
        let mut table_inventory_complete = true;
        for shard in &open_shards.shards {
            reset_query_deadline(&shard.connection)?;
            let mut statement = match shard.connection.prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'Msg_%' ORDER BY name ASC",
            ) {
                Ok(statement) => statement,
                Err(_) => {
                    table_inventory_complete = false;
                    warnings.push(QueryWarning {
                        code: "messageTableInventoryUnavailable",
                        message: "a message shard table inventory could not be prepared".into(),
                        shard_id: Some(shard.shard_id),
                        count: None,
                    });
                    continue;
                }
            };
            let mut rows = match statement.query([]) {
                Ok(rows) => rows,
                Err(_) => {
                    table_inventory_complete = false;
                    warnings.push(QueryWarning {
                        code: "messageTableInventoryUnavailable",
                        message: "a message shard table inventory could not be queried".into(),
                        shard_id: Some(shard.shard_id),
                        count: None,
                    });
                    continue;
                }
            };
            loop {
                let name = match rows.next() {
                    Ok(Some(row)) => row.get::<_, String>(0).ok(),
                    Ok(None) => break,
                    Err(_) => {
                        table_inventory_complete = false;
                        warnings.push(QueryWarning {
                            code: "messageTableInventoryIncomplete",
                            message: "a message shard table inventory became unreadable".into(),
                            shard_id: Some(shard.shard_id),
                            count: None,
                        });
                        break;
                    }
                };
                let Some(name) = name else {
                    continue;
                };
                let Some(digest) = name.strip_prefix("Msg_") else {
                    continue;
                };
                if digest.len() != 32 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    continue;
                }
                actual_tables
                    .entry(digest.to_ascii_lowercase())
                    .and_modify(|(_, shard_ids)| {
                        shard_ids.insert(shard.shard_id);
                    })
                    .or_insert_with(|| (name, BTreeSet::from([shard.shard_id])));
            }
        }

        let mut digest_candidates = BTreeMap::<String, String>::new();
        for identifier in candidate_records.keys() {
            let digest = format!("{:x}", md5::compute(identifier.as_bytes()));
            if digest_candidates
                .insert(digest, identifier.clone())
                .is_some()
            {
                return Err(LiveQueryError::Database(
                    "conversation candidate inventory contains an MD5 table-name collision".into(),
                ));
            }
        }
        let unmatched_message_table_count = actual_tables
            .keys()
            .filter(|digest| !digest_candidates.contains_key(*digest))
            .count();
        if unmatched_message_table_count > 0 {
            warnings.push(QueryWarning {
                code: "unmatchedMessageTable",
                message: "one or more hashed message tables could not be mapped back to an authorized session, contact, or chat-room identifier".into(),
                shard_id: None,
                count: Some(unmatched_message_table_count),
            });
        }

        let matched_digests = digest_candidates.keys().cloned().collect::<BTreeSet<_>>();
        let mut conversations = Vec::new();
        for (digest, identifier) in digest_candidates {
            let Some((table_name, shard_ids)) = actual_tables.get(&digest) else {
                continue;
            };
            let record = candidate_records.get(&identifier).ok_or_else(|| {
                LiveQueryError::Database("conversation candidate inventory changed".into())
            })?;
            let contact_kind = classify_contact_id(
                &record.id,
                account_holder,
                record.in_contact_table,
                record.in_chat_room_table,
            );
            let kind = match contact_kind {
                ContactKind::Group => ConversationKind::Group,
                ContactKind::Official => ConversationKind::Business,
                ContactKind::Service | ContactKind::AccountHolder => ConversationKind::System,
                ContactKind::Person | ContactKind::Unknown => ConversationKind::Direct,
            };
            conversations.push(CorpusConversation {
                source_id: identifier,
                display_name: contact_display_name(record, contact_kind),
                kind,
                contact_kind,
                table_name: table_name.clone(),
                shard_ids: shard_ids.iter().copied().collect(),
            });
        }
        // Preserve row coverage even when the source no longer exposes a reversible
        // session/contact identifier for a hashed message table. The synthetic source
        // identity records the actual hashed table digest because no reversible source
        // conversation ID exists. Personal-memory pages preserve that unresolved source
        // identity alongside their stable C###### join key. Coverage still reports the
        // mapping limitation, so callers never confuse complete row traversal with
        // recovered conversation identity.
        for (digest, (table_name, shard_ids)) in &actual_tables {
            if matched_digests.contains(digest) {
                continue;
            }
            conversations.push(CorpusConversation {
                source_id: format!("unresolved-table:{digest}"),
                display_name: "Unresolved conversation".into(),
                kind: ConversationKind::Unresolved,
                contact_kind: ContactKind::Unknown,
                table_name: table_name.clone(),
                shard_ids: shard_ids.iter().copied().collect(),
            });
        }
        conversations.sort_by(|left, right| left.source_id.cmp(&right.source_id));
        coalesce_warnings(&mut warnings);
        let coverage_complete = loaded_contacts.coverage_complete
            && table_inventory_complete
            && unmatched_message_table_count == 0
            && open_shards.warnings.is_empty()
            && session_decode_failures == 0;
        let inventory = CorpusInventory {
            contacts,
            conversations,
            unmatched_message_table_count,
            warnings,
            coverage_complete,
        };
        Ok(Self {
            source,
            open_shards,
            inventory,
            contact_display_names,
        })
    }

    pub(crate) fn inventory(&self) -> &CorpusInventory {
        &self.inventory
    }

    pub(crate) fn scan_metadata(
        &self,
        conversation: &CorpusConversation,
    ) -> Result<CorpusMetadataScan, LiveQueryError> {
        let mut messages = Vec::new();
        let mut warnings = Vec::new();
        let mut coverage_complete = true;
        for shard in self
            .open_shards
            .shards
            .iter()
            .filter(|shard| conversation.shard_ids.contains(&shard.shard_id))
        {
            reset_query_deadline(&shard.connection)?;
            if !message_table_exists(&shard.connection, &conversation.table_name)? {
                continue;
            }
            let Some(shape) = corpus_message_query_shape(
                &shard.connection,
                &conversation.table_name,
                shard.shard_id,
                &mut warnings,
            )?
            else {
                coverage_complete = false;
                continue;
            };
            let mut after_row_id = i64::MIN;
            loop {
                reset_query_deadline(&shard.connection)?;
                let sql = format!(
                    "SELECT m.rowid, m.sort_seq, m.server_id, m.local_type, {}, m.create_time \
                     FROM [{}] m {} WHERE m.rowid > ?1 ORDER BY m.rowid ASC LIMIT ?2",
                    shape.sender_expression, conversation.table_name, shape.sender_join,
                );
                let mut statement = match shard.connection.prepare(&sql) {
                    Ok(statement) => statement,
                    Err(_) => {
                        coverage_complete = false;
                        warnings.push(QueryWarning {
                            code: "corpusMetadataQueryFailed",
                            message: "a bounded metadata page could not be prepared".into(),
                            shard_id: Some(shard.shard_id),
                            count: None,
                        });
                        break;
                    }
                };
                let mut rows = match statement
                    .query(params![after_row_id, CORPUS_METADATA_PAGE_ROWS as i64])
                {
                    Ok(rows) => rows,
                    Err(_) => {
                        coverage_complete = false;
                        warnings.push(QueryWarning {
                            code: "corpusMetadataQueryFailed",
                            message: "a bounded metadata page could not be read".into(),
                            shard_id: Some(shard.shard_id),
                            count: None,
                        });
                        break;
                    }
                };
                let mut page_count = 0usize;
                let mut page_last_row_id = after_row_id;
                loop {
                    let row = match rows.next() {
                        Ok(Some(row)) => row,
                        Ok(None) => break,
                        Err(_) => {
                            coverage_complete = false;
                            warnings.push(QueryWarning {
                                code: "corpusMetadataRowFailed",
                                message: "a metadata row became unreadable during a bounded page"
                                    .into(),
                                shard_id: Some(shard.shard_id),
                                count: Some(1),
                            });
                            break;
                        }
                    };
                    let decoded = (|| -> Result<CorpusMessageMetadata, rusqlite::Error> {
                        let row_id = row.get::<_, i64>(0)?;
                        let sort_sequence = row.get::<_, i64>(1)?;
                        let server_id = row.get::<_, i64>(2)?;
                        let local_type = row.get::<_, i64>(3)?;
                        let sender = row
                            .get::<_, Option<String>>(4)?
                            .and_then(|value| normalized_source_identifier(&value));
                        let create_time = row.get::<_, i64>(5)?;
                        let is_account_holder = sender.as_deref().and_then(|sender| {
                            self.source
                                .account_holder_source_id()
                                .map(|account_holder| sender == account_holder)
                        });
                        Ok(CorpusMessageMetadata {
                            location: CorpusMessageLocation {
                                sort_sequence,
                                create_time,
                                server_id,
                                shard_id: shard.shard_id,
                                row_id,
                            },
                            local_type,
                            sender,
                            is_account_holder,
                        })
                    })();
                    match decoded {
                        Ok(message) => {
                            page_last_row_id = message.location.row_id;
                            messages.push(message);
                            page_count = page_count.saturating_add(1);
                        }
                        Err(_) => {
                            coverage_complete = false;
                            warnings.push(QueryWarning {
                                code: "corpusMetadataRowFailed",
                                message: "a metadata row had incompatible SQLite value types"
                                    .into(),
                                shard_id: Some(shard.shard_id),
                                count: Some(1),
                            });
                        }
                    }
                }
                if page_count == 0 || page_last_row_id <= after_row_id {
                    break;
                }
                after_row_id = page_last_row_id;
                if page_count < CORPUS_METADATA_PAGE_ROWS {
                    break;
                }
            }
        }
        messages.sort_by(|left, right| left.location.cmp(&right.location));
        coalesce_warnings(&mut warnings);
        Ok(CorpusMetadataScan {
            messages,
            warnings,
            coverage_complete,
        })
    }

    pub(crate) fn hydrate(
        &self,
        conversation: &CorpusConversation,
        selected: &[CorpusMessageLocation],
        maximum_text_bytes: usize,
    ) -> Result<CorpusHydration, LiveQueryError> {
        if maximum_text_bytes == 0 || maximum_text_bytes > MAX_PROJECTED_TEXT_BYTES {
            return Err(LiveQueryError::InvalidArgument(format!(
                "corpus message text bound must be between 1 and {MAX_PROJECTED_TEXT_BYTES} bytes"
            )));
        }
        let selected = selected.iter().cloned().collect::<BTreeSet<_>>();
        let mut projected = Vec::<(CorpusMessageLocation, MessageItem, bool)>::new();
        let mut warnings = Vec::new();
        let mut coverage_complete = true;
        for shard in self
            .open_shards
            .shards
            .iter()
            .filter(|shard| conversation.shard_ids.contains(&shard.shard_id))
        {
            let locations = selected
                .iter()
                .filter(|location| location.shard_id == shard.shard_id)
                .collect::<Vec<_>>();
            if locations.is_empty() {
                continue;
            }
            reset_query_deadline(&shard.connection)?;
            if !message_table_exists(&shard.connection, &conversation.table_name)? {
                coverage_complete = false;
                warnings.push(QueryWarning {
                    code: "corpusHydrationRowMissing",
                    message: "a selected message table was no longer present during hydration"
                        .into(),
                    shard_id: Some(shard.shard_id),
                    count: Some(locations.len()),
                });
                continue;
            }
            let Some(shape) = corpus_message_query_shape(
                &shard.connection,
                &conversation.table_name,
                shard.shard_id,
                &mut warnings,
            )?
            else {
                coverage_complete = false;
                continue;
            };
            for chunk in locations.chunks(CORPUS_HYDRATION_ROWS) {
                reset_query_deadline(&shard.connection)?;
                let placeholders = (1..=chunk.len())
                    .map(|index| format!("?{index}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "SELECT m.rowid, m.sort_seq, m.server_id, m.local_type, {}, m.create_time, \
                            m.message_content, {}, {}, {}, {} \
                     FROM [{}] m {} WHERE m.rowid IN ({placeholders})",
                    shape.sender_expression,
                    shape.packed_info,
                    shape.status,
                    shape.compression_type,
                    shape.compressed_content,
                    conversation.table_name,
                    shape.sender_join,
                );
                let row_ids = chunk.iter().map(|location| location.row_id);
                let mut statement = match shard.connection.prepare(&sql) {
                    Ok(statement) => statement,
                    Err(_) => {
                        coverage_complete = false;
                        warnings.push(QueryWarning {
                            code: "corpusHydrationQueryFailed",
                            message:
                                "a bounded selected-message hydration query could not be prepared"
                                    .into(),
                            shard_id: Some(shard.shard_id),
                            count: Some(chunk.len()),
                        });
                        continue;
                    }
                };
                let mut rows = match statement.query(rusqlite::params_from_iter(row_ids)) {
                    Ok(rows) => rows,
                    Err(_) => {
                        coverage_complete = false;
                        warnings.push(QueryWarning {
                            code: "corpusHydrationQueryFailed",
                            message: "a bounded selected-message hydration query could not be read"
                                .into(),
                            shard_id: Some(shard.shard_id),
                            count: Some(chunk.len()),
                        });
                        continue;
                    }
                };
                loop {
                    let row = match rows.next() {
                        Ok(Some(row)) => row,
                        Ok(None) => break,
                        Err(_) => {
                            coverage_complete = false;
                            warnings.push(QueryWarning {
                                code: "corpusHydrationRowFailed",
                                message: "a selected message became unreadable during hydration"
                                    .into(),
                                shard_id: Some(shard.shard_id),
                                count: Some(1),
                            });
                            break;
                        }
                    };
                    let raw = match raw_message_from_row(row, shard.shard_id) {
                        Ok(raw) => raw,
                        Err(_) => {
                            coverage_complete = false;
                            warnings.push(QueryWarning {
                                code: "corpusHydrationRowFailed",
                                message: "a selected message had incompatible SQLite value types"
                                    .into(),
                                shard_id: Some(shard.shard_id),
                                count: Some(1),
                            });
                            continue;
                        }
                    };
                    let location = CorpusMessageLocation {
                        sort_sequence: raw.key.sort_sequence,
                        create_time: raw.key.create_time,
                        server_id: raw.key.server_id,
                        shard_id: raw.key.shard_id,
                        row_id: raw.key.row_id,
                    };
                    if !selected.contains(&location) {
                        coverage_complete = false;
                        warnings.push(QueryWarning {
                            code: "corpusHydrationIdentityChanged",
                            message: "a selected row no longer matched its metadata identity and was rejected".into(),
                            shard_id: Some(shard.shard_id),
                            count: Some(1),
                        });
                        continue;
                    }
                    let digest =
                        digest_text("greenbubbles-conversation-v1", &conversation.source_id);
                    match project_message(self.source, &conversation.source_id, &digest, raw) {
                        Ok((message, decode_failed)) => {
                            projected.push((location, message, decode_failed));
                        }
                        Err(_) => {
                            coverage_complete = false;
                            warnings.push(QueryWarning {
                                code: "corpusHydrationProjectionFailed",
                                message: "a selected message could not be projected safely".into(),
                                shard_id: Some(shard.shard_id),
                                count: Some(1),
                            });
                        }
                    }
                }
            }
        }

        let hydrated_locations = projected
            .iter()
            .map(|(location, _, _)| location.clone())
            .collect::<BTreeSet<_>>();
        let missing = selected.len().saturating_sub(hydrated_locations.len());
        if missing > 0 {
            coverage_complete = false;
            warnings.push(QueryWarning {
                code: "corpusHydrationRowMissing",
                message: "one or more selected metadata identities were absent during hydration"
                    .into(),
                shard_id: None,
                count: Some(missing),
            });
        }

        let account_holder = self.source.account_holder_source_id();
        let mut messages = projected
            .into_iter()
            .map(|(location, message, decode_failed)| {
                let sender = (!message.sender.is_empty()).then_some(message.sender);
                let is_account_holder = sender.as_deref().and_then(|sender| {
                    account_holder.map(|account_holder| sender == account_holder)
                });
                let sender_display_name = if is_account_holder == Some(true) {
                    Some("You".to_string())
                } else {
                    sender
                        .as_ref()
                        .and_then(|sender| self.contact_display_names.get(sender).cloned())
                };
                let (payload_kind, text, summarized_truncated) =
                    summarize_decoded_payload(&message.content, maximum_text_bytes);
                CorpusHydratedMessage {
                    location,
                    canonical_id: message.id,
                    sender,
                    sender_display_name,
                    is_account_holder,
                    message_type: message.message_type,
                    message_subtype: message.message_subtype,
                    payload_kind,
                    text,
                    text_truncated: message.content_truncated || summarized_truncated,
                    content_decode_failed: decode_failed,
                }
            })
            .collect::<Vec<_>>();
        messages.sort_by(|left, right| left.location.cmp(&right.location));
        coalesce_warnings(&mut warnings);
        Ok(CorpusHydration {
            messages,
            warnings,
            coverage_complete,
        })
    }
}

struct CorpusMessageQueryShape {
    sender_expression: String,
    sender_join: &'static str,
    packed_info: String,
    status: String,
    compression_type: String,
    compressed_content: String,
}

fn message_table_exists(connection: &Connection, table_name: &str) -> Result<bool, LiveQueryError> {
    reset_query_deadline(connection)?;
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table_name],
            |row| row.get::<_, i64>(0),
        )
        .map(|value| value == 1)
        .map_err(|error| database_error(&error.to_string()))
}

fn corpus_message_query_shape(
    connection: &Connection,
    table_name: &str,
    shard_id: u32,
    warnings: &mut Vec<QueryWarning>,
) -> Result<Option<CorpusMessageQueryShape>, LiveQueryError> {
    reset_query_deadline(connection)?;
    let columns = match table_columns(connection, table_name) {
        Ok(columns) => columns,
        Err(_) => {
            warnings.push(QueryWarning {
                code: "unsupportedCorpusMessageSchema",
                message: "a message table schema could not be inspected for corpus selection"
                    .into(),
                shard_id: Some(shard_id),
                count: None,
            });
            return Ok(None);
        }
    };
    let required = [
        "sort_seq",
        "server_id",
        "local_type",
        "create_time",
        "message_content",
    ];
    if required.iter().any(|column| !columns.contains(*column)) {
        warnings.push(QueryWarning {
            code: "unsupportedCorpusMessageSchema",
            message: "a message table is missing required corpus-selection columns".into(),
            shard_id: Some(shard_id),
            count: None,
        });
        return Ok(None);
    }
    let has_name_table = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'Name2Id')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or_default()
        == 1;
    let has_sender = columns.contains("real_sender_id") && has_name_table;
    Ok(Some(CorpusMessageQueryShape {
        sender_expression: if has_sender {
            "COALESCE(n.user_name, '')".into()
        } else {
            "''".into()
        },
        sender_join: if has_sender {
            "LEFT JOIN Name2Id n ON m.real_sender_id = n.rowid"
        } else {
            ""
        },
        packed_info: optional_message_column(&columns, "packed_info_data"),
        status: optional_message_column(&columns, "status"),
        compression_type: optional_message_column(&columns, "WCDB_CT_message_content"),
        compressed_content: optional_message_column(&columns, "compress_content"),
    }))
}

fn reset_query_deadline(connection: &Connection) -> Result<(), LiveQueryError> {
    let deadline = Instant::now() + MAXIMUM_SQL_STATEMENT_DURATION;
    connection
        .progress_handler(10_000, Some(move || Instant::now() >= deadline))
        .map_err(|error| database_error(&error.to_string()))
}

pub fn list_conversations(
    source: &LiveQuerySource<'_>,
    limit: usize,
    cursor: Option<&str>,
) -> Result<QueryEnvelope<ConversationItem>, LiveQueryError> {
    validate_limit(limit)?;
    let cursor = cursor
        .map(decode_conversation_cursor)
        .transpose()?
        .map(|cursor| validate_conversation_cursor(source, cursor))
        .transpose()?;

    let connection = source.open_database(Path::new("session/session.db"))?;
    let columns = table_columns(&connection, "SessionTable")?;
    for required in ["username", "sort_timestamp", "summary"] {
        if !columns.contains(required) {
            return Err(LiveQueryError::Database(format!(
                "session schema is missing required column {required}"
            )));
        }
    }

    let last_type = optional_column(&columns, "last_msg_type");
    let last_sender = optional_column(&columns, "last_msg_sender");
    let last_display = optional_column(&columns, "last_sender_display_name");
    let fetch_limit = limit.saturating_add(1);
    let sql = if cursor.is_some() {
        format!(
            "SELECT username, sort_timestamp, summary, {last_type}, {last_sender}, {last_display} \
             FROM SessionTable \
             WHERE sort_timestamp < ?1 \
                OR (sort_timestamp = ?1 AND username > ?2) \
             ORDER BY sort_timestamp DESC, username ASC \
             LIMIT ?3"
        )
    } else {
        format!(
            "SELECT username, sort_timestamp, summary, {last_type}, {last_sender}, {last_display} \
             FROM SessionTable \
             ORDER BY sort_timestamp DESC, username ASC \
             LIMIT ?1"
        )
    };

    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| database_error(&error.to_string()))?;
    let mut rows = if let Some(cursor) = &cursor {
        statement.query(params![
            cursor.sort_timestamp,
            cursor.username,
            fetch_limit as i64
        ])
    } else {
        statement.query(params![fetch_limit as i64])
    }
    .map_err(|error| database_error(&error.to_string()))?;

    let mut decoded_rows = Vec::with_capacity(fetch_limit);
    let mut summary_decode_failures = 0usize;
    while let Some(row) = rows
        .next()
        .map_err(|error| database_error(&error.to_string()))?
    {
        let username = row
            .get::<_, String>(0)
            .map_err(|error| database_error(&error.to_string()))?;
        let sort_timestamp = row
            .get::<_, i64>(1)
            .map_err(|error| database_error(&error.to_string()))?;
        let (summary, summary_decode_state, summary_truncated) = match decode_sqlite_text(
            row.get_ref(2)
                .map_err(|error| database_error(&error.to_string()))?,
        ) {
            Ok(value) => {
                let value = strip_group_summary_prefix(&username, value);
                let (value, truncated) = truncate_utf8(value, MAX_PROJECTED_TEXT_BYTES);
                (Some(value), "complete", truncated)
            }
            Err(()) => {
                summary_decode_failures += 1;
                (None, "failed", false)
            }
        };
        let last_message_type = row
            .get::<_, Option<i64>>(3)
            .map_err(|error| database_error(&error.to_string()))?
            .map(|value| value as u32);
        let last_message_sender = bounded_optional_string(
            row.get::<_, Option<String>>(4)
                .map_err(|error| database_error(&error.to_string()))?,
        );
        let last_sender_display_name = bounded_optional_string(
            row.get::<_, Option<String>>(5)
                .map_err(|error| database_error(&error.to_string()))?,
        );
        decoded_rows.push(ConversationRow {
            username,
            summary,
            summary_decode_state,
            summary_truncated,
            sort_timestamp,
            last_message_type,
            last_message_sender,
            last_sender_display_name,
        });
    }
    drop(rows);
    drop(statement);
    drop(connection);

    let has_more = decoded_rows.len() > limit;
    decoded_rows.truncate(limit);
    let next_cursor = if has_more {
        decoded_rows
            .last()
            .map(|row| encode_conversation_cursor(source, row))
            .transpose()?
    } else {
        None
    };
    let mut items = decoded_rows
        .into_iter()
        .map(|row| ConversationItem {
            id: row.username,
            display_name: None,
            summary: row.summary,
            summary_decode_state: row.summary_decode_state,
            summary_truncated: row.summary_truncated,
            sort_timestamp: row.sort_timestamp,
            last_message_type: row.last_message_type,
            last_message_sender: row.last_message_sender,
            last_sender_display_name: row.last_sender_display_name,
        })
        .collect::<Vec<_>>();
    let mut warnings = Vec::new();
    if summary_decode_failures > 0 {
        warnings.push(QueryWarning {
            code: "summaryDecodeFailed",
            message: "one or more conversation summaries could not be decoded".into(),
            shard_id: None,
            count: Some(summary_decode_failures),
        });
    }
    let contact_enrichment = enrich_conversation_items(source, &mut items)?;
    warnings.extend(contact_enrichment.warnings);
    coalesce_warnings(&mut warnings);
    let database_count = 1 + usize::from(contact_enrichment.database_read);

    Ok(QueryEnvelope {
        schema: QUERY_SCHEMA,
        format_version: QUERY_FORMAT_VERSION,
        operation: "conversations.list",
        ok: true,
        source: source_description(source),
        consistency: QueryConsistency {
            guarantee: if contact_enrichment.database_read {
                "independentDatabaseReadStatements"
            } else {
                "singleDatabaseReadStatement"
            },
            database_count,
            cross_database_atomic: database_count <= 1,
            coverage_complete: contact_enrichment.coverage_complete,
            observed_at_unix_milliseconds: now_unix_milliseconds(),
        },
        page: QueryPage {
            limit,
            returned: items.len(),
            has_more,
            next_cursor,
        },
        warnings,
        items,
    })
}

pub fn find_conversation(
    source: &LiveQuerySource<'_>,
    conversation: &str,
) -> Result<Option<ConversationItem>, LiveQueryError> {
    validate_conversation_id(conversation)?;
    let connection = source.open_database(Path::new("session/session.db"))?;
    let columns = table_columns(&connection, "SessionTable")?;
    for required in ["username", "sort_timestamp", "summary"] {
        if !columns.contains(required) {
            return Err(LiveQueryError::Database(format!(
                "session schema is missing required column {required}"
            )));
        }
    }

    let last_type = optional_column(&columns, "last_msg_type");
    let last_sender = optional_column(&columns, "last_msg_sender");
    let last_display = optional_column(&columns, "last_sender_display_name");
    let sql = format!(
        "SELECT username, sort_timestamp, summary, {last_type}, {last_sender}, {last_display} \
         FROM SessionTable WHERE username = ?1 LIMIT 1"
    );
    let mut row = connection
        .query_row(&sql, [conversation], |row| {
            let username = row.get::<_, String>(0)?;
            let sort_timestamp = row.get::<_, i64>(1)?;
            let summary = decode_sqlite_text(row.get_ref(2)?).ok();
            let summary = summary.map(|value| strip_group_summary_prefix(&username, value));
            let (summary, summary_truncated) = summary
                .map(|value| truncate_utf8(value, MAX_PROJECTED_TEXT_BYTES))
                .map_or((None, false), |(value, truncated)| (Some(value), truncated));
            let summary_decode_state = if summary.is_some() {
                "complete"
            } else {
                "failed"
            };
            Ok(ConversationItem {
                id: username,
                display_name: None,
                summary,
                summary_decode_state,
                summary_truncated,
                sort_timestamp,
                last_message_type: row.get::<_, Option<i64>>(3)?.map(|value| value as u32),
                last_message_sender: bounded_optional_string(row.get(4)?),
                last_sender_display_name: bounded_optional_string(row.get(5)?),
            })
        })
        .optional()
        .map_err(|error| database_error(&error.to_string()))?;
    drop(connection);
    if let Some(item) = row.as_mut() {
        let enrichment = resolve_contact_display_names(source, [item.id.as_str()])?;
        item.display_name = enrichment.display_names.get(&item.id).cloned();
    }
    Ok(row)
}

pub fn find_conversations(
    source: &LiveQuerySource<'_>,
    conversations: &[String],
) -> Result<std::collections::BTreeMap<String, ConversationItem>, LiveQueryError> {
    if conversations.len() > MAX_PAGE_LIMIT {
        return Err(LiveQueryError::InvalidArgument(format!(
            "conversation metadata batch exceeds the {MAX_PAGE_LIMIT}-item safety limit"
        )));
    }
    if conversations.is_empty() {
        return Ok(std::collections::BTreeMap::new());
    }
    for conversation in conversations {
        validate_conversation_id(conversation)?;
    }
    let connection = source.open_database(Path::new("session/session.db"))?;
    let columns = table_columns(&connection, "SessionTable")?;
    for required in ["username", "sort_timestamp", "summary"] {
        if !columns.contains(required) {
            return Err(LiveQueryError::Database(format!(
                "session schema is missing required column {required}"
            )));
        }
    }
    let last_type = optional_column(&columns, "last_msg_type");
    let last_sender = optional_column(&columns, "last_msg_sender");
    let last_display = optional_column(&columns, "last_sender_display_name");
    let placeholders = (1..=conversations.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT username, sort_timestamp, summary, {last_type}, {last_sender}, {last_display} \
         FROM SessionTable WHERE username IN ({placeholders})"
    );
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| database_error(&error.to_string()))?;
    let mut rows = statement
        .query(rusqlite::params_from_iter(conversations.iter()))
        .map_err(|error| database_error(&error.to_string()))?;
    let mut items = std::collections::BTreeMap::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| database_error(&error.to_string()))?
    {
        let username = row
            .get::<_, String>(0)
            .map_err(|error| database_error(&error.to_string()))?;
        let summary = decode_sqlite_text(
            row.get_ref(2)
                .map_err(|error| database_error(&error.to_string()))?,
        )
        .ok()
        .map(|value| strip_group_summary_prefix(&username, value));
        let (summary, summary_truncated) = summary
            .map(|value| truncate_utf8(value, MAX_PROJECTED_TEXT_BYTES))
            .map_or((None, false), |(value, truncated)| (Some(value), truncated));
        let summary_decode_state = if summary.is_some() {
            "complete"
        } else {
            "failed"
        };
        let item = ConversationItem {
            id: username.clone(),
            display_name: None,
            summary,
            summary_decode_state,
            summary_truncated,
            sort_timestamp: row
                .get(1)
                .map_err(|error| database_error(&error.to_string()))?,
            last_message_type: row
                .get::<_, Option<i64>>(3)
                .map_err(|error| database_error(&error.to_string()))?
                .map(|value| value as u32),
            last_message_sender: bounded_optional_string(
                row.get(4)
                    .map_err(|error| database_error(&error.to_string()))?,
            ),
            last_sender_display_name: bounded_optional_string(
                row.get(5)
                    .map_err(|error| database_error(&error.to_string()))?,
            ),
        };
        if items.insert(username, item).is_some() {
            return Err(LiveQueryError::Database(
                "session database repeats a requested conversation identity".into(),
            ));
        }
    }
    drop(rows);
    drop(statement);
    drop(connection);
    let enrichment =
        resolve_contact_display_names(source, items.values().map(|item| item.id.as_str()))?;
    for item in items.values_mut() {
        item.display_name = enrichment.display_names.get(&item.id).cloned();
    }
    Ok(items)
}

pub fn list_messages(
    source: &LiveQuerySource<'_>,
    conversation: &str,
    limit: usize,
    cursor: Option<&str>,
) -> Result<QueryEnvelope<MessageItem>, LiveQueryError> {
    list_messages_in_time_range(source, conversation, limit, cursor, None, None)
}

fn open_message_shards(source: &LiveQuerySource<'_>) -> Result<OpenMessageShards, LiveQueryError> {
    let inventory = source.message_shards()?;
    let mut shards = Vec::with_capacity(inventory.len());
    let mut warnings = Vec::new();
    for shard in inventory {
        match source.open_database(&shard.relative_path) {
            Ok(connection) => shards.push(OpenMessageShard {
                shard_id: shard.shard_id,
                connection,
            }),
            Err(_) => warnings.push(QueryWarning {
                code: "shardUnavailable",
                message: "a message shard could not be opened read-only".into(),
                shard_id: Some(shard.shard_id),
                count: None,
            }),
        }
    }
    if shards.is_empty() {
        return Err(LiveQueryError::Database(
            "no message shard could be opened read-only".into(),
        ));
    }
    Ok(OpenMessageShards { shards, warnings })
}

pub(crate) fn list_messages_in_time_range(
    source: &LiveQuerySource<'_>,
    conversation: &str,
    limit: usize,
    cursor: Option<&str>,
    not_before_unix: Option<i64>,
    not_after_unix: Option<i64>,
) -> Result<QueryEnvelope<MessageItem>, LiveQueryError> {
    list_messages_in_time_range_with_open_shards(
        source,
        conversation,
        limit,
        cursor,
        not_before_unix,
        not_after_unix,
        None,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn list_messages_in_time_range_with_open_shards(
    source: &LiveQuerySource<'_>,
    conversation: &str,
    limit: usize,
    cursor: Option<&str>,
    not_before_unix: Option<i64>,
    not_after_unix: Option<i64>,
    open_shards: Option<&OpenMessageShards>,
    enrich_contacts: bool,
) -> Result<QueryEnvelope<MessageItem>, LiveQueryError> {
    validate_limit(limit)?;
    validate_conversation_id(conversation)?;
    if not_before_unix
        .zip(not_after_unix)
        .is_some_and(|(start, end)| start > end)
    {
        return Err(LiveQueryError::InvalidArgument(
            "message time range is inverted".into(),
        ));
    }
    let conversation_digest = digest_text("greenbubbles-conversation-v1", conversation);
    let cursor = cursor
        .map(decode_message_cursor)
        .transpose()?
        .map(|cursor| validate_message_cursor(source, &conversation_digest, cursor))
        .transpose()?;

    let table_name = format!("Msg_{:x}", md5::compute(conversation.as_bytes()));
    let locally_opened_shards;
    let open_shards = if let Some(open_shards) = open_shards {
        open_shards
    } else {
        locally_opened_shards = open_message_shards(source)?;
        &locally_opened_shards
    };
    let fetch_limit = limit.saturating_add(1);
    let mut messages = Vec::new();
    let mut warnings = open_shards.warnings.clone();
    let mut queried_database_count = open_shards.shards.len();

    for shard in &open_shards.shards {
        let connection = &shard.connection;
        let table_exists = match connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [&table_name],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(value) => value == 1,
            Err(_) => {
                warnings.push(QueryWarning {
                    code: "shardSchemaUnavailable",
                    message: "a message shard schema could not be inspected".into(),
                    shard_id: Some(shard.shard_id),
                    count: None,
                });
                continue;
            }
        };
        if !table_exists {
            continue;
        }

        let columns = match table_columns(connection, &table_name) {
            Ok(columns) => columns,
            Err(_) => {
                warnings.push(QueryWarning {
                    code: "shardSchemaUnavailable",
                    message: "a message table schema could not be inspected".into(),
                    shard_id: Some(shard.shard_id),
                    count: None,
                });
                continue;
            }
        };
        let required = [
            "sort_seq",
            "server_id",
            "local_type",
            "create_time",
            "message_content",
        ];
        if required.iter().any(|column| !columns.contains(*column)) {
            warnings.push(QueryWarning {
                code: "unsupportedMessageSchema",
                message: "a message table is missing required typed-query columns".into(),
                shard_id: Some(shard.shard_id),
                count: None,
            });
            continue;
        }

        let has_name_table = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'Name2Id')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
            == 1;
        let has_sender = columns.contains("real_sender_id") && has_name_table;
        let sender_expression = if has_sender {
            "COALESCE(n.user_name, '')"
        } else {
            "''"
        };
        let sender_join = if has_sender {
            "LEFT JOIN Name2Id n ON m.real_sender_id = n.rowid"
        } else {
            ""
        };
        let packed_info = optional_message_column(&columns, "packed_info_data");
        let status = optional_message_column(&columns, "status");
        let compression_type = optional_message_column(&columns, "WCDB_CT_message_content");
        let compressed_content = optional_message_column(&columns, "compress_content");

        let sql = format!(
            "SELECT m.rowid, m.sort_seq, m.server_id, m.local_type, {sender_expression}, \
                    m.create_time, m.message_content, {packed_info}, {status}, \
                    {compression_type}, {compressed_content} \
             FROM [{table_name}] m {sender_join} \
             WHERE (:cursor_sort IS NULL OR \
                    (m.sort_seq, m.create_time, m.server_id, {shard_id}, m.rowid) \
                    < (:cursor_sort, :cursor_time, :cursor_server, :cursor_shard, :cursor_row)) \
               AND (:not_before IS NULL OR m.create_time >= :not_before) \
               AND (:not_after IS NULL OR m.create_time <= :not_after) \
             ORDER BY m.sort_seq DESC, m.create_time DESC, m.server_id DESC, m.rowid DESC \
             LIMIT :fetch_limit",
            shard_id = shard.shard_id,
        );
        let mut statement = match connection.prepare(&sql) {
            Ok(statement) => statement,
            Err(_) => {
                warnings.push(QueryWarning {
                    code: "shardQueryFailed",
                    message: "a bounded message query could not be prepared".into(),
                    shard_id: Some(shard.shard_id),
                    count: None,
                });
                continue;
            }
        };
        let cursor_sort = cursor.as_ref().map(|value| value.sort_sequence);
        let cursor_time = cursor.as_ref().map(|value| value.create_time);
        let cursor_server = cursor.as_ref().map(|value| value.server_id);
        let cursor_shard = cursor.as_ref().map(|value| value.shard_id as i64);
        let cursor_row = cursor.as_ref().map(|value| value.row_id);
        let query_result = statement.query(rusqlite::named_params! {
            ":cursor_sort": cursor_sort,
            ":cursor_time": cursor_time,
            ":cursor_server": cursor_server,
            ":cursor_shard": cursor_shard,
            ":cursor_row": cursor_row,
            ":not_before": not_before_unix,
            ":not_after": not_after_unix,
            ":fetch_limit": fetch_limit as i64,
        });
        let mut rows = match query_result {
            Ok(rows) => rows,
            Err(_) => {
                warnings.push(QueryWarning {
                    code: "shardQueryFailed",
                    message: "a bounded message query could not be executed".into(),
                    shard_id: Some(shard.shard_id),
                    count: None,
                });
                continue;
            }
        };
        loop {
            let row = match rows.next() {
                Ok(Some(row)) => row,
                Ok(None) => break,
                Err(_) => {
                    warnings.push(QueryWarning {
                        code: "shardRowReadFailed",
                        message: "a message shard changed or became unreadable during a page"
                            .into(),
                        shard_id: Some(shard.shard_id),
                        count: None,
                    });
                    break;
                }
            };
            let raw = raw_message_from_row(row, shard.shard_id);
            match raw {
                Ok(raw) => messages.push(raw),
                Err(_) => warnings.push(QueryWarning {
                    code: "messageRowDecodeFailed",
                    message: "one message row had incompatible SQLite value types".into(),
                    shard_id: Some(shard.shard_id),
                    count: Some(1),
                }),
            }
        }
    }

    if queried_database_count == 0 {
        return Err(LiveQueryError::Database(
            "no message shard could be opened read-only".into(),
        ));
    }

    messages.sort_unstable_by(|left, right| right.key.cmp(&left.key));
    let has_more = messages.len() > limit;
    messages.truncate(limit);
    let next_cursor = if has_more {
        messages
            .last()
            .map(|message| {
                encode_message_cursor(source, &conversation_digest, "messages.list", &message.key)
            })
            .transpose()?
    } else {
        None
    };

    let mut content_decode_failures = 0usize;
    let mut projected = Vec::with_capacity(messages.len());
    for raw in messages {
        let (item, content_decode_failed) =
            project_message(source, conversation, &conversation_digest, raw)?;
        content_decode_failures += usize::from(content_decode_failed);
        projected.push(item);
    }
    if content_decode_failures > 0 {
        warnings.push(QueryWarning {
            code: "messageContentDecodeFailed",
            message: "one or more message bodies could not be decoded".into(),
            shard_id: None,
            count: Some(content_decode_failures),
        });
    }
    let contact_enrichment = if enrich_contacts {
        enrich_message_items(source, &mut projected)?
    } else {
        ContactDisplayNameEnrichment {
            display_names: BTreeMap::new(),
            database_read: false,
            coverage_complete: true,
            warnings: Vec::new(),
        }
    };
    let contact_database_read = contact_enrichment.database_read;
    let contact_coverage_complete = contact_enrichment.coverage_complete;
    queried_database_count =
        queried_database_count.saturating_add(usize::from(contact_database_read));
    warnings.extend(contact_enrichment.warnings);
    coalesce_warnings(&mut warnings);
    let coverage_complete = contact_coverage_complete
        && warnings.iter().all(|warning| {
            !matches!(
                warning.code,
                "shardUnavailable"
                    | "shardSchemaUnavailable"
                    | "unsupportedMessageSchema"
                    | "shardQueryFailed"
                    | "shardRowReadFailed"
                    | "messageRowDecodeFailed"
                    | "messageContentDecodeFailed"
            )
        });

    Ok(QueryEnvelope {
        schema: QUERY_SCHEMA,
        format_version: QUERY_FORMAT_VERSION,
        operation: "messages.list",
        ok: true,
        source: source_description(source),
        consistency: QueryConsistency {
            guarantee: "perDatabaseReadStatement",
            database_count: queried_database_count,
            cross_database_atomic: queried_database_count <= 1,
            coverage_complete,
            observed_at_unix_milliseconds: now_unix_milliseconds(),
        },
        page: QueryPage {
            limit,
            returned: projected.len(),
            has_more,
            next_cursor,
        },
        warnings,
        items: projected,
    })
}

pub fn get_message(
    source: &LiveQuerySource<'_>,
    conversation: &str,
    message_id: &str,
) -> Result<QueryResourceEnvelope<MessageItem>, LiveQueryError> {
    validate_conversation_id(conversation)?;
    let conversation_digest = digest_text("greenbubbles-conversation-v1", conversation);
    let key = validate_message_identity(
        source,
        &conversation_digest,
        decode_message_cursor(message_id)?,
    )?;
    let shard = source
        .message_shards()?
        .into_iter()
        .find(|shard| shard.shard_id == key.shard_id)
        .ok_or_else(|| {
            LiveQueryError::NotFound(
                "the message shard named by this identity is no longer available".into(),
            )
        })?;
    let connection = source.open_database(&shard.relative_path)?;
    let table_name = format!("Msg_{:x}", md5::compute(conversation.as_bytes()));
    let table_exists = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [&table_name],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| database_error(&error.to_string()))?
        == 1;
    if !table_exists {
        return Err(LiveQueryError::NotFound(
            "the conversation table named by this message identity is unavailable".into(),
        ));
    }
    let columns = table_columns(&connection, &table_name)?;
    for required in [
        "sort_seq",
        "server_id",
        "local_type",
        "create_time",
        "message_content",
    ] {
        if !columns.contains(required) {
            return Err(LiveQueryError::Database(
                "the selected message table has an incompatible schema".into(),
            ));
        }
    }

    let has_name_table = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'Name2Id')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        == 1;
    let has_sender = columns.contains("real_sender_id") && has_name_table;
    let sender_expression = if has_sender {
        "COALESCE(n.user_name, '')"
    } else {
        "''"
    };
    let sender_join = if has_sender {
        "LEFT JOIN Name2Id n ON m.real_sender_id = n.rowid"
    } else {
        ""
    };
    let packed_info = optional_message_column(&columns, "packed_info_data");
    let status = optional_message_column(&columns, "status");
    let compression_type = optional_message_column(&columns, "WCDB_CT_message_content");
    let compressed_content = optional_message_column(&columns, "compress_content");
    let sql = format!(
        "SELECT m.rowid, m.sort_seq, m.server_id, m.local_type, {sender_expression}, \
                m.create_time, m.message_content, {packed_info}, {status}, \
                {compression_type}, {compressed_content} \
         FROM [{table_name}] m {sender_join} \
         WHERE m.rowid = ?1 AND m.sort_seq = ?2 AND m.create_time = ?3 \
           AND m.server_id = ?4 \
         LIMIT 1"
    );
    let raw = connection
        .query_row(
            &sql,
            params![
                key.row_id,
                key.sort_sequence,
                key.create_time,
                key.server_id
            ],
            |row| raw_message_from_row(row, key.shard_id),
        )
        .optional()
        .map_err(|error| database_error(&error.to_string()))?
        .ok_or_else(|| {
            LiveQueryError::NotFound(
                "the message named by this identity is no longer available".into(),
            )
        })?;
    let (item, content_decode_failed) =
        project_message(source, conversation, &conversation_digest, raw)?;
    let mut items = vec![item];
    let contact_enrichment = enrich_message_items(source, &mut items)?;
    let contact_database_read = contact_enrichment.database_read;
    let contact_coverage_complete = contact_enrichment.coverage_complete;
    let mut warnings = if content_decode_failed {
        vec![QueryWarning {
            code: "messageContentDecodeFailed",
            message: "the selected message body could not be decoded".into(),
            shard_id: None,
            count: Some(1),
        }]
    } else {
        Vec::new()
    };
    warnings.extend(contact_enrichment.warnings);
    coalesce_warnings(&mut warnings);
    let item = items
        .pop()
        .expect("exact message enrichment preserves one selected item");
    let database_count = 1 + usize::from(contact_database_read);

    Ok(QueryResourceEnvelope {
        schema: QUERY_SCHEMA,
        format_version: QUERY_FORMAT_VERSION,
        operation: "message.get",
        ok: true,
        source: source_description(source),
        consistency: QueryConsistency {
            guarantee: if contact_database_read {
                "independentDatabaseReadStatements"
            } else {
                "singleDatabaseReadStatement"
            },
            database_count,
            cross_database_atomic: database_count <= 1,
            coverage_complete: !content_decode_failed && contact_coverage_complete,
            observed_at_unix_milliseconds: now_unix_milliseconds(),
        },
        warnings,
        item,
    })
}

pub fn get_search_result_message(
    source: &LiveQuerySource<'_>,
    conversation: &str,
    search_result_id: &str,
) -> Result<QueryResourceEnvelope<MessageItem>, LiveQueryError> {
    validate_conversation_id(conversation)?;
    let conversation_digest = digest_text("greenbubbles-search-conversation-v1", conversation);
    let cursor = decode_search_cursor(search_result_id)?;
    if cursor.version != CURSOR_FORMAT_VERSION
        || cursor.kind != "message.search.identity"
        || cursor.source_identity != source.identity
        || cursor.conversation_digest.as_deref() != Some(conversation_digest.as_str())
    {
        return Err(LiveQueryError::InvalidCursor(
            "search result identity does not belong to this source and conversation".into(),
        ));
    }

    let table_name = format!("Msg_{:x}", md5::compute(conversation.as_bytes()));
    let mut match_item = None;
    let mut queried_database_count = 0usize;
    for shard in source.message_shards()? {
        let connection = source.open_database(&shard.relative_path)?;
        queried_database_count += 1;
        let table_exists = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [&table_name],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| database_error(&error.to_string()))?
            == 1;
        if !table_exists {
            continue;
        }
        let columns = table_columns(&connection, &table_name)?;
        for required in [
            "sort_seq",
            "server_id",
            "local_type",
            "create_time",
            "message_content",
        ] {
            if !columns.contains(required) {
                return Err(LiveQueryError::Database(
                    "the selected message table has an incompatible schema".into(),
                ));
            }
        }

        let has_name_table = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'Name2Id')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
            == 1;
        let has_sender = columns.contains("real_sender_id") && has_name_table;
        let sender_expression = if has_sender {
            "COALESCE(n.user_name, '')"
        } else {
            "''"
        };
        let sender_join = if has_sender {
            "LEFT JOIN Name2Id n ON m.real_sender_id = n.rowid"
        } else {
            ""
        };
        let local_id_column = ["local_id", "message_local_id", "msg_local_id", "meslocalid"]
            .into_iter()
            .find(|name| columns.contains(*name));
        let local_id_expression = local_id_column
            .map(|name| format!("m.[{name}]"))
            .unwrap_or_else(|| "m.rowid".to_string());
        let packed_info = optional_message_column(&columns, "packed_info_data");
        let status = optional_message_column(&columns, "status");
        let compression_type = optional_message_column(&columns, "WCDB_CT_message_content");
        let compressed_content = optional_message_column(&columns, "compress_content");
        let sql = format!(
            "SELECT m.rowid, m.sort_seq, m.server_id, m.local_type, {sender_expression}, \
                    m.create_time, m.message_content, {packed_info}, {status}, \
                    {compression_type}, {compressed_content} \
             FROM [{table_name}] m {sender_join} \
             WHERE {local_id_expression} = ?1 AND m.sort_seq = ?2 AND m.create_time = ?3 \
             LIMIT 2"
        );
        let mut statement = connection
            .prepare(&sql)
            .map_err(|error| database_error(&error.to_string()))?;
        let mut rows = statement
            .query(params![
                cursor.message_local_id,
                cursor.sort_sequence,
                cursor.create_time,
            ])
            .map_err(|error| database_error(&error.to_string()))?;
        while let Some(row) = rows
            .next()
            .map_err(|error| database_error(&error.to_string()))?
        {
            let raw = raw_message_from_row(row, shard.shard_id)
                .map_err(|error| database_error(&error.to_string()))?;
            if match_item.is_some() {
                return Err(LiveQueryError::Database(
                    "the native search identity maps to more than one source message".into(),
                ));
            }
            match_item = Some(raw);
        }
    }
    let raw = match_item.ok_or_else(|| {
        LiveQueryError::NotFound(
            "the message named by this native search result is no longer available".into(),
        )
    })?;
    let message_digest = digest_text("greenbubbles-conversation-v1", conversation);
    let (item, content_decode_failed) =
        project_message(source, conversation, &message_digest, raw)?;
    let mut items = vec![item];
    let contact_enrichment = enrich_message_items(source, &mut items)?;
    let contact_database_read = contact_enrichment.database_read;
    let contact_coverage_complete = contact_enrichment.coverage_complete;
    let mut item = items
        .pop()
        .expect("exact native-search enrichment preserves one selected item");
    item.id = search_result_id.to_string();
    let mut warnings = if content_decode_failed {
        vec![QueryWarning {
            code: "messageContentDecodeFailed",
            message: "the selected message body could not be decoded".into(),
            shard_id: None,
            count: Some(1),
        }]
    } else {
        Vec::new()
    };
    warnings.extend(contact_enrichment.warnings);
    coalesce_warnings(&mut warnings);
    queried_database_count =
        queried_database_count.saturating_add(usize::from(contact_database_read));
    Ok(QueryResourceEnvelope {
        schema: QUERY_SCHEMA,
        format_version: QUERY_FORMAT_VERSION,
        operation: "message.get",
        ok: true,
        source: source_description(source),
        consistency: QueryConsistency {
            guarantee: "exactSearchIdentityLookup",
            database_count: queried_database_count,
            cross_database_atomic: queried_database_count <= 1,
            coverage_complete: !content_decode_failed && contact_coverage_complete,
            observed_at_unix_milliseconds: now_unix_milliseconds(),
        },
        warnings,
        item,
    })
}

pub fn search_messages(
    source: &LiveQuerySource<'_>,
    query: &str,
    conversation: Option<&str>,
    limit: usize,
    cursor: Option<&str>,
) -> Result<QueryEnvelope<SearchItem>, LiveQueryError> {
    search_messages_in_time_range(source, query, conversation, limit, cursor, None, None)
}

pub(crate) fn search_messages_in_time_range(
    source: &LiveQuerySource<'_>,
    query: &str,
    conversation: Option<&str>,
    limit: usize,
    cursor: Option<&str>,
    not_before_unix: Option<i64>,
    not_after_unix: Option<i64>,
) -> Result<QueryEnvelope<SearchItem>, LiveQueryError> {
    let cursor_kind = cursor.map(decode_cursor_kind).transpose()?;
    if cursor_kind.as_deref() == Some(FALLBACK_SEARCH_CURSOR_KIND) {
        return search_messages_fallback_in_time_range(
            source,
            query,
            conversation,
            limit,
            cursor,
            not_before_unix,
            not_after_unix,
        );
    }

    match search_messages_native_in_time_range(
        source,
        query,
        conversation,
        limit,
        cursor,
        not_before_unix,
        not_after_unix,
    ) {
        Err(LiveQueryError::SearchUnavailable(_)) if cursor.is_none() => {
            search_messages_fallback_in_time_range(
                source,
                query,
                conversation,
                limit,
                None,
                not_before_unix,
                not_after_unix,
            )
        }
        result => result,
    }
}

fn search_messages_native_in_time_range(
    source: &LiveQuerySource<'_>,
    query: &str,
    conversation: Option<&str>,
    limit: usize,
    cursor: Option<&str>,
    not_before_unix: Option<i64>,
    not_after_unix: Option<i64>,
) -> Result<QueryEnvelope<SearchItem>, LiveQueryError> {
    if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
        return Err(LiveQueryError::InvalidArgument(format!(
            "--limit must be between 1 and {MAX_SEARCH_LIMIT} for search"
        )));
    }
    let normalized_query = query.trim();
    if normalized_query.is_empty()
        || normalized_query.len() > MAX_SEARCH_QUERY_BYTES
        || normalized_query.contains('\0')
    {
        return Err(LiveQueryError::InvalidArgument(
            "search query is empty or outside safe limits".into(),
        ));
    }
    if let Some(conversation) = conversation {
        validate_conversation_id(conversation)?;
    }
    if not_before_unix
        .zip(not_after_unix)
        .is_some_and(|(start, end)| start > end)
    {
        return Err(LiveQueryError::InvalidArgument(
            "search time range is inverted".into(),
        ));
    }
    let query_digest = digest_text("greenbubbles-search-query-v1", normalized_query);
    let conversation_digest =
        conversation.map(|value| digest_text("greenbubbles-search-conversation-v1", value));
    let cursor = cursor
        .map(decode_search_cursor)
        .transpose()?
        .map(|cursor| {
            validate_search_cursor(
                source,
                &query_digest,
                conversation_digest.as_deref(),
                cursor,
            )
        })
        .transpose()?;

    let fts_relative_path = Path::new("message/message_fts.db");
    if source.safe_database_path(fts_relative_path).is_err() {
        return Err(LiveQueryError::SearchUnavailable(
            "the selected source has no compatible native message FTS database".into(),
        ));
    }
    let connection = source.open_database(fts_relative_path)?;
    wx_context::register_mm_fts_tokenizer(&connection).map_err(|_| {
        LiveQueryError::SearchUnavailable(
            "the native WeChat FTS tokenizer could not be registered".into(),
        )
    })?;
    let search_tables = native_search_tables(&connection)?;
    if search_tables.is_empty() {
        return Err(LiveQueryError::SearchUnavailable(
            "the native FTS database contains no compatible message index tables".into(),
        ));
    }
    let name_columns = table_columns(&connection, "name2id")?;
    let name_column = if name_columns.contains("username") {
        "username"
    } else if name_columns.contains("user_name") {
        "user_name"
    } else {
        return Err(LiveQueryError::SearchUnavailable(
            "the native FTS name mapping schema is incompatible".into(),
        ));
    };

    let mut selects = Vec::with_capacity(search_tables.len());
    for table in &search_tables {
        let name = &table.name;
        let ordinal = table.ordinal;
        selects.push(format!(
            "SELECT f.acontent AS snippet, f.message_local_id, f.sort_seq, f.local_type, \
                    f.create_time, COALESCE(talker.[{name_column}], '') AS conversation_id, \
                    COALESCE(sender.[{name_column}], '') AS sender, {ordinal} AS table_ordinal, \
                    f.rowid AS source_row_id \
             FROM [{name}] f \
             LEFT JOIN name2id talker ON f.session_id = talker.rowid \
             LEFT JOIN name2id sender ON f.sender_id = sender.rowid \
             WHERE [{name}] MATCH ?1 \
               AND (?2 IS NULL OR talker.[{name_column}] = ?2) \
               AND (?3 IS NULL OR \
                    (f.create_time, f.sort_seq, f.message_local_id, {ordinal}, f.rowid) \
                    < (?3, ?4, ?5, ?6, ?7)) \
               AND (?9 IS NULL OR f.create_time >= ?9) \
               AND (?10 IS NULL OR f.create_time <= ?10)"
        ));
    }
    let sql = format!(
        "SELECT snippet, message_local_id, sort_seq, local_type, create_time, \
                conversation_id, sender, table_ordinal, source_row_id \
         FROM ({}) \
         ORDER BY create_time DESC, sort_seq DESC, message_local_id DESC, \
                  table_ordinal DESC, source_row_id DESC \
         LIMIT ?8",
        selects.join(" UNION ALL ")
    );
    let fts_query = literal_native_fts_query(normalized_query);
    let fetch_limit = limit.saturating_add(1);
    let cursor_create_time = cursor.as_ref().map(|value| value.create_time);
    let cursor_sort_sequence = cursor
        .as_ref()
        .map(|value| value.sort_sequence)
        .unwrap_or_default();
    let cursor_local_id = cursor
        .as_ref()
        .map(|value| value.message_local_id)
        .unwrap_or_default();
    let cursor_table = cursor
        .as_ref()
        .map(|value| value.table_ordinal as i64)
        .unwrap_or_default();
    let cursor_row = cursor
        .as_ref()
        .map(|value| value.row_id)
        .unwrap_or_default();

    let mut statement = connection.prepare(&sql).map_err(|_| {
        LiveQueryError::SearchUnavailable(
            "the native FTS query could not be prepared against this schema".into(),
        )
    })?;
    let mut rows = statement
        .query(params![
            fts_query,
            conversation,
            cursor_create_time,
            cursor_sort_sequence,
            cursor_local_id,
            cursor_table,
            cursor_row,
            fetch_limit as i64,
            not_before_unix,
            not_after_unix
        ])
        .map_err(|_| {
            LiveQueryError::SearchUnavailable(
                "the native FTS query could not be executed against this index".into(),
            )
        })?;
    let mut hits = Vec::with_capacity(fetch_limit);
    while let Some(row) = rows.next().map_err(|_| {
        LiveQueryError::SearchUnavailable(
            "the native FTS result became unreadable during the bounded page".into(),
        )
    })? {
        let snippet = decode_sqlite_text(
            row.get_ref(0)
                .map_err(|_| LiveQueryError::SearchUnavailable("invalid FTS snippet".into()))?,
        )
        .map_err(|_| LiveQueryError::SearchUnavailable("invalid FTS snippet".into()))?;
        hits.push(RawSearchHit {
            key: SearchKey {
                message_local_id: row.get(1).map_err(|_| {
                    LiveQueryError::SearchUnavailable("invalid FTS message identity".into())
                })?,
                sort_sequence: row.get(2).map_err(|_| {
                    LiveQueryError::SearchUnavailable("invalid FTS ordering key".into())
                })?,
                create_time: row.get(4).map_err(|_| {
                    LiveQueryError::SearchUnavailable("invalid FTS timestamp".into())
                })?,
                table_ordinal: row.get::<_, i64>(7).map_err(|_| {
                    LiveQueryError::SearchUnavailable("invalid FTS table identity".into())
                })? as u32,
                row_id: row.get(8).map_err(|_| {
                    LiveQueryError::SearchUnavailable("invalid FTS row identity".into())
                })?,
            },
            local_type: row.get(3).map_err(|_| {
                LiveQueryError::SearchUnavailable("invalid FTS message type".into())
            })?,
            conversation_id: row.get(5).map_err(|_| {
                LiveQueryError::SearchUnavailable("invalid FTS conversation mapping".into())
            })?,
            sender: row.get(6).map_err(|_| {
                LiveQueryError::SearchUnavailable("invalid FTS sender mapping".into())
            })?,
            snippet,
        });
    }
    drop(rows);
    drop(statement);
    drop(connection);

    let has_more = hits.len() > limit;
    hits.truncate(limit);
    let next_cursor = if has_more {
        hits.last()
            .map(|hit| {
                encode_search_cursor(
                    source,
                    &query_digest,
                    conversation_digest.as_deref(),
                    "messages.search",
                    &hit.key,
                )
            })
            .transpose()?
    } else {
        None
    };
    let mut items = Vec::with_capacity(hits.len());
    for hit in hits {
        let (message_type, message_subtype) = wx_db::split_local_type(hit.local_type);
        let (snippet, snippet_truncated) = truncate_utf8(hit.snippet, MAX_PROJECTED_TEXT_BYTES);
        let (sender, _) = truncate_utf8(hit.sender, MAX_PROJECTED_TEXT_BYTES);
        let (conversation_id, _) = truncate_utf8(hit.conversation_id, MAX_CONVERSATION_ID_BYTES);
        let identity_conversation_digest =
            digest_text("greenbubbles-search-conversation-v1", &conversation_id);
        let id = encode_search_cursor(
            source,
            &query_digest,
            Some(&identity_conversation_digest),
            "message.search.identity",
            &hit.key,
        )?;
        items.push(SearchItem {
            id,
            conversation_id,
            sender,
            sender_display_name: None,
            created_at_unix: hit.key.create_time,
            sort_sequence: hit.key.sort_sequence,
            message_local_id: hit.key.message_local_id,
            message_type,
            message_type_label: wx_db::msg_type_label(message_type),
            message_subtype,
            message_subtype_label: wx_db::msg_sub_type_label(message_type, message_subtype),
            snippet,
            snippet_truncated,
        });
    }
    let contact_enrichment = enrich_search_items(source, &mut items)?;
    let contact_database_read = contact_enrichment.database_read;
    let mut warnings = vec![QueryWarning {
        code: "nativeSearchIndexFreshnessUnverified",
        message: "results come from WeChat's native FTS database; its lag relative to message shards is not independently proven".into(),
        shard_id: None,
        count: None,
    }];
    warnings.extend(contact_enrichment.warnings);
    coalesce_warnings(&mut warnings);
    let database_count = 1 + usize::from(contact_database_read);

    Ok(QueryEnvelope {
        schema: QUERY_SCHEMA,
        format_version: QUERY_FORMAT_VERSION,
        operation: "messages.search",
        ok: true,
        source: source_description(source),
        consistency: QueryConsistency {
            guarantee: if contact_database_read {
                "nativeFtsAndContactReadStatements"
            } else {
                "singleNativeFtsReadStatement"
            },
            database_count,
            cross_database_atomic: database_count <= 1,
            coverage_complete: false,
            observed_at_unix_milliseconds: now_unix_milliseconds(),
        },
        page: QueryPage {
            limit,
            returned: items.len(),
            has_more,
            next_cursor,
        },
        warnings,
        items,
    })
}

fn search_messages_fallback_in_time_range(
    source: &LiveQuerySource<'_>,
    query: &str,
    conversation: Option<&str>,
    limit: usize,
    cursor: Option<&str>,
    not_before_unix: Option<i64>,
    not_after_unix: Option<i64>,
) -> Result<QueryEnvelope<SearchItem>, LiveQueryError> {
    if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
        return Err(LiveQueryError::InvalidArgument(format!(
            "--limit must be between 1 and {MAX_SEARCH_LIMIT} for search"
        )));
    }
    let normalized_query = query.trim();
    if normalized_query.is_empty()
        || normalized_query.len() > MAX_SEARCH_QUERY_BYTES
        || normalized_query.contains('\0')
    {
        return Err(LiveQueryError::InvalidArgument(
            "search query is empty or outside safe limits".into(),
        ));
    }
    if let Some(conversation) = conversation {
        validate_conversation_id(conversation)?;
    }
    if not_before_unix
        .zip(not_after_unix)
        .is_some_and(|(start, end)| start > end)
    {
        return Err(LiveQueryError::InvalidArgument(
            "search time range is inverted".into(),
        ));
    }

    let query_digest = digest_text("greenbubbles-search-query-v1", normalized_query);
    let conversation_digest =
        conversation.map(|value| digest_text("greenbubbles-search-conversation-v1", value));
    let decoded_cursor = cursor
        .map(decode_fallback_search_cursor)
        .transpose()?
        .map(|cursor| {
            validate_fallback_search_cursor(
                source,
                &query_digest,
                conversation,
                conversation_digest.as_deref(),
                cursor,
            )
        })
        .transpose()?;

    let (conversation_ids, next_unscanned_conversation, initial_inner_cursor) =
        if let Some(conversation) = conversation {
            (
                vec![conversation.to_string()],
                None,
                decoded_cursor.and_then(|cursor| cursor.inner_message_cursor),
            )
        } else {
            let current = decoded_cursor
                .as_ref()
                .map(|cursor| cursor.conversation_id.clone());
            let initial_inner = decoded_cursor.and_then(|cursor| cursor.inner_message_cursor);
            let mut ids = if let Some(current) = current.as_ref() {
                let mut ids = vec![current.clone()];
                ids.extend(fallback_conversations_after(
                    source,
                    Some(current),
                    MAX_FALLBACK_SEARCH_CONVERSATIONS_PER_PAGE,
                )?);
                ids
            } else {
                fallback_conversations_after(
                    source,
                    None,
                    MAX_FALLBACK_SEARCH_CONVERSATIONS_PER_PAGE.saturating_add(1),
                )?
            };
            let next = if ids.len() > MAX_FALLBACK_SEARCH_CONVERSATIONS_PER_PAGE {
                Some(ids.remove(MAX_FALLBACK_SEARCH_CONVERSATIONS_PER_PAGE))
            } else {
                None
            };
            (ids, next, initial_inner)
        };
    let open_shards = (!conversation_ids.is_empty())
        .then(|| open_message_shards(source))
        .transpose()?;

    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut scanned_messages = 0usize;
    let mut queried_database_count = usize::from(conversation.is_none());
    let mut statement_count = usize::from(conversation.is_none());
    let normalized_query_lower = normalized_query.to_lowercase();
    let mut next_cursor = None;
    let mut has_more = false;
    let mut inner_cursor = initial_inner_cursor;

    for (conversation_index, conversation_id) in conversation_ids.iter().enumerate() {
        if scanned_messages >= MAX_FALLBACK_SEARCH_MESSAGES_PER_PAGE {
            has_more = true;
            next_cursor = Some(encode_fallback_search_cursor(
                source,
                &query_digest,
                conversation_digest.as_deref(),
                conversation_id,
                inner_cursor.as_deref(),
            )?);
            break;
        }

        let remaining_scan = MAX_FALLBACK_SEARCH_MESSAGES_PER_PAGE
            .saturating_sub(scanned_messages)
            .max(1);
        let page = list_messages_in_time_range_with_open_shards(
            source,
            conversation_id,
            remaining_scan.min(MAX_PAGE_LIMIT),
            inner_cursor.as_deref(),
            not_before_unix,
            not_after_unix,
            Some(
                open_shards
                    .as_ref()
                    .expect("non-empty fallback conversation set opens message shards"),
            ),
            false,
        )?;
        statement_count = statement_count.saturating_add(1);
        queried_database_count =
            queried_database_count.saturating_add(page.consistency.database_count);
        warnings.extend(page.warnings.iter().cloned());
        let page_has_more = page.page.has_more;
        let page_next_cursor = page.page.next_cursor.clone();
        let page_item_count = page.items.len();

        for (message_index, message) in page.items.into_iter().enumerate() {
            scanned_messages = scanned_messages.saturating_add(1);
            let (continuation, source_row_id) =
                message_identity_to_list_cursor(source, conversation_id, &message.id)?;
            let fallback_match = fallback_search_match(
                &message.content,
                &normalized_query_lower,
                message.content_truncated,
            );
            if let Some((snippet, snippet_truncated)) = fallback_match {
                items.push(SearchItem {
                    id: message.id,
                    conversation_id: message.conversation_id,
                    sender: message.sender,
                    sender_display_name: message.sender_display_name,
                    created_at_unix: message.created_at_unix,
                    sort_sequence: message.sort_sequence,
                    message_local_id: source_row_id,
                    message_type: message.message_type,
                    message_type_label: message.message_type_label,
                    message_subtype: message.message_subtype,
                    message_subtype_label: message.message_subtype_label,
                    snippet,
                    snippet_truncated,
                });
            }

            if items.len() == limit {
                let current_page_has_unscanned = message_index + 1 < page_item_count;
                let later_conversation_is_known = conversation_index + 1 < conversation_ids.len()
                    || next_unscanned_conversation.is_some();
                has_more =
                    current_page_has_unscanned || page_has_more || later_conversation_is_known;
                if has_more {
                    next_cursor = Some(encode_fallback_search_cursor(
                        source,
                        &query_digest,
                        conversation_digest.as_deref(),
                        conversation_id,
                        Some(&continuation),
                    )?);
                }
                break;
            }
        }

        if items.len() == limit {
            break;
        }
        if page_has_more {
            has_more = true;
            let continuation = page_next_cursor.ok_or_else(|| {
                LiveQueryError::Database(
                    "bounded fallback search page omitted its continuation".into(),
                )
            })?;
            next_cursor = Some(encode_fallback_search_cursor(
                source,
                &query_digest,
                conversation_digest.as_deref(),
                conversation_id,
                Some(&continuation),
            )?);
            break;
        }

        inner_cursor = None;
        if scanned_messages >= MAX_FALLBACK_SEARCH_MESSAGES_PER_PAGE {
            if let Some(next_conversation) = conversation_ids.get(conversation_index + 1) {
                has_more = true;
                next_cursor = Some(encode_fallback_search_cursor(
                    source,
                    &query_digest,
                    conversation_digest.as_deref(),
                    next_conversation,
                    None,
                )?);
            } else if let Some(next_conversation) = next_unscanned_conversation.as_deref() {
                has_more = true;
                next_cursor = Some(encode_fallback_search_cursor(
                    source,
                    &query_digest,
                    conversation_digest.as_deref(),
                    next_conversation,
                    None,
                )?);
            }
            break;
        }
    }

    if !has_more {
        if let Some(next_conversation) = next_unscanned_conversation.as_deref() {
            has_more = true;
            next_cursor = Some(encode_fallback_search_cursor(
                source,
                &query_digest,
                conversation_digest.as_deref(),
                next_conversation,
                None,
            )?);
        }
    }

    let contact_enrichment = enrich_search_items(source, &mut items)?;
    queried_database_count =
        queried_database_count.saturating_add(usize::from(contact_enrichment.database_read));
    statement_count = statement_count.saturating_add(usize::from(contact_enrichment.database_read));
    warnings.extend(contact_enrichment.warnings);

    warnings.push(QueryWarning {
        code: "fallbackSearchSourceWindowBounded",
        message: format!(
            "native FTS is unavailable; this page decoded at most {MAX_FALLBACK_SEARCH_MESSAGES_PER_PAGE} source messages and may return no matches before its continuation"
        ),
        shard_id: None,
        count: Some(scanned_messages),
    });
    if conversation.is_none() {
        warnings.push(QueryWarning {
            code: "fallbackSearchOrderedByConversation",
            message: "fallback search scans conversations in deterministic identifier order, then messages newest-first within each conversation".into(),
            shard_id: None,
            count: None,
        });
    }
    coalesce_warnings(&mut warnings);

    Ok(QueryEnvelope {
        schema: QUERY_SCHEMA,
        format_version: QUERY_FORMAT_VERSION,
        operation: "messages.search",
        ok: true,
        source: source_description(source),
        consistency: QueryConsistency {
            guarantee: "boundedDecodedSourceWindow",
            database_count: queried_database_count,
            cross_database_atomic: statement_count == 1 && queried_database_count <= 1,
            coverage_complete: false,
            observed_at_unix_milliseconds: now_unix_milliseconds(),
        },
        page: QueryPage {
            limit,
            returned: items.len(),
            has_more,
            next_cursor,
        },
        warnings,
        items,
    })
}

fn fallback_conversations_after(
    source: &LiveQuerySource<'_>,
    after: Option<&str>,
    limit: usize,
) -> Result<Vec<String>, LiveQueryError> {
    let limit = limit
        .max(1)
        .min(MAX_FALLBACK_SEARCH_CONVERSATIONS_PER_PAGE.saturating_add(1));
    let connection = source.open_database(Path::new("session/session.db"))?;
    let columns = table_columns(&connection, "SessionTable")?;
    if !columns.contains("username") {
        return Err(LiveQueryError::Database(
            "session schema is missing the conversation identifier required for fallback search"
                .into(),
        ));
    }
    let mut statement = connection
        .prepare(
            "SELECT DISTINCT username FROM SessionTable \
             WHERE (?1 IS NULL OR username > ?1) \
             ORDER BY username ASC LIMIT ?2",
        )
        .map_err(|error| database_error(&error.to_string()))?;
    let mut rows = statement
        .query(params![after, limit as i64])
        .map_err(|error| database_error(&error.to_string()))?;
    let mut conversations = Vec::with_capacity(limit);
    while let Some(row) = rows
        .next()
        .map_err(|error| database_error(&error.to_string()))?
    {
        let conversation = row
            .get::<_, String>(0)
            .map_err(|error| database_error(&error.to_string()))?;
        validate_conversation_id(&conversation)?;
        conversations.push(conversation);
    }
    Ok(conversations)
}

fn fallback_search_match(
    content: &Value,
    normalized_query_lower: &str,
    source_was_truncated: bool,
) -> Option<(String, bool)> {
    let mut searchable = String::new();
    let mut truncated = source_was_truncated;
    append_searchable_json_text(
        content,
        &mut searchable,
        MAX_PROJECTED_TEXT_BYTES,
        &mut truncated,
    );
    if !searchable.to_lowercase().contains(normalized_query_lower) {
        return None;
    }
    Some((searchable, truncated))
}

fn append_searchable_json_text(
    value: &Value,
    output: &mut String,
    maximum_bytes: usize,
    truncated: &mut bool,
) {
    if output.len() >= maximum_bytes {
        *truncated = true;
        return;
    }
    match value {
        Value::String(text) => {
            if text.is_empty() {
                return;
            }
            if !output.is_empty() && output.len() < maximum_bytes {
                output.push(' ');
            }
            let available = maximum_bytes.saturating_sub(output.len());
            if text.len() <= available {
                output.push_str(text);
            } else {
                let mut boundary = available;
                while boundary > 0 && !text.is_char_boundary(boundary) {
                    boundary -= 1;
                }
                output.push_str(&text[..boundary]);
                *truncated = true;
            }
        }
        Value::Array(values) => {
            for value in values {
                append_searchable_json_text(value, output, maximum_bytes, truncated);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                append_searchable_json_text(value, output, maximum_bytes, truncated);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn message_identity_to_list_cursor(
    source: &LiveQuerySource<'_>,
    conversation: &str,
    identity: &str,
) -> Result<(String, i64), LiveQueryError> {
    let conversation_digest = digest_text("greenbubbles-conversation-v1", conversation);
    let key = validate_message_identity(
        source,
        &conversation_digest,
        decode_message_cursor(identity)?,
    )?;
    let source_row_id = key.row_id;
    let cursor = encode_message_cursor(source, &conversation_digest, "messages.list", &key)?;
    Ok((cursor, source_row_id))
}

fn encode_fallback_search_cursor(
    source: &LiveQuerySource<'_>,
    query_digest: &str,
    conversation_digest: Option<&str>,
    conversation_id: &str,
    inner_message_cursor: Option<&str>,
) -> Result<String, LiveQueryError> {
    encode_cursor(&FallbackSearchCursor {
        version: CURSOR_FORMAT_VERSION,
        kind: FALLBACK_SEARCH_CURSOR_KIND.into(),
        source_identity: source.identity.clone(),
        query_digest: query_digest.into(),
        conversation_digest: conversation_digest.map(str::to_owned),
        conversation_id: conversation_id.into(),
        inner_message_cursor: inner_message_cursor.map(str::to_owned),
    })
}

fn decode_fallback_search_cursor(value: &str) -> Result<FallbackSearchCursor, LiveQueryError> {
    decode_cursor(value)
}

fn validate_fallback_search_cursor(
    source: &LiveQuerySource<'_>,
    query_digest: &str,
    conversation: Option<&str>,
    conversation_digest: Option<&str>,
    cursor: FallbackSearchCursor,
) -> Result<FallbackSearchCursor, LiveQueryError> {
    if cursor.version != CURSOR_FORMAT_VERSION
        || cursor.kind != FALLBACK_SEARCH_CURSOR_KIND
        || cursor.source_identity != source.identity
        || cursor.query_digest != query_digest
        || cursor.conversation_digest.as_deref() != conversation_digest
        || cursor.conversation_id.is_empty()
        || cursor.conversation_id.len() > MAX_CONVERSATION_ID_BYTES
        || conversation.is_some_and(|expected| cursor.conversation_id != expected)
        || cursor
            .inner_message_cursor
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > MAX_CURSOR_BYTES)
    {
        return Err(LiveQueryError::InvalidCursor(
            "fallback search cursor does not belong to this query, source, and conversation".into(),
        ));
    }
    Ok(cursor)
}

fn decode_cursor_kind(value: &str) -> Result<String, LiveQueryError> {
    Ok(decode_cursor::<CursorKind>(value)?.kind)
}

pub fn serialize_query_response<T: Serialize>(value: &T) -> Result<String, LiveQueryError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| LiveQueryError::Database("JSON response serialization failed".into()))?;
    if bytes.len() > MAX_SERIALIZED_RESPONSE_BYTES {
        return Err(LiveQueryError::ResponseTooLarge {
            maximum_bytes: MAX_SERIALIZED_RESPONSE_BYTES,
        });
    }
    String::from_utf8(bytes)
        .map_err(|_| LiveQueryError::Database("JSON response was not valid UTF-8".into()))
}

pub fn serialize_query_error(
    operation: &'static str,
    code: &'static str,
    message: &'static str,
    retryable: bool,
) -> String {
    serde_json::to_string_pretty(&QueryErrorEnvelope {
        schema: QUERY_SCHEMA,
        format_version: QUERY_FORMAT_VERSION,
        operation,
        ok: false,
        error: QueryErrorBody {
            code,
            message,
            retryable,
        },
    })
    .expect("query error envelopes contain only static serializable fields")
}

fn native_search_tables(connection: &Connection) -> Result<Vec<NativeSearchTable>, LiveQueryError> {
    let mut statement = connection
        .prepare(
            "SELECT name FROM sqlite_schema \
             WHERE type = 'table' AND name LIKE 'message_fts_v4_%' \
             ORDER BY name ASC",
        )
        .map_err(|_| {
            LiveQueryError::SearchUnavailable("the native FTS schema could not be inspected".into())
        })?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|_| {
            LiveQueryError::SearchUnavailable(
                "the native FTS schema could not be enumerated".into(),
            )
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| {
            LiveQueryError::SearchUnavailable(
                "the native FTS schema contains invalid table names".into(),
            )
        })?;
    drop(statement);

    let required = [
        "acontent",
        "message_local_id",
        "sort_seq",
        "local_type",
        "session_id",
        "sender_id",
        "create_time",
    ];
    let mut tables = Vec::new();
    let mut ordinals = BTreeSet::new();
    for name in names {
        let Some(suffix) = name.strip_prefix("message_fts_v4_") else {
            continue;
        };
        if suffix.is_empty() || !suffix.bytes().all(|value| value.is_ascii_digit()) {
            continue;
        }
        let ordinal = suffix.parse::<u32>().map_err(|_| {
            LiveQueryError::SearchUnavailable("native FTS table ordinal is invalid".into())
        })?;
        if !ordinals.insert(ordinal) {
            return Err(LiveQueryError::SearchUnavailable(
                "native FTS schema contains duplicate table ordinals".into(),
            ));
        }
        let columns = table_columns(connection, &name)?;
        if required.iter().all(|column| columns.contains(*column)) {
            tables.push(NativeSearchTable { name, ordinal });
        }
    }
    tables.sort_by_key(|table| table.ordinal);
    Ok(tables)
}

fn literal_native_fts_query(query: &str) -> String {
    format!("\"{}\"", query.replace('"', "\"\""))
}

fn validate_limit(limit: usize) -> Result<(), LiveQueryError> {
    if !(1..=MAX_PAGE_LIMIT).contains(&limit) {
        return Err(LiveQueryError::InvalidArgument(format!(
            "--limit must be between 1 and {MAX_PAGE_LIMIT}"
        )));
    }
    Ok(())
}

fn validate_conversation_id(conversation: &str) -> Result<(), LiveQueryError> {
    if conversation.is_empty()
        || conversation.len() > MAX_CONVERSATION_ID_BYTES
        || conversation.contains('\0')
    {
        return Err(LiveQueryError::InvalidArgument(
            "conversation identifier is empty or outside safe limits".into(),
        ));
    }
    Ok(())
}

fn resolve_contact_display_names<'b>(
    source: &LiveQuerySource<'_>,
    identifiers: impl IntoIterator<Item = &'b str>,
) -> Result<ContactDisplayNameEnrichment, LiveQueryError> {
    let mut requested = BTreeSet::new();
    for identifier in identifiers {
        if identifier.is_empty() {
            continue;
        }
        requested.insert(identifier.to_string());
        if requested.len() > MAX_PAGE_LIMIT {
            return Err(LiveQueryError::InvalidArgument(format!(
                "contact enrichment exceeds the {MAX_PAGE_LIMIT}-identifier safety limit"
            )));
        }
    }
    if requested.is_empty() {
        return Ok(ContactDisplayNameEnrichment {
            display_names: BTreeMap::new(),
            database_read: false,
            coverage_complete: true,
            warnings: Vec::new(),
        });
    }

    let requested_count = requested.len();
    let mut query_identifiers = requested
        .iter()
        .filter(|identifier| {
            identifier.len() <= MAX_CONVERSATION_ID_BYTES && !identifier.contains('\0')
        })
        .cloned()
        .collect::<Vec<_>>();
    let excluded_count = requested_count.saturating_sub(query_identifiers.len());
    if query_identifiers.is_empty() {
        return Ok(ContactDisplayNameEnrichment {
            display_names: BTreeMap::new(),
            database_read: false,
            coverage_complete: false,
            warnings: vec![QueryWarning {
                code: "contactDisplayNameUnresolved",
                message: "contact display names could not be resolved; bounded raw identifiers were retained"
                    .into(),
                shard_id: None,
                count: Some(requested_count),
            }],
        });
    }
    query_identifiers.sort();

    let connection = match source.open_database(Path::new("contact/contact.db")) {
        Ok(connection) => connection,
        Err(_) => {
            return Ok(unavailable_contact_enrichment(requested_count, false));
        }
    };
    let columns = match table_columns(&connection, "contact") {
        Ok(columns) => columns,
        Err(_) => return Ok(unavailable_contact_enrichment(requested_count, true)),
    };
    let identifier_column = ["username", "user_name"]
        .into_iter()
        .find(|column| columns.contains(*column));
    let remark_column = ["remark", "remark_name"]
        .into_iter()
        .find(|column| columns.contains(*column));
    let nickname_column = ["nick_name", "nickname"]
        .into_iter()
        .find(|column| columns.contains(*column));
    let alias_column = ["alias"]
        .into_iter()
        .find(|column| columns.contains(*column));
    let Some(identifier_column) = identifier_column else {
        return Ok(unavailable_contact_enrichment(requested_count, true));
    };
    if remark_column.is_none() && nickname_column.is_none() && alias_column.is_none() {
        return Ok(unavailable_contact_enrichment(requested_count, true));
    }

    let selected_column = |column: Option<&str>| {
        column
            .map(|column| format!("[{column}]"))
            .unwrap_or_else(|| "NULL".to_string())
    };
    let placeholders = (1..=query_identifiers.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT [{identifier_column}], {}, {}, {} FROM [contact] \
         WHERE [{identifier_column}] IN ({placeholders})",
        selected_column(remark_column),
        selected_column(nickname_column),
        selected_column(alias_column),
    );
    let mut statement = match connection.prepare(&sql) {
        Ok(statement) => statement,
        Err(_) => return Ok(unavailable_contact_enrichment(requested_count, true)),
    };
    let mut rows = match statement.query(rusqlite::params_from_iter(query_identifiers.iter())) {
        Ok(rows) => rows,
        Err(_) => return Ok(unavailable_contact_enrichment(requested_count, true)),
    };

    let mut display_names = BTreeMap::new();
    let mut decode_failure_count = 0usize;
    let mut truncated_count = 0usize;
    let mut row_read_failed = false;
    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(_) => {
                row_read_failed = true;
                break;
            }
        };
        let identifier = match row
            .get_ref(0)
            .ok()
            .and_then(|value| decode_sqlite_text(value).ok())
        {
            Some(identifier) if requested.contains(&identifier) => identifier,
            _ => {
                decode_failure_count = decode_failure_count.saturating_add(1);
                continue;
            }
        };
        let mut display_name = None;
        for index in 1..=3 {
            match row
                .get_ref(index)
                .ok()
                .and_then(|value| decode_sqlite_text(value).ok())
            {
                Some(value) if !value.trim().is_empty() => {
                    let (value, truncated) = truncate_utf8(value, MAX_PROJECTED_TEXT_BYTES);
                    truncated_count = truncated_count.saturating_add(usize::from(truncated));
                    display_name = Some(value);
                    break;
                }
                Some(_) => {}
                None => {
                    decode_failure_count = decode_failure_count.saturating_add(1);
                }
            }
        }
        if let Some(display_name) = display_name {
            display_names.entry(identifier).or_insert(display_name);
        }
    }
    drop(rows);
    drop(statement);
    drop(connection);

    let unresolved_count = requested_count.saturating_sub(display_names.len());
    let mut warnings = Vec::new();
    if row_read_failed {
        warnings.push(QueryWarning {
            code: "contactEnrichmentUnavailable",
            message: "contact display-name enrichment became unreadable; resolved names were retained and other raw identifiers were preserved".into(),
            shard_id: None,
            count: Some(unresolved_count.max(1)),
        });
    }
    if decode_failure_count > 0 {
        warnings.push(QueryWarning {
            code: "contactDisplayNameDecodeFailed",
            message: "one or more contact display-name fields could not be decoded; raw identifiers were retained where necessary".into(),
            shard_id: None,
            count: Some(decode_failure_count),
        });
    }
    if truncated_count > 0 {
        warnings.push(QueryWarning {
            code: "contactDisplayNameTruncated",
            message: "one or more contact display names exceeded the fixed presentation bound and were truncated on a UTF-8 boundary".into(),
            shard_id: None,
            count: Some(truncated_count),
        });
    }
    if unresolved_count > 0 || excluded_count > 0 {
        warnings.push(QueryWarning {
            code: "contactDisplayNameUnresolved",
            message:
                "one or more contact display names were unavailable; raw identifiers were retained"
                    .into(),
            shard_id: None,
            count: Some(unresolved_count.max(excluded_count)),
        });
    }
    Ok(ContactDisplayNameEnrichment {
        display_names,
        database_read: true,
        coverage_complete: !row_read_failed
            && decode_failure_count == 0
            && unresolved_count == 0
            && excluded_count == 0,
        warnings,
    })
}

fn unavailable_contact_enrichment(
    requested_count: usize,
    database_read: bool,
) -> ContactDisplayNameEnrichment {
    ContactDisplayNameEnrichment {
        display_names: BTreeMap::new(),
        database_read,
        coverage_complete: false,
        warnings: vec![QueryWarning {
            code: "contactEnrichmentUnavailable",
            message: "contact display-name enrichment is unavailable for this source schema; raw identifiers were retained".into(),
            shard_id: None,
            count: Some(requested_count),
        }],
    }
}

fn enrich_conversation_items(
    source: &LiveQuerySource<'_>,
    items: &mut [ConversationItem],
) -> Result<ContactDisplayNameEnrichment, LiveQueryError> {
    let enrichment =
        resolve_contact_display_names(source, items.iter().map(|item| item.id.as_str()))?;
    for item in items {
        item.display_name = enrichment.display_names.get(&item.id).cloned();
    }
    Ok(enrichment)
}

fn enrich_message_items(
    source: &LiveQuerySource<'_>,
    items: &mut [MessageItem],
) -> Result<ContactDisplayNameEnrichment, LiveQueryError> {
    let enrichment =
        resolve_contact_display_names(source, items.iter().map(|item| item.sender.as_str()))?;
    for item in items {
        item.sender_display_name = enrichment.display_names.get(&item.sender).cloned();
    }
    Ok(enrichment)
}

fn enrich_search_items(
    source: &LiveQuerySource<'_>,
    items: &mut [SearchItem],
) -> Result<ContactDisplayNameEnrichment, LiveQueryError> {
    let enrichment =
        resolve_contact_display_names(source, items.iter().map(|item| item.sender.as_str()))?;
    for item in items {
        item.sender_display_name = enrichment.display_names.get(&item.sender).cloned();
    }
    Ok(enrichment)
}

fn table_columns(
    connection: &Connection,
    table_name: &str,
) -> Result<BTreeSet<String>, LiveQueryError> {
    if table_name.contains(']') {
        return Err(LiveQueryError::Database(
            "database table identifier is invalid".into(),
        ));
    }
    let sql = format!("PRAGMA table_info([{table_name}])");
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| database_error(&error.to_string()))?;
    let values = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| database_error(&error.to_string()))?;
    let mut columns = BTreeSet::new();
    for value in values {
        columns.insert(value.map_err(|error| database_error(&error.to_string()))?);
    }
    Ok(columns)
}

fn optional_column(columns: &BTreeSet<String>, name: &'static str) -> &'static str {
    if columns.contains(name) {
        name
    } else {
        "NULL"
    }
}

fn optional_message_column(columns: &BTreeSet<String>, name: &'static str) -> String {
    if columns.contains(name) {
        format!("m.[{name}]")
    } else {
        "NULL".into()
    }
}

fn decode_sqlite_text(value: ValueRef<'_>) -> Result<String, ()> {
    match value {
        ValueRef::Null => Ok(String::new()),
        ValueRef::Text(bytes) => Ok(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Blob(bytes) => {
            const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
            if bytes.len() >= 4 && bytes[..4] == ZSTD_MAGIC {
                let decoder =
                    zstd::stream::read::Decoder::new(Cursor::new(bytes)).map_err(|_| ())?;
                let mut decoded = Vec::with_capacity(MAX_PROJECTED_TEXT_BYTES.min(bytes.len()));
                decoder
                    .take((MAX_PROJECTED_TEXT_BYTES + 1) as u64)
                    .read_to_end(&mut decoded)
                    .map_err(|_| ())?;
                Ok(String::from_utf8_lossy(&decoded).into_owned())
            } else {
                Ok(String::from_utf8_lossy(bytes).into_owned())
            }
        }
        ValueRef::Integer(value) => Ok(value.to_string()),
        ValueRef::Real(value) => Ok(value.to_string()),
    }
}

fn sqlite_value_bytes(value: ValueRef<'_>) -> Vec<u8> {
    match value {
        ValueRef::Null => Vec::new(),
        ValueRef::Text(value) | ValueRef::Blob(value) => value.to_vec(),
        ValueRef::Integer(value) => value.to_string().into_bytes(),
        ValueRef::Real(value) => value.to_string().into_bytes(),
    }
}

fn sqlite_optional_bytes(value: ValueRef<'_>) -> Option<Vec<u8>> {
    match value {
        ValueRef::Null => None,
        value => Some(sqlite_value_bytes(value)),
    }
}

fn strip_group_summary_prefix(username: &str, summary: String) -> String {
    if !wx_db::is_group_chat(username) {
        return summary;
    }
    if let Some(newline) = summary.find('\n') {
        let prefix = &summary[..newline];
        if prefix.ends_with(':') && !prefix.contains(' ') {
            return summary[newline + 1..].to_string();
        }
    }
    summary
}

fn bounded_optional_string(value: Option<String>) -> Option<String> {
    value.map(|value| truncate_utf8(value, MAX_PROJECTED_TEXT_BYTES).0)
}

fn truncate_utf8(mut value: String, maximum_bytes: usize) -> (String, bool) {
    if value.len() <= maximum_bytes {
        return (value, false);
    }
    let mut boundary = maximum_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    (value, true)
}

fn truncate_json_strings(value: &mut Value, maximum_bytes: usize, count: &mut usize) {
    match value {
        Value::String(text) => {
            if text.len() > maximum_bytes {
                let replacement = truncate_utf8(std::mem::take(text), maximum_bytes).0;
                *text = replacement;
                *count += 1;
            }
        }
        Value::Array(values) => {
            for value in values {
                truncate_json_strings(value, maximum_bytes, count);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                truncate_json_strings(value, maximum_bytes, count);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn source_description(source: &LiveQuerySource<'_>) -> QuerySourceDescription {
    QuerySourceDescription {
        mode: source.mode(),
        identity: source.identity.clone(),
    }
}

fn encode_conversation_cursor(
    source: &LiveQuerySource<'_>,
    row: &ConversationRow,
) -> Result<String, LiveQueryError> {
    encode_cursor(&ConversationCursor {
        version: CURSOR_FORMAT_VERSION,
        kind: "conversations.list".into(),
        source_identity: source.identity.clone(),
        sort_timestamp: row.sort_timestamp,
        username: row.username.clone(),
    })
}

fn decode_conversation_cursor(value: &str) -> Result<ConversationCursor, LiveQueryError> {
    decode_cursor(value)
}

fn validate_conversation_cursor(
    source: &LiveQuerySource<'_>,
    cursor: ConversationCursor,
) -> Result<ConversationCursor, LiveQueryError> {
    if cursor.version != CURSOR_FORMAT_VERSION
        || cursor.kind != "conversations.list"
        || cursor.source_identity != source.identity
        || cursor.username.is_empty()
        || cursor.username.len() > MAX_CONVERSATION_ID_BYTES
    {
        return Err(LiveQueryError::InvalidCursor(
            "cursor does not belong to this operation and source".into(),
        ));
    }
    Ok(cursor)
}

fn encode_message_cursor(
    source: &LiveQuerySource<'_>,
    conversation_digest: &str,
    kind: &str,
    key: &MessageKey,
) -> Result<String, LiveQueryError> {
    encode_cursor(&MessageCursor {
        version: CURSOR_FORMAT_VERSION,
        kind: kind.into(),
        source_identity: source.identity.clone(),
        conversation_digest: conversation_digest.into(),
        sort_sequence: key.sort_sequence,
        create_time: key.create_time,
        server_id: key.server_id,
        shard_id: key.shard_id,
        row_id: key.row_id,
    })
}

fn decode_message_cursor(value: &str) -> Result<MessageCursor, LiveQueryError> {
    decode_cursor(value)
}

fn validate_message_cursor(
    source: &LiveQuerySource<'_>,
    conversation_digest: &str,
    cursor: MessageCursor,
) -> Result<MessageCursor, LiveQueryError> {
    if cursor.version != CURSOR_FORMAT_VERSION
        || cursor.kind != "messages.list"
        || cursor.source_identity != source.identity
        || cursor.conversation_digest != conversation_digest
    {
        return Err(LiveQueryError::InvalidCursor(
            "cursor does not belong to this operation, source, and conversation".into(),
        ));
    }
    Ok(cursor)
}

fn validate_message_identity(
    source: &LiveQuerySource<'_>,
    conversation_digest: &str,
    identity: MessageCursor,
) -> Result<MessageKey, LiveQueryError> {
    if identity.version != CURSOR_FORMAT_VERSION
        || identity.kind != "message.identity"
        || identity.source_identity != source.identity
        || identity.conversation_digest != conversation_digest
    {
        return Err(LiveQueryError::InvalidCursor(
            "message identity does not belong to this source and conversation".into(),
        ));
    }
    Ok(MessageKey {
        sort_sequence: identity.sort_sequence,
        create_time: identity.create_time,
        server_id: identity.server_id,
        shard_id: identity.shard_id,
        row_id: identity.row_id,
    })
}

fn encode_search_cursor(
    source: &LiveQuerySource<'_>,
    query_digest: &str,
    conversation_digest: Option<&str>,
    kind: &str,
    key: &SearchKey,
) -> Result<String, LiveQueryError> {
    encode_cursor(&SearchCursor {
        version: CURSOR_FORMAT_VERSION,
        kind: kind.into(),
        source_identity: source.identity.clone(),
        query_digest: query_digest.into(),
        conversation_digest: conversation_digest.map(str::to_owned),
        create_time: key.create_time,
        sort_sequence: key.sort_sequence,
        message_local_id: key.message_local_id,
        table_ordinal: key.table_ordinal,
        row_id: key.row_id,
    })
}

fn decode_search_cursor(value: &str) -> Result<SearchCursor, LiveQueryError> {
    decode_cursor(value)
}

fn validate_search_cursor(
    source: &LiveQuerySource<'_>,
    query_digest: &str,
    conversation_digest: Option<&str>,
    cursor: SearchCursor,
) -> Result<SearchCursor, LiveQueryError> {
    if cursor.version != CURSOR_FORMAT_VERSION
        || cursor.kind != "messages.search"
        || cursor.source_identity != source.identity
        || cursor.query_digest != query_digest
        || cursor.conversation_digest.as_deref() != conversation_digest
    {
        return Err(LiveQueryError::InvalidCursor(
            "cursor does not belong to this search, source, and conversation filter".into(),
        ));
    }
    Ok(cursor)
}

fn encode_cursor<T: Serialize>(value: &T) -> Result<String, LiveQueryError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| LiveQueryError::InvalidCursor("cursor could not be encoded".into()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T, LiveQueryError> {
    if value.is_empty() || value.len() > MAX_CURSOR_BYTES {
        return Err(LiveQueryError::InvalidCursor(
            "cursor is empty or outside safe limits".into(),
        ));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| LiveQueryError::InvalidCursor("cursor is not valid base64url".into()))?;
    if bytes.len() > MAX_CURSOR_BYTES {
        return Err(LiveQueryError::InvalidCursor(
            "decoded cursor is outside safe limits".into(),
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| LiveQueryError::InvalidCursor("cursor structure is invalid".into()))
}

fn digest_text(domain: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

fn coalesce_warnings(warnings: &mut Vec<QueryWarning>) {
    let mut coalesced: Vec<QueryWarning> = Vec::new();
    for warning in warnings.drain(..) {
        if let Some(existing) = coalesced
            .iter_mut()
            .find(|existing| existing.code == warning.code && existing.shard_id == warning.shard_id)
        {
            existing.count = Some(
                existing
                    .count
                    .unwrap_or(1)
                    .saturating_add(warning.count.unwrap_or(1)),
            );
        } else {
            coalesced.push(warning);
        }
    }
    *warnings = coalesced;
}

fn now_unix_milliseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn database_error(message: &str) -> LiveQueryError {
    let lower = message.to_ascii_lowercase();
    let safe = if lower.contains("key") || lower.contains("encrypt") {
        "database could not be opened with the supplied access material"
    } else if lower.contains("locked") || lower.contains("busy") {
        "database was busy beyond the query timeout"
    } else {
        "SQLite rejected the bounded read-only operation"
    };
    LiveQueryError::Database(safe.into())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};

    use rusqlite::Connection;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn explicit_source_sender_wins_over_a_malformed_group_prefix() {
        let malformed =
            "<msg><appmsg><refermsg><chatusr>wxid_quoted</chatusr></refermsg></appmsg>:\nbody";
        assert_eq!(
            resolved_message_sender("wxid_sender", malformed),
            "wxid_sender"
        );
        assert_eq!(
            resolved_message_sender("", "wxid_fallback"),
            "wxid_fallback"
        );
        assert_eq!(resolved_message_sender("", malformed), "");

        let (_temp, root) = fixture();
        let source = LiveQuerySource::open(&root, QueryDatabaseAccess::Decrypted).unwrap();
        let raw = RawMessage {
            key: MessageKey {
                sort_sequence: 1,
                create_time: 1,
                server_id: 1,
                shard_id: 0,
                row_id: 1,
            },
            local_type: 1,
            sender: "wxid_sender".into(),
            raw_content: malformed.as_bytes().to_vec(),
            packed_info: None,
            status: 0,
            compression_type: None,
            compressed_content: None,
        };
        let (projected, decode_failed) =
            project_message(&source, "123456@chatroom", "test-digest", raw).unwrap();
        assert!(!decode_failed);
        assert_eq!(projected.sender, "wxid_sender");
    }

    fn fixture() -> (TempDir, PathBuf) {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("db_storage");
        fs::create_dir_all(root.join("contact")).unwrap();
        fs::create_dir_all(root.join("session")).unwrap();
        fs::create_dir_all(root.join("message")).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();

        let contact = Connection::open(root.join("contact/contact.db")).unwrap();
        contact
            .execute_batch(
                "CREATE TABLE contact(
                    username TEXT PRIMARY KEY,
                    alias BLOB,
                    remark BLOB,
                    nick_name BLOB
                 );
                 INSERT INTO contact VALUES
                    ('wxid_a', 'Alias A', 'Remark A', 'Nickname A'),
                    ('wxid_b', 'Alias B', '', 'Nickname B'),
                    ('wxid_c', 'Alias C', '', ''),
                    ('wxid_d', '', '', ''),
                    ('wxid_talker', '', 'Talker Remark', ''),
                    ('wxid_sender', '', 'Sender Remark', '');",
            )
            .unwrap();

        let session = Connection::open(root.join("session/session.db")).unwrap();
        session
            .execute_batch(
                "CREATE TABLE SessionTable(
                    username TEXT NOT NULL,
                    sort_timestamp INTEGER NOT NULL,
                    summary BLOB,
                    last_msg_type INTEGER,
                    last_msg_sender TEXT,
                    last_sender_display_name TEXT
                 );
                 INSERT INTO SessionTable VALUES
                    ('wxid_a', 30, 'a', 1, 'wxid_a', 'A'),
                    ('wxid_b', 20, 'b', 1, 'wxid_b', 'B'),
                    ('wxid_c', 20, 'c', 1, 'wxid_c', 'C'),
                    ('wxid_d', 10, 'd', 1, 'wxid_d', 'D');",
            )
            .unwrap();
        drop(session);

        let talker = "wxid_talker";
        let table = format!("Msg_{:x}", md5::compute(talker.as_bytes()));
        for (shard, values) in [
            (0, vec![(100, 1000, 0, "s0-new"), (90, 900, 7, "s0-old")]),
            (1, vec![(100, 1000, 0, "s1-new"), (80, 800, 8, "s1-old")]),
        ] {
            let connection =
                Connection::open(root.join(format!("message/message_{shard}.db"))).unwrap();
            connection
                .execute_batch(&format!(
                    "CREATE TABLE Name2Id(user_name TEXT);
                     INSERT INTO Name2Id(rowid, user_name) VALUES (1, 'wxid_sender');
                     CREATE TABLE [{table}](
                        server_id INTEGER,
                        sort_seq INTEGER,
                        local_type INTEGER,
                        real_sender_id INTEGER,
                        create_time INTEGER,
                        status INTEGER,
                        message_content BLOB,
                        packed_info_data BLOB,
                        WCDB_CT_message_content INTEGER,
                        compress_content BLOB
                     );"
                ))
                .unwrap();
            for (sort_sequence, create_time, server_id, body) in values {
                connection
                    .execute(
                        &format!(
                            "INSERT INTO [{table}](server_id, sort_seq, local_type, real_sender_id, \
                             create_time, status, message_content, WCDB_CT_message_content) \
                             VALUES (?1, ?2, 1, 1, ?3, 0, ?4, 0)"
                        ),
                        params![server_id, sort_sequence, create_time, body.as_bytes()],
                    )
                .unwrap();
            }
        }
        let fts = Connection::open(root.join("message/message_fts.db")).unwrap();
        fts.execute_batch(
            "CREATE TABLE name2id(rowid INTEGER PRIMARY KEY, username TEXT NOT NULL);
             INSERT INTO name2id VALUES (1, 'wxid_talker');
             INSERT INTO name2id VALUES (2, 'wxid_sender');
             CREATE VIRTUAL TABLE message_fts_v4_0 USING fts5(
                acontent, message_local_id UNINDEXED, sort_seq UNINDEXED,
                local_type UNINDEXED, session_id UNINDEXED, sender_id UNINDEXED,
                create_time UNINDEXED, tokenize='unicode61'
             );
             INSERT INTO message_fts_v4_0 VALUES
                ('searchable old message', 2, 80, 1, 1, 2, 800);",
        )
        .unwrap();
        (temp, root)
    }

    #[test]
    fn conversation_cursor_pages_duplicate_timestamps_once() {
        let (_temp, root) = fixture();
        let source = LiveQuerySource::open(&root, QueryDatabaseAccess::Decrypted).unwrap();
        let first = list_conversations(&source, 2, None).unwrap();
        assert_eq!(
            first
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["wxid_a", "wxid_b"]
        );
        assert!(first.page.has_more);
        let second = list_conversations(&source, 2, first.page.next_cursor.as_deref()).unwrap();
        assert_eq!(
            second
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["wxid_c", "wxid_d"]
        );
        assert!(!second.page.has_more);
    }

    #[test]
    fn contact_display_names_use_bounded_batch_precedence_across_query_shapes() {
        let (_temp, root) = fixture();
        let source = LiveQuerySource::open(&root, QueryDatabaseAccess::Decrypted).unwrap();

        let conversations = list_conversations(&source, 4, None).unwrap();
        let names = conversations
            .items
            .iter()
            .map(|item| (item.id.as_str(), item.display_name.as_deref()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(names["wxid_a"], Some("Remark A"));
        assert_eq!(names["wxid_b"], Some("Nickname B"));
        assert_eq!(names["wxid_c"], Some("Alias C"));
        assert_eq!(names["wxid_d"], None);
        assert_eq!(conversations.consistency.database_count, 2);
        assert!(!conversations.consistency.cross_database_atomic);
        assert!(!conversations.consistency.coverage_complete);
        assert!(conversations
            .warnings
            .iter()
            .any(|warning| warning.code == "contactDisplayNameUnresolved"));

        let exact_conversation = find_conversation(&source, "wxid_a").unwrap().unwrap();
        assert_eq!(exact_conversation.display_name.as_deref(), Some("Remark A"));
        let batch = find_conversations(&source, &["wxid_b".into(), "wxid_c".into()]).unwrap();
        assert_eq!(batch["wxid_b"].display_name.as_deref(), Some("Nickname B"));
        assert_eq!(batch["wxid_c"].display_name.as_deref(), Some("Alias C"));

        let messages = list_messages(&source, "wxid_talker", 2, None).unwrap();
        assert!(messages
            .items
            .iter()
            .all(|item| item.sender_display_name.as_deref() == Some("Sender Remark")));
        assert_eq!(messages.consistency.database_count, 3);
        assert!(!messages.consistency.cross_database_atomic);

        let search = search_messages(&source, "searchable", Some("wxid_talker"), 10, None).unwrap();
        assert_eq!(
            search.items[0].sender_display_name.as_deref(),
            Some("Sender Remark")
        );
        assert_eq!(search.consistency.database_count, 2);
        assert!(!search.consistency.cross_database_atomic);
        let exact = get_search_result_message(&source, "wxid_talker", &search.items[0].id).unwrap();
        assert_eq!(
            exact.item.sender_display_name.as_deref(),
            Some("Sender Remark")
        );
    }

    #[test]
    fn contact_schema_variants_and_blob_names_are_decoded_safely() {
        let (_temp, root) = fixture();
        let contact = Connection::open(root.join("contact/contact.db")).unwrap();
        contact.execute_batch("DROP TABLE contact;").unwrap();
        contact
            .execute_batch(
                "CREATE TABLE contact(
                    user_name TEXT PRIMARY KEY,
                    alias BLOB,
                    remark_name BLOB,
                    nickname BLOB
                 );
                 INSERT INTO contact VALUES
                    ('wxid_a', X'416C6961732041', X'52656D61726B2041', X'4E69636B2041'),
                    ('wxid_b', X'416C6961732042', X'', X'4E69636B2042'),
                    ('wxid_c', X'416C6961732043', X'', X'');",
            )
            .unwrap();
        drop(contact);

        let source = LiveQuerySource::open(&root, QueryDatabaseAccess::Decrypted).unwrap();
        let conversations = list_conversations(&source, 3, None).unwrap();
        assert_eq!(
            conversations.items[0].display_name.as_deref(),
            Some("Remark A")
        );
        assert_eq!(
            conversations.items[1].display_name.as_deref(),
            Some("Nick B")
        );
        assert_eq!(
            conversations.items[2].display_name.as_deref(),
            Some("Alias C")
        );
        assert!(conversations.consistency.coverage_complete);
    }

    #[test]
    fn incompatible_contact_enrichment_warns_without_blocking_raw_messages() {
        let (_temp, root) = fixture();
        let contact = Connection::open(root.join("contact/contact.db")).unwrap();
        contact.execute_batch("DROP TABLE contact;").unwrap();
        contact
            .execute_batch("CREATE TABLE contact(username TEXT PRIMARY KEY);")
            .unwrap();
        drop(contact);

        let source = LiveQuerySource::open(&root, QueryDatabaseAccess::Decrypted).unwrap();
        let page = list_messages(&source, "wxid_talker", 1, None).unwrap();
        assert_eq!(page.items[0].sender, "wxid_sender");
        assert_eq!(page.items[0].sender_display_name, None);
        assert_eq!(page.consistency.database_count, 3);
        assert!(!page.consistency.cross_database_atomic);
        assert!(!page.consistency.coverage_complete);
        assert!(page
            .warnings
            .iter()
            .any(|warning| warning.code == "contactEnrichmentUnavailable"));
    }

    #[test]
    fn contact_enrichment_rejects_more_than_five_hundred_unique_ids() {
        let (_temp, root) = fixture();
        let source = LiveQuerySource::open(&root, QueryDatabaseAccess::Decrypted).unwrap();
        let identifiers = (0..=MAX_PAGE_LIMIT)
            .map(|index| format!("wxid_{index}"))
            .collect::<Vec<_>>();
        let result = resolve_contact_display_names(&source, identifiers.iter().map(String::as_str));
        assert!(matches!(result, Err(LiveQueryError::InvalidArgument(_))));
    }

    #[test]
    fn message_cursor_is_total_across_shards_and_duplicate_server_ids() {
        let (_temp, root) = fixture();
        let source = LiveQuerySource::open(&root, QueryDatabaseAccess::Decrypted).unwrap();
        let mut cursor = None;
        let mut bodies = Vec::new();
        loop {
            let page = list_messages(&source, "wxid_talker", 1, cursor.as_deref()).unwrap();
            assert!(page.items.len() <= 1);
            for item in &page.items {
                bodies.push(item.content["Text"].as_str().unwrap().to_string());
            }
            if !page.page.has_more {
                break;
            }
            cursor = page.page.next_cursor;
        }
        assert_eq!(bodies, ["s1-new", "s0-new", "s0-old", "s1-old"]);
    }

    #[test]
    fn cursor_is_bound_to_conversation() {
        let (_temp, root) = fixture();
        let source = LiveQuerySource::open(&root, QueryDatabaseAccess::Decrypted).unwrap();
        let first = list_messages(&source, "wxid_talker", 1, None).unwrap();
        let error = list_messages(
            &source,
            "different_talker",
            1,
            first.page.next_cursor.as_deref(),
        )
        .unwrap_err();
        assert!(matches!(error, LiveQueryError::InvalidCursor(_)));
    }

    #[test]
    fn time_ranges_are_pushed_into_message_queries() {
        let (_temp, root) = fixture();
        let source = LiveQuerySource::open(&root, QueryDatabaseAccess::Decrypted).unwrap();
        let page =
            list_messages_in_time_range(&source, "wxid_talker", 10, None, Some(850), Some(950))
                .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].content["Text"], "s0-old");
    }

    #[test]
    fn native_search_identity_hydrates_one_exact_source_message() {
        let (_temp, root) = fixture();
        let source = LiveQuerySource::open(&root, QueryDatabaseAccess::Decrypted).unwrap();
        let page = search_messages(&source, "searchable", Some("wxid_talker"), 10, None).unwrap();
        assert_eq!(page.items.len(), 1);
        let resource =
            get_search_result_message(&source, "wxid_talker", &page.items[0].id).unwrap();
        assert_eq!(resource.item.id, page.items[0].id);
        assert_eq!(resource.item.content["Text"], "s1-old");
        assert!(matches!(
            get_search_result_message(&source, "wxid_other", &page.items[0].id),
            Err(LiveQueryError::InvalidCursor(_))
        ));
    }

    #[test]
    fn decoded_fallback_search_pages_and_hydrates_without_native_fts() {
        let (_temp, root) = fixture();
        fs::remove_file(root.join("message/message_fts.db")).unwrap();
        let source = LiveQuerySource::open(&root, QueryDatabaseAccess::Decrypted).unwrap();
        let first = search_messages(&source, "s0", Some("wxid_talker"), 1, None).unwrap();
        assert_eq!(first.items.len(), 1);
        assert_eq!(first.items[0].snippet, "s0-new");
        assert!(first.page.has_more);
        assert_eq!(first.consistency.guarantee, "boundedDecodedSourceWindow");
        assert!(first
            .warnings
            .iter()
            .any(|warning| warning.code == "fallbackSearchSourceWindowBounded"));
        let first_id = first.items[0].id.clone();
        let exact = get_message(&source, "wxid_talker", &first_id).unwrap();
        assert_eq!(exact.item.content["Text"], "s0-new");

        let second = search_messages(
            &source,
            "s0",
            Some("wxid_talker"),
            1,
            first.page.next_cursor.as_deref(),
        )
        .unwrap();
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].snippet, "s0-old");
        assert!(search_messages(
            &source,
            "different query",
            Some("wxid_talker"),
            1,
            first.page.next_cursor.as_deref(),
        )
        .is_err());
    }

    #[test]
    fn decoded_fallback_returns_an_empty_bounded_window_with_a_continuation() {
        let (_temp, root) = fixture();
        fs::remove_file(root.join("message/message_fts.db")).unwrap();
        let talker = "wxid_talker";
        let table = format!("Msg_{:x}", md5::compute(talker.as_bytes()));
        let connection = Connection::open(root.join("message/message_1.db")).unwrap();
        for index in 0..=MAX_FALLBACK_SEARCH_MESSAGES_PER_PAGE {
            let body = if index == MAX_FALLBACK_SEARCH_MESSAGES_PER_PAGE {
                "needle beyond first source window"
            } else {
                "ordinary body"
            };
            connection
                .execute(
                    &format!(
                        "INSERT INTO [{table}](server_id, sort_seq, local_type, real_sender_id, \
                         create_time, status, message_content, WCDB_CT_message_content) \
                         VALUES (?1, ?2, 1, 1, ?3, 0, ?4, 0)"
                    ),
                    params![
                        20_000 + index as i64,
                        20_000 - index as i64,
                        20_000 - index as i64,
                        body.as_bytes()
                    ],
                )
                .unwrap();
        }
        drop(connection);

        let source = LiveQuerySource::open(&root, QueryDatabaseAccess::Decrypted).unwrap();
        let first = search_messages(&source, "needle", Some(talker), 10, None).unwrap();
        assert!(first.items.is_empty());
        assert!(first.page.has_more);
        let second = search_messages(
            &source,
            "needle",
            Some(talker),
            10,
            first.page.next_cursor.as_deref(),
        )
        .unwrap();
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].snippet, "needle beyond first source window");
    }

    #[test]
    fn source_connections_reject_writes() {
        let (_temp, root) = fixture();
        let source = LiveQuerySource::open(&root, QueryDatabaseAccess::Decrypted).unwrap();
        let connection = source
            .open_database(Path::new("session/session.db"))
            .unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(connection
            .execute("CREATE TABLE forbidden(value INTEGER)", [])
            .is_err());
    }

    #[test]
    fn unsafe_roots_schema_drift_and_damaged_shards_are_explicit() {
        let (temp, root) = fixture();
        let linked_root = temp.path().join("linked-db-storage");
        symlink(&root, &linked_root).unwrap();
        assert!(matches!(
            LiveQuerySource::open(&linked_root, QueryDatabaseAccess::Decrypted),
            Err(LiveQueryError::UnsafeSource(_))
        ));

        fs::write(root.join("message/message_1.db"), b"not a SQLite database").unwrap();
        let source = LiveQuerySource::open(&root, QueryDatabaseAccess::Decrypted).unwrap();
        let partial = list_messages(&source, "wxid_talker", 10, None).unwrap();
        assert_eq!(
            partial
                .items
                .iter()
                .map(|item| item.content["Text"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["s0-new", "s0-old"]
        );
        assert!(!partial.consistency.coverage_complete);
        assert!(partial.warnings.iter().any(|warning| {
            matches!(warning.code, "shardUnavailable" | "shardSchemaUnavailable")
                && warning.shard_id == Some(1)
        }));

        let (_schema_temp, schema_root) = fixture();
        let session = Connection::open(schema_root.join("session/session.db")).unwrap();
        session.execute_batch("DROP TABLE SessionTable; CREATE TABLE SessionTable(username TEXT, sort_timestamp INTEGER);").unwrap();
        drop(session);
        let schema_source =
            LiveQuerySource::open(&schema_root, QueryDatabaseAccess::Decrypted).unwrap();
        assert!(matches!(
            list_conversations(&schema_source, 10, None),
            Err(LiveQueryError::Database(_))
        ));
    }

    #[test]
    fn projection_truncates_large_content_on_a_utf8_boundary() {
        let (_temp, root) = fixture();
        let talker = "wxid_talker";
        let table = format!("Msg_{:x}", md5::compute(talker.as_bytes()));
        let connection = Connection::open(root.join("message/message_1.db")).unwrap();
        let body = "🫧".repeat(MAX_PROJECTED_TEXT_BYTES);
        connection
            .execute(
                &format!(
                    "INSERT INTO [{table}](server_id, sort_seq, local_type, real_sender_id, \
                     create_time, status, message_content, WCDB_CT_message_content) \
                     VALUES (99, 200, 1, 1, 2000, 0, ?1, 0)"
                ),
                [body.as_bytes()],
            )
            .unwrap();
        drop(connection);

        let source = LiveQuerySource::open(&root, QueryDatabaseAccess::Decrypted).unwrap();
        let page = list_messages(&source, talker, 1, None).unwrap();
        assert!(page.items[0].content_truncated);
        let text = page.items[0].content["Text"].as_str().unwrap();
        assert!(text.len() <= MAX_PROJECTED_TEXT_BYTES);
        assert!(text.is_char_boundary(text.len()));
        assert!(serialize_query_response(&page).unwrap().len() <= MAX_SERIALIZED_RESPONSE_BYTES);
    }
}
