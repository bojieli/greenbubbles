use std::collections::BTreeSet;
use std::fs;
use std::io::{Cursor, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rusqlite::types::ValueRef;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const QUERY_SCHEMA: &str = "greenbubbles.query.v1";
pub const QUERY_FORMAT_VERSION: u32 = 1;
pub const DEFAULT_PAGE_LIMIT: usize = 100;
pub const MAX_PAGE_LIMIT: usize = 500;
pub const MAX_PROJECTED_TEXT_BYTES: usize = 16 * 1024;
pub const MAX_SERIALIZED_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

const CURSOR_FORMAT_VERSION: u32 = 1;
const MAX_CURSOR_BYTES: usize = 4096;
const MAX_CONVERSATION_ID_BYTES: usize = 4096;
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_millis(2_000);

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
    #[error("serialized response exceeds the {maximum_bytes}-byte safety limit")]
    ResponseTooLarge { maximum_bytes: usize },
}

#[derive(Debug, Clone, Copy)]
pub enum QueryDatabaseAccess<'a> {
    LiveEncrypted(&'a [u8; 32]),
    Decrypted,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum QuerySourceMode {
    LiveEncrypted,
    Decrypted,
}

#[derive(Debug)]
pub struct LiveQuerySource<'a> {
    root: PathBuf,
    identity: String,
    access: QueryDatabaseAccess<'a>,
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
        hasher.update(canonical.as_os_str().as_encoded_bytes());
        hasher.update(metadata.dev().to_le_bytes());
        hasher.update(metadata.ino().to_le_bytes());
        let digest = hasher.finalize();
        let identity = format!("sha256:{}", hex::encode(&digest[..16]));

        Ok(Self {
            root: canonical,
            identity,
            access,
        })
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub fn mode(&self) -> QuerySourceMode {
        match self.access {
            QueryDatabaseAccess::LiveEncrypted(_) => QuerySourceMode::LiveEncrypted,
            QueryDatabaseAccess::Decrypted => QuerySourceMode::Decrypted,
        }
    }

    fn open_database(&self, relative_path: &Path) -> Result<Connection, LiveQueryError> {
        let path = self.safe_database_path(relative_path)?;
        let key = match self.access {
            QueryDatabaseAccess::LiveEncrypted(key) => Some(key),
            QueryDatabaseAccess::Decrypted => None,
        };
        let connection = wx_db::open_readonly_connection(&path, key)
            .map_err(|error| database_error(&error.to_string()))?;
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
        Ok(path)
    }

    fn message_shards(&self) -> Result<Vec<MessageShard>, LiveQueryError> {
        let relative_directory = Path::new("message");
        let directory = self.root.join(relative_directory);
        let metadata = fs::symlink_metadata(&directory).map_err(|_| {
            LiveQueryError::UnsafeSource("message database directory is unavailable".into())
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(LiveQueryError::UnsafeSource(
                "message database directory is not a current-user-owned real directory".into(),
            ));
        }

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
}

#[derive(Debug)]
struct MessageShard {
    shard_id: u32,
    relative_path: PathBuf,
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
pub struct ConversationItem {
    pub id: String,
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
    pub created_at_unix: i64,
    pub status: i32,
    pub content: Value,
    pub content_decode_state: &'static str,
    pub content_truncated: bool,
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
    let items = decoded_rows
        .into_iter()
        .map(|row| ConversationItem {
            id: row.username,
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

    Ok(QueryEnvelope {
        schema: QUERY_SCHEMA,
        format_version: QUERY_FORMAT_VERSION,
        operation: "conversations.list",
        ok: true,
        source: source_description(source),
        consistency: QueryConsistency {
            guarantee: "singleDatabaseReadStatement",
            database_count: 1,
            cross_database_atomic: true,
            coverage_complete: true,
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

pub fn list_messages(
    source: &LiveQuerySource<'_>,
    conversation: &str,
    limit: usize,
    cursor: Option<&str>,
) -> Result<QueryEnvelope<MessageItem>, LiveQueryError> {
    validate_limit(limit)?;
    validate_conversation_id(conversation)?;
    let conversation_digest = digest_text("greenbubbles-conversation-v1", conversation);
    let cursor = cursor
        .map(decode_message_cursor)
        .transpose()?
        .map(|cursor| validate_message_cursor(source, &conversation_digest, cursor))
        .transpose()?;

    let table_name = format!("Msg_{:x}", md5::compute(conversation.as_bytes()));
    let shards = source.message_shards()?;
    let fetch_limit = limit.saturating_add(1);
    let mut messages = Vec::new();
    let mut warnings = Vec::new();
    let mut queried_database_count = 0usize;

    for shard in &shards {
        let connection = match source.open_database(&shard.relative_path) {
            Ok(connection) => connection,
            Err(_) => {
                warnings.push(QueryWarning {
                    code: "shardUnavailable",
                    message: "a message shard could not be opened read-only".into(),
                    shard_id: Some(shard.shard_id),
                    count: None,
                });
                continue;
            }
        };
        queried_database_count += 1;
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

        let columns = match table_columns(&connection, &table_name) {
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

        let sql = if cursor.is_some() {
            format!(
                "SELECT m.rowid, m.sort_seq, m.server_id, m.local_type, {sender_expression}, \
                        m.create_time, m.message_content, {packed_info}, {status}, \
                        {compression_type}, {compressed_content} \
                 FROM [{table_name}] m {sender_join} \
                 WHERE (m.sort_seq, m.create_time, m.server_id, {shard_id}, m.rowid) \
                       < (?1, ?2, ?3, ?4, ?5) \
                 ORDER BY m.sort_seq DESC, m.create_time DESC, m.server_id DESC, m.rowid DESC \
                 LIMIT ?6",
                shard_id = shard.shard_id,
            )
        } else {
            format!(
                "SELECT m.rowid, m.sort_seq, m.server_id, m.local_type, {sender_expression}, \
                        m.create_time, m.message_content, {packed_info}, {status}, \
                        {compression_type}, {compressed_content} \
                 FROM [{table_name}] m {sender_join} \
                 ORDER BY m.sort_seq DESC, m.create_time DESC, m.server_id DESC, m.rowid DESC \
                 LIMIT ?1"
            )
        };
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
        let query_result = if let Some(cursor) = &cursor {
            statement.query(params![
                cursor.sort_sequence,
                cursor.create_time,
                cursor.server_id,
                cursor.shard_id,
                cursor.row_id,
                fetch_limit as i64
            ])
        } else {
            statement.query(params![fetch_limit as i64])
        };
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
            let raw = (|| -> Result<RawMessage, rusqlite::Error> {
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
                        shard_id: shard.shard_id,
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
            })();
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
        let (sender, mut content, decode_state) = match decoded {
            Ok(message) => (
                message.sender,
                serde_json::to_value(message.content)
                    .unwrap_or_else(|_| json!({"unavailable": "projectionFailed"})),
                "complete",
            ),
            Err(_) => {
                content_decode_failures += 1;
                (raw.sender, json!({"unavailable": "decodeFailed"}), "failed")
            }
        };
        let mut truncated_field_count = 0usize;
        truncate_json_strings(
            &mut content,
            MAX_PROJECTED_TEXT_BYTES,
            &mut truncated_field_count,
        );
        let (sender, sender_truncated) = truncate_utf8(sender, MAX_PROJECTED_TEXT_BYTES);
        let id = encode_message_cursor(source, &conversation_digest, "message.identity", &raw.key)?;
        projected.push(MessageItem {
            id,
            conversation_id: conversation.to_string(),
            sort_sequence: raw.key.sort_sequence,
            server_id: raw.key.server_id,
            message_type,
            message_type_label: wx_db::msg_type_label(message_type),
            message_subtype,
            message_subtype_label: wx_db::msg_sub_type_label(message_type, message_subtype),
            sender,
            created_at_unix: raw.key.create_time,
            status: raw.status,
            content,
            content_decode_state: decode_state,
            content_truncated: sender_truncated || truncated_field_count > 0,
        });
    }
    if content_decode_failures > 0 {
        warnings.push(QueryWarning {
            code: "messageContentDecodeFailed",
            message: "one or more message bodies could not be decoded".into(),
            shard_id: None,
            count: Some(content_decode_failures),
        });
    }
    coalesce_warnings(&mut warnings);
    let coverage_complete = warnings.iter().all(|warning| {
        !matches!(
            warning.code,
            "shardUnavailable"
                | "shardSchemaUnavailable"
                | "unsupportedMessageSchema"
                | "shardQueryFailed"
                | "shardRowReadFailed"
                | "messageRowDecodeFailed"
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
        || cursor.row_id <= 0
    {
        return Err(LiveQueryError::InvalidCursor(
            "cursor does not belong to this operation, source, and conversation".into(),
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
    use std::os::unix::fs::PermissionsExt;

    use rusqlite::Connection;
    use tempfile::TempDir;

    use super::*;

    fn fixture() -> (TempDir, PathBuf) {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("db_storage");
        fs::create_dir_all(root.join("contact")).unwrap();
        fs::create_dir_all(root.join("session")).unwrap();
        fs::create_dir_all(root.join("message")).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();

        let contact = Connection::open(root.join("contact/contact.db")).unwrap();
        contact
            .execute_batch("CREATE TABLE contact(username TEXT PRIMARY KEY);")
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
