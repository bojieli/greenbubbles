use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use rusqlite::types::ValueRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::live_query::{
    get_message, get_search_result_message, LiveQueryError, LiveQuerySource, MessageItem,
};

pub const ATTACHMENT_SCHEMA: &str = "greenbubbles.attachment.v1";
pub const ATTACHMENT_FORMAT_VERSION: u32 = 1;

const MAXIMUM_CONVERSATION_DIRECTORIES: usize = 4_096;
const MAXIMUM_CONVERSATION_FILES: usize = 100_000;
const MAXIMUM_ATTACHMENT_CANDIDATES: usize = 256;
const MAXIMUM_IMAGE_SOURCE_BYTES: u64 = 128 * 1024 * 1024;
const MAXIMUM_VOICE_SOURCE_BYTES: u64 = 32 * 1024 * 1024;
const MAXIMUM_VIDEO_SOURCE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAXIMUM_DOCUMENT_SOURCE_BYTES: u64 = 512 * 1024 * 1024;
const MAXIMUM_DECODED_AUDIO_BYTES: u64 = 128 * 1024 * 1024;
const SQLITE_OWNER_MASK: u32 = 0o077;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttachmentKind {
    Image,
    Voice,
    Video,
    Document,
}

impl AttachmentKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Voice => "voice",
            Self::Video => "video",
            Self::Document => "document",
        }
    }

    const fn maximum_source_bytes(self) -> u64 {
        match self {
            Self::Image => MAXIMUM_IMAGE_SOURCE_BYTES,
            Self::Voice => MAXIMUM_VOICE_SOURCE_BYTES,
            Self::Video => MAXIMUM_VIDEO_SOURCE_BYTES,
            Self::Document => MAXIMUM_DOCUMENT_SOURCE_BYTES,
        }
    }
}

impl FromStr for AttachmentKind {
    type Err = LiveAttachmentError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "image" => Ok(Self::Image),
            "voice" | "audio" => Ok(Self::Voice),
            "video" => Ok(Self::Video),
            "document" | "file" => Ok(Self::Document),
            _ => Err(LiveAttachmentError::InvalidArgument(
                "--kind must be image, voice, video, or document".into(),
            )),
        }
    }
}

#[derive(Debug, Error)]
pub enum LiveAttachmentError {
    #[error("invalid attachment request: {0}")]
    InvalidArgument(String),
    #[error("unsafe attachment source: {0}")]
    UnsafeSource(String),
    #[error("attachment is unavailable: {0}")]
    Unavailable(String),
    #[error("attachment source changed during access")]
    SourceChanged,
    #[error("attachment decoding failed: {0}")]
    Decode(String),
    #[error("attachment output failed: {0}")]
    Output(String),
    #[error("attachment I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentInspection {
    pub schema: &'static str,
    pub format_version: u32,
    pub operation: &'static str,
    pub ok: bool,
    pub kind: &'static str,
    pub source_md5: String,
    pub availability: &'static str,
    pub candidate_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_attachment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_source_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_source_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_encryption_format: Option<&'static str>,
    pub account_image_key_required: bool,
    pub source_path_released: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMaterialization {
    pub schema: &'static str,
    pub format_version: u32,
    pub operation: &'static str,
    pub ok: bool,
    pub attachment_id: String,
    pub kind: &'static str,
    pub decoded_format: String,
    pub decoded_byte_count: u64,
    pub decoded_sha256: String,
    pub source_path_released: bool,
    pub output_path_released: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentErrorBody {
    pub code: &'static str,
    pub message: &'static str,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentErrorEnvelope {
    pub schema: &'static str,
    pub format_version: u32,
    pub operation: &'static str,
    pub ok: bool,
    pub error: AttachmentErrorBody,
}

#[derive(Debug)]
struct AttachmentSource {
    account_root: PathBuf,
}

#[derive(Debug)]
struct Candidate {
    source: CandidateSource,
    attachment_id: String,
    byte_count: u64,
    source_format: String,
    image_format: Option<wx_media::DatFormat>,
}

#[derive(Debug)]
enum CandidateSource {
    File {
        path: PathBuf,
        relative_path: PathBuf,
        version: FileVersion,
    },
    Voice {
        data: Vec<u8>,
    },
}

#[derive(Debug, Clone, Copy)]
struct FileVersion {
    device: u64,
    inode: u64,
    byte_count: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

#[derive(Debug)]
struct MessageAttachmentDescriptor {
    message_id: String,
    server_id: i64,
    source_md5: Option<String>,
    title: Option<String>,
}

pub fn inspect_image_attachment(
    account_root: &Path,
    conversation: &str,
    source_md5: &str,
) -> Result<AttachmentInspection, LiveAttachmentError> {
    validate_conversation(conversation)?;
    let source_md5 = normalize_md5(source_md5)?;
    let source = AttachmentSource::open(account_root)?;
    let candidates = source.resolve_candidates(conversation, &source_md5)?;
    let preferred = preferred_candidate(&candidates, &source_md5);
    Ok(AttachmentInspection {
        schema: ATTACHMENT_SCHEMA,
        format_version: ATTACHMENT_FORMAT_VERSION,
        operation: "attachment.inspect",
        ok: true,
        kind: "image",
        source_md5,
        availability: if candidates.is_empty() {
            "notDownloaded"
        } else {
            "downloaded"
        },
        candidate_count: candidates.len(),
        preferred_attachment_id: preferred.map(|candidate| candidate.attachment_id.clone()),
        preferred_source_byte_count: preferred.map(|candidate| candidate.byte_count),
        preferred_source_format: preferred.map(|candidate| candidate.source_format.clone()),
        source_encryption_format: preferred
            .map(|candidate| dat_format_name(candidate.image_format)),
        account_image_key_required: preferred
            .is_some_and(|candidate| candidate.image_format == Some(wx_media::DatFormat::V2)),
        source_path_released: false,
    })
}

pub fn materialize_image_attachment(
    account_root: &Path,
    conversation: &str,
    source_md5: &str,
    attachment_id: &str,
    output: &Path,
) -> Result<AttachmentMaterialization, LiveAttachmentError> {
    validate_conversation(conversation)?;
    let source_md5 = normalize_md5(source_md5)?;
    validate_attachment_id(attachment_id)?;
    let source = AttachmentSource::open(account_root)?;
    let candidates = source.resolve_candidates(conversation, &source_md5)?;
    let candidate = candidates
        .into_iter()
        .find(|candidate| candidate.attachment_id == attachment_id)
        .ok_or_else(|| {
            LiveAttachmentError::Unavailable(
                "the selected candidate no longer matches this conversation and MD5".into(),
            )
        })?;

    let encrypted = read_file_candidate(&candidate)?;
    let v2_aes_key = if candidate.image_format == Some(wx_media::DatFormat::V2) {
        Some(
            wx_media::derive_v2_key_from_dir(&source.account_root).map_err(|_| {
                LiveAttachmentError::Decode(
                    "the per-account V2 image key could not be derived".into(),
                )
            })?,
        )
    } else {
        None
    };
    let decoded = wx_media::decrypt_dat(
        &encrypted,
        &wx_media::DatDecryptOptions {
            v2_aes_key,
            xor_key: None,
        },
    )
    .map_err(|_| {
        LiveAttachmentError::Decode("the selected WeChat image could not be decoded safely".into())
    })?;
    if decoded.data.len() as u64 > MAXIMUM_IMAGE_SOURCE_BYTES {
        return Err(LiveAttachmentError::Decode(
            "decoded image exceeds the fixed materialization limit".into(),
        ));
    }
    let digest = hex::encode(Sha256::digest(&decoded.data));
    publish_output(&source.account_root, output, &decoded.data)?;

    Ok(AttachmentMaterialization {
        schema: ATTACHMENT_SCHEMA,
        format_version: ATTACHMENT_FORMAT_VERSION,
        operation: "attachment.materialize",
        ok: true,
        attachment_id: attachment_id.to_string(),
        kind: "image",
        decoded_format: decoded.ext,
        decoded_byte_count: decoded.data.len() as u64,
        decoded_sha256: digest,
        source_path_released: false,
        output_path_released: false,
    })
}

pub fn inspect_message_attachment(
    account_root: Option<&Path>,
    source: &LiveQuerySource<'_>,
    conversation: &str,
    message_id: &str,
    kind: AttachmentKind,
) -> Result<AttachmentInspection, LiveAttachmentError> {
    validate_conversation(conversation)?;
    let descriptor = resolve_message_descriptor(source, conversation, message_id, kind)?;
    let candidates =
        resolve_message_candidates(account_root, source, conversation, kind, &descriptor)?;
    let preferred = preferred_message_candidate(&candidates, kind);
    Ok(AttachmentInspection {
        schema: ATTACHMENT_SCHEMA,
        format_version: ATTACHMENT_FORMAT_VERSION,
        operation: "attachment.inspect",
        ok: true,
        kind: kind.name(),
        source_md5: descriptor.source_md5.unwrap_or_default(),
        availability: if candidates.is_empty() {
            "notDownloaded"
        } else {
            "downloaded"
        },
        candidate_count: candidates.len(),
        preferred_attachment_id: preferred.map(|candidate| candidate.attachment_id.clone()),
        preferred_source_byte_count: preferred.map(|candidate| candidate.byte_count),
        preferred_source_format: preferred.map(|candidate| candidate.source_format.clone()),
        source_encryption_format: preferred
            .filter(|_| kind == AttachmentKind::Image)
            .map(|candidate| dat_format_name(candidate.image_format)),
        account_image_key_required: preferred.is_some_and(|candidate| {
            kind == AttachmentKind::Image && candidate.image_format == Some(wx_media::DatFormat::V2)
        }),
        source_path_released: false,
    })
}

pub fn materialize_message_attachment(
    account_root: Option<&Path>,
    source: &LiveQuerySource<'_>,
    conversation: &str,
    message_id: &str,
    kind: AttachmentKind,
    attachment_id: &str,
    output: &Path,
) -> Result<AttachmentMaterialization, LiveAttachmentError> {
    validate_conversation(conversation)?;
    validate_attachment_id(attachment_id)?;
    let descriptor = resolve_message_descriptor(source, conversation, message_id, kind)?;
    let candidates =
        resolve_message_candidates(account_root, source, conversation, kind, &descriptor)?;
    let candidate = candidates
        .into_iter()
        .find(|candidate| candidate.attachment_id == attachment_id)
        .ok_or_else(|| {
            LiveAttachmentError::Unavailable(
                "the selected candidate no longer matches this source, conversation, message, and kind"
                    .into(),
            )
        })?;

    let protected_root = account_root.unwrap_or_else(|| source.root());
    let (decoded_format, decoded_byte_count, decoded_sha256) = match kind {
        AttachmentKind::Image => {
            let encrypted = read_file_candidate(&candidate)?;
            let v2_aes_key = if candidate.image_format == Some(wx_media::DatFormat::V2) {
                let account_root = account_root.ok_or_else(|| {
                    LiveAttachmentError::Unavailable(
                        "image materialization requires the authorized WeChat account root".into(),
                    )
                })?;
                Some(wx_media::derive_v2_key_from_dir(account_root).map_err(|_| {
                    LiveAttachmentError::Decode(
                        "the per-account V2 image key could not be derived".into(),
                    )
                })?)
            } else {
                None
            };
            let decoded = wx_media::decrypt_dat(
                &encrypted,
                &wx_media::DatDecryptOptions {
                    v2_aes_key,
                    xor_key: None,
                },
            )
            .map_err(|_| {
                LiveAttachmentError::Decode(
                    "the selected WeChat image could not be decoded safely".into(),
                )
            })?;
            if decoded.data.len() as u64 > MAXIMUM_IMAGE_SOURCE_BYTES {
                return Err(LiveAttachmentError::Decode(
                    "decoded image exceeds the fixed materialization limit".into(),
                ));
            }
            let digest = hex::encode(Sha256::digest(&decoded.data));
            publish_output(protected_root, output, &decoded.data)?;
            (decoded.ext, decoded.data.len() as u64, digest)
        }
        AttachmentKind::Voice => {
            let CandidateSource::Voice { data } = candidate.source else {
                return Err(LiveAttachmentError::Decode(
                    "the selected voice candidate has an incompatible source".into(),
                ));
            };
            let (data, format) = match wx_media::transcode_silk_to_ogg_opus(&data) {
                Ok(decoded) => (decoded.data, decoded.ext.to_string()),
                Err(_) => (data, "silk".to_string()),
            };
            if data.len() as u64 > MAXIMUM_DECODED_AUDIO_BYTES {
                return Err(LiveAttachmentError::Decode(
                    "decoded audio exceeds the fixed materialization limit".into(),
                ));
            }
            let digest = hex::encode(Sha256::digest(&data));
            publish_output(protected_root, output, &data)?;
            (format, data.len() as u64, digest)
        }
        AttachmentKind::Video | AttachmentKind::Document => {
            let format = candidate.source_format.clone();
            let (byte_count, digest) = publish_file_candidate(
                protected_root,
                output,
                &candidate,
                kind.maximum_source_bytes(),
            )?;
            (format, byte_count, digest)
        }
    };

    Ok(AttachmentMaterialization {
        schema: ATTACHMENT_SCHEMA,
        format_version: ATTACHMENT_FORMAT_VERSION,
        operation: "attachment.materialize",
        ok: true,
        attachment_id: attachment_id.to_string(),
        kind: kind.name(),
        decoded_format,
        decoded_byte_count,
        decoded_sha256,
        source_path_released: false,
        output_path_released: false,
    })
}

pub fn serialize_attachment_error(
    operation: &'static str,
    code: &'static str,
    message: &'static str,
    retryable: bool,
) -> String {
    serde_json::to_string_pretty(&AttachmentErrorEnvelope {
        schema: ATTACHMENT_SCHEMA,
        format_version: ATTACHMENT_FORMAT_VERSION,
        operation,
        ok: false,
        error: AttachmentErrorBody {
            code,
            message,
            retryable,
        },
    })
    .expect("attachment error envelopes contain only static serializable fields")
}

fn resolve_message_descriptor(
    source: &LiveQuerySource<'_>,
    conversation: &str,
    message_id: &str,
    kind: AttachmentKind,
) -> Result<MessageAttachmentDescriptor, LiveAttachmentError> {
    let resource = match get_message(source, conversation, message_id) {
        Ok(resource) => resource,
        Err(LiveQueryError::InvalidCursor(_)) => {
            get_search_result_message(source, conversation, message_id)
                .map_err(map_live_query_error)?
        }
        Err(error) => return Err(map_live_query_error(error)),
    };
    let message = resource.item;
    if !message_matches_kind(&message, kind) {
        return Err(LiveAttachmentError::InvalidArgument(
            "the exact message identity does not describe the requested attachment kind".into(),
        ));
    }
    let source_md5 = extract_unique_message_field(&message.content, "md5")
        .and_then(|value| normalize_md5(&value).ok());
    let title = extract_unique_message_field(&message.content, "title")
        .filter(|value| !value.is_empty() && value.len() <= 16 * 1024);
    match kind {
        AttachmentKind::Image | AttachmentKind::Video if source_md5.is_none() => {
            return Err(LiveAttachmentError::Unavailable(
                "the exact message has no supported 32-hex attachment MD5".into(),
            ));
        }
        AttachmentKind::Document if source_md5.is_none() && title.is_none() => {
            return Err(LiveAttachmentError::Unavailable(
                "the exact document message has neither a supported MD5 nor title locator".into(),
            ));
        }
        AttachmentKind::Voice if message.server_id <= 0 => {
            return Err(LiveAttachmentError::Unavailable(
                "the exact voice message has no positive server identity".into(),
            ));
        }
        _ => {}
    }
    Ok(MessageAttachmentDescriptor {
        message_id: message_id.to_string(),
        server_id: message.server_id,
        source_md5,
        title,
    })
}

fn map_live_query_error(error: LiveQueryError) -> LiveAttachmentError {
    match error {
        LiveQueryError::InvalidArgument(_) | LiveQueryError::InvalidCursor(_) => {
            LiveAttachmentError::InvalidArgument(
                "message identity is not bound to this source and conversation".into(),
            )
        }
        LiveQueryError::UnsafeSource(_) => LiveAttachmentError::UnsafeSource(
            "the database source required for attachment lookup is unsafe".into(),
        ),
        LiveQueryError::NotFound(_) => LiveAttachmentError::Unavailable(
            "the exact source message is no longer available".into(),
        ),
        _ => LiveAttachmentError::Unavailable(
            "the exact source message could not be hydrated read-only".into(),
        ),
    }
}

fn message_matches_kind(message: &MessageItem, kind: AttachmentKind) -> bool {
    match (kind, message.message_type, message.message_subtype) {
        (AttachmentKind::Image, 3, _) | (AttachmentKind::Image, 49, 2 | 8) => true,
        (AttachmentKind::Voice, 34, _) | (AttachmentKind::Voice, 49, 3) => true,
        (AttachmentKind::Video, 43, _) | (AttachmentKind::Video, 49, 4 | 51 | 63) => true,
        (AttachmentKind::Document, 49, 6 | 74) => true,
        _ => false,
    }
}

fn extract_unique_message_field(value: &Value, requested: &str) -> Option<String> {
    fn collect(value: &Value, requested: &str, values: &mut BTreeSet<String>) {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    if key.eq_ignore_ascii_case(requested) {
                        if let Some(value) = value.as_str().filter(|value| !value.is_empty()) {
                            values.insert(value.to_string());
                        }
                    }
                    collect(value, requested, values);
                }
            }
            Value::Array(array) => {
                for value in array {
                    collect(value, requested, values);
                }
            }
            _ => {}
        }
    }
    let mut values = BTreeSet::new();
    collect(value, requested, &mut values);
    (values.len() == 1)
        .then(|| values.into_iter().next())
        .flatten()
}

fn resolve_message_candidates(
    account_root: Option<&Path>,
    source: &LiveQuerySource<'_>,
    conversation: &str,
    kind: AttachmentKind,
    descriptor: &MessageAttachmentDescriptor,
) -> Result<Vec<Candidate>, LiveAttachmentError> {
    match kind {
        AttachmentKind::Voice => resolve_voice_candidates(source, conversation, descriptor),
        AttachmentKind::Image => {
            let account_root = account_root.ok_or_else(|| {
                LiveAttachmentError::Unavailable(
                    "filesystem attachment lookup requires a WeChat account root".into(),
                )
            })?;
            let source = AttachmentSource::open(account_root)?;
            source.resolve_image_candidates(
                conversation,
                descriptor.source_md5.as_deref().ok_or_else(|| {
                    LiveAttachmentError::Unavailable("image MD5 is unavailable".into())
                })?,
                Some(&descriptor.message_id),
            )
        }
        AttachmentKind::Video | AttachmentKind::Document => {
            let account_root = account_root.ok_or_else(|| {
                LiveAttachmentError::Unavailable(
                    "filesystem attachment lookup requires a WeChat account root".into(),
                )
            })?;
            resolve_filesystem_message_candidates(
                account_root,
                source,
                conversation,
                kind,
                descriptor,
            )
        }
    }
}

fn resolve_voice_candidates(
    source: &LiveQuerySource<'_>,
    conversation: &str,
    descriptor: &MessageAttachmentDescriptor,
) -> Result<Vec<Candidate>, LiveAttachmentError> {
    const MAXIMUM_VOICE_CANDIDATES: usize = 256;
    let mut candidates = Vec::new();
    let mut cumulative_bytes = 0u64;
    for relative_path in source.media_databases().map_err(map_live_query_error)? {
        let connection = source
            .open_database(&relative_path)
            .map_err(map_live_query_error)?;
        let exists = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND lower(name) = 'voiceinfo')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|_| {
                LiveAttachmentError::Unavailable(
                    "a media database schema could not be inspected read-only".into(),
                )
            })?
            == 1;
        if !exists {
            continue;
        }
        let columns = sqlite_table_columns(&connection, "VoiceInfo")?;
        let Some(server_column) = find_column_alias(
            &columns,
            &["svr_id", "server_id", "msg_svr_id", "msg_server_id"],
        ) else {
            continue;
        };
        let Some(data_column) = find_column_alias(&columns, &["voice_data", "voice", "data"])
        else {
            continue;
        };
        let sql = format!(
            "SELECT rowid, [{}] FROM [VoiceInfo] WHERE [{}] = ?1 ORDER BY rowid ASC LIMIT ?2",
            escape_identifier(data_column),
            escape_identifier(server_column),
        );
        let mut statement = connection.prepare(&sql).map_err(|_| {
            LiveAttachmentError::Unavailable(
                "a bounded voice lookup could not be prepared read-only".into(),
            )
        })?;
        let mut rows = statement
            .query(rusqlite::params![
                descriptor.server_id,
                (MAXIMUM_VOICE_CANDIDATES + 1) as i64
            ])
            .map_err(|_| {
                LiveAttachmentError::Unavailable(
                    "a bounded voice lookup could not be executed read-only".into(),
                )
            })?;
        while let Some(row) = rows.next().map_err(|_| {
            LiveAttachmentError::Unavailable(
                "a voice row became unreadable during bounded inspection".into(),
            )
        })? {
            if candidates.len() >= MAXIMUM_VOICE_CANDIDATES {
                return Err(LiveAttachmentError::Unavailable(
                    "voice candidate count exceeds the fixed inspection limit".into(),
                ));
            }
            let row_id = row.get::<_, i64>(0).map_err(|_| {
                LiveAttachmentError::Unavailable("voice row identity is incompatible".into())
            })?;
            let data = sqlite_blob_value(row.get_ref(1).map_err(|_| {
                LiveAttachmentError::Unavailable("voice payload type is incompatible".into())
            })?)?;
            if data.is_empty() {
                continue;
            }
            if data.len() as u64 > MAXIMUM_VOICE_SOURCE_BYTES {
                return Err(LiveAttachmentError::UnsafeSource(
                    "voice payload exceeds the fixed source-size limit".into(),
                ));
            }
            cumulative_bytes = cumulative_bytes.saturating_add(data.len() as u64);
            if cumulative_bytes > MAXIMUM_DECODED_AUDIO_BYTES {
                return Err(LiveAttachmentError::Unavailable(
                    "voice candidate bytes exceed the fixed inspection limit".into(),
                ));
            }
            let content_sha256 = Sha256::digest(&data);
            let mut identity = Sha256::new();
            identity.update(b"greenbubbles-live-voice-attachment-v1\0");
            identity.update(source.identity().as_bytes());
            identity.update([0]);
            identity.update(conversation.as_bytes());
            identity.update([0]);
            identity.update(descriptor.message_id.as_bytes());
            identity.update([0]);
            identity.update(relative_path.as_os_str().as_encoded_bytes());
            identity.update(row_id.to_le_bytes());
            identity.update(descriptor.server_id.to_le_bytes());
            identity.update(content_sha256);
            candidates.push(Candidate {
                attachment_id: hex::encode(identity.finalize()),
                byte_count: data.len() as u64,
                source_format: detect_source_format(&data[..data.len().min(32)], None),
                image_format: None,
                source: CandidateSource::Voice { data },
            });
        }
    }
    candidates.sort_by(|left, right| left.attachment_id.cmp(&right.attachment_id));
    Ok(candidates)
}

fn sqlite_blob_value(value: ValueRef<'_>) -> Result<Vec<u8>, LiveAttachmentError> {
    match value {
        ValueRef::Blob(value) | ValueRef::Text(value) => Ok(value.to_vec()),
        ValueRef::Null => Ok(Vec::new()),
        ValueRef::Integer(_) | ValueRef::Real(_) => Err(LiveAttachmentError::Unavailable(
            "voice payload has an incompatible SQLite value type".into(),
        )),
    }
}

fn sqlite_table_columns(
    connection: &rusqlite::Connection,
    table: &str,
) -> Result<Vec<String>, LiveAttachmentError> {
    let sql = format!("PRAGMA table_info([{}])", escape_identifier(table));
    let mut statement = connection.prepare(&sql).map_err(|_| {
        LiveAttachmentError::Unavailable("attachment metadata schema is incompatible".into())
    })?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|_| {
            LiveAttachmentError::Unavailable("attachment metadata schema is incompatible".into())
        })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|_| {
        LiveAttachmentError::Unavailable("attachment metadata schema is incompatible".into())
    })
}

fn find_column_alias<'a>(columns: &'a [String], aliases: &[&str]) -> Option<&'a str> {
    columns
        .iter()
        .find(|column| {
            aliases
                .iter()
                .any(|alias| column.eq_ignore_ascii_case(alias))
        })
        .map(String::as_str)
}

fn escape_identifier(value: &str) -> String {
    value.replace(']', "]]")
}

#[derive(Debug)]
struct HardlinkRow {
    file_name: String,
    dir1: String,
    dir2: String,
}

fn resolve_filesystem_message_candidates(
    account_root: &Path,
    source: &LiveQuerySource<'_>,
    conversation: &str,
    kind: AttachmentKind,
    descriptor: &MessageAttachmentDescriptor,
) -> Result<Vec<Candidate>, LiveAttachmentError> {
    let attachment_source = AttachmentSource::open(account_root)?;
    let account_root = &attachment_source.account_root;
    let mut paths = BTreeMap::<PathBuf, PathBuf>::new();
    if let Some(source_md5) = descriptor.source_md5.as_deref() {
        for path in hardlink_candidate_paths(source, account_root, conversation, kind, source_md5)?
        {
            add_existing_candidate_path(account_root, path, &mut paths)?;
        }
    }
    scan_scoped_filesystem_candidates(
        account_root,
        conversation,
        kind,
        descriptor.source_md5.as_deref(),
        descriptor.title.as_deref(),
        &mut paths,
    )?;
    if paths.len() > MAXIMUM_ATTACHMENT_CANDIDATES {
        return Err(LiveAttachmentError::Unavailable(
            "attachment candidate count exceeds the fixed inspection limit".into(),
        ));
    }
    let source_md5 = descriptor.source_md5.as_deref().unwrap_or("");
    paths
        .into_iter()
        .map(|(relative_path, path)| {
            inspect_file_candidate(
                account_root,
                path,
                relative_path,
                conversation,
                source_md5,
                kind,
                Some(&descriptor.message_id),
            )
        })
        .collect()
}

fn hardlink_candidate_paths(
    source: &LiveQuerySource<'_>,
    account_root: &Path,
    conversation: &str,
    kind: AttachmentKind,
    source_md5: &str,
) -> Result<Vec<PathBuf>, LiveAttachmentError> {
    let Some(connection) = source
        .open_optional_database(Path::new("hardlink/hardlink.db"))
        .map_err(map_live_query_error)?
    else {
        return Ok(Vec::new());
    };
    let prefix = match kind {
        AttachmentKind::Video => "video",
        AttachmentKind::Document => "file",
        _ => return Ok(Vec::new()),
    };
    let table = [
        format!("{prefix}_hardlink_info_v3"),
        format!("{prefix}_hardlink_info_v4"),
    ]
    .into_iter()
    .find(|table| {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
                [table],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or_default()
            == 1
    });
    let Some(table) = table else {
        return Ok(Vec::new());
    };
    let has_directory_map = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'dir2id')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or_default()
        == 1;
    if !has_directory_map {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT f.file_name, COALESCE(d1.username, ''), COALESCE(d2.username, '') \
         FROM [{table}] f \
         LEFT JOIN dir2id d1 ON d1.rowid = f.dir1 \
         LEFT JOIN dir2id d2 ON d2.rowid = f.dir2 \
         WHERE lower(f.md5) = lower(?1) \
         ORDER BY f.file_name ASC, f.rowid ASC LIMIT ?2"
    );
    let mut statement = match connection.prepare(&sql) {
        Ok(statement) => statement,
        Err(_) => return Ok(Vec::new()),
    };
    let rows = match statement.query_map(
        rusqlite::params![source_md5, (MAXIMUM_ATTACHMENT_CANDIDATES + 1) as i64],
        |row| {
            Ok(HardlinkRow {
                file_name: row.get(0)?,
                dir1: row.get(1)?,
                dir2: row.get(2)?,
            })
        },
    ) {
        Ok(rows) => rows,
        Err(_) => return Ok(Vec::new()),
    };
    let mut records = Vec::new();
    for row in rows {
        let row = match row {
            Ok(row) => row,
            Err(_) => continue,
        };
        records.push(row);
        if records.len() > MAXIMUM_ATTACHMENT_CANDIDATES {
            return Err(LiveAttachmentError::Unavailable(
                "hardlink metadata exceeds the fixed candidate limit".into(),
            ));
        }
    }
    let conversation_hash = format!("{:x}", md5::compute(conversation.as_bytes()));
    let mut paths = Vec::new();
    for record in records {
        if record.dir1 != conversation && record.dir1 != conversation_hash {
            continue;
        }
        validate_path_segment(&record.dir1, "hardlink conversation directory")?;
        validate_path_segment(&record.dir2, "hardlink month directory")?;
        validate_path_segment(&record.file_name, "hardlink filename")?;
        match kind {
            AttachmentKind::Video => {
                paths.extend([
                    account_root
                        .join("msg/attach")
                        .join(&record.dir1)
                        .join(&record.dir2)
                        .join("Video")
                        .join(&record.file_name),
                    account_root
                        .join("msg/attach")
                        .join(&record.dir1)
                        .join(&record.dir2)
                        .join(&record.file_name),
                    account_root
                        .join("msg/video")
                        .join(&record.dir1)
                        .join(&record.dir2)
                        .join(&record.file_name),
                ]);
            }
            AttachmentKind::Document => {
                paths.extend([
                    account_root
                        .join("msg/file")
                        .join(&record.dir1)
                        .join(&record.dir2)
                        .join(&record.file_name),
                    account_root
                        .join("msg/file")
                        .join(&record.dir1)
                        .join(&record.file_name),
                    account_root
                        .join("msg/attach")
                        .join(&record.dir1)
                        .join(&record.dir2)
                        .join("File")
                        .join(&record.file_name),
                ]);
            }
            _ => {}
        }
    }
    Ok(paths)
}

fn add_existing_candidate_path(
    account_root: &Path,
    path: PathBuf,
    paths: &mut BTreeMap<PathBuf, PathBuf>,
) -> Result<(), LiveAttachmentError> {
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Err(LiveAttachmentError::UnsafeSource(
            "attachment metadata resolved a symbolic-link candidate".into(),
        ));
    }
    if !metadata.is_file() {
        return Ok(());
    }
    let relative = path
        .strip_prefix(account_root)
        .map_err(|_| {
            LiveAttachmentError::UnsafeSource(
                "attachment candidate escaped the selected account".into(),
            )
        })?
        .to_path_buf();
    validate_relative_path(&relative)?;
    paths.insert(relative, path);
    Ok(())
}

fn scan_scoped_filesystem_candidates(
    account_root: &Path,
    conversation: &str,
    kind: AttachmentKind,
    source_md5: Option<&str>,
    title: Option<&str>,
    paths: &mut BTreeMap<PathBuf, PathBuf>,
) -> Result<(), LiveAttachmentError> {
    validate_path_segment(conversation, "conversation identifier")?;
    let conversation_hash = format!("{:x}", md5::compute(conversation.as_bytes()));
    let mut roots = Vec::<(PathBuf, Option<&'static str>)>::new();
    for segment in [conversation, conversation_hash.as_str()] {
        match kind {
            AttachmentKind::Video => {
                roots.push((account_root.join("msg/attach").join(segment), Some("Video")));
                roots.push((account_root.join("msg/video").join(segment), None));
            }
            AttachmentKind::Document => {
                roots.push((account_root.join("msg/file").join(segment), None));
                roots.push((account_root.join("msg/attach").join(segment), Some("File")));
            }
            _ => {}
        }
    }
    let title_basename = title
        .and_then(|title| Path::new(title).file_name())
        .and_then(|name| name.to_str())
        .map(str::to_string);
    let mut directory_count = 0usize;
    let mut file_count = 0usize;
    for (conversation_root, nested_directory) in roots {
        match fs::symlink_metadata(&conversation_root) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
        validate_owned_directory_within(
            account_root,
            &conversation_root,
            "conversation media root",
        )?;
        for month in fs::read_dir(&conversation_root)? {
            let month = month?;
            directory_count = directory_count.saturating_add(1);
            if directory_count > MAXIMUM_CONVERSATION_DIRECTORIES {
                return Err(LiveAttachmentError::Unavailable(
                    "conversation media directory exceeds the bounded traversal limit".into(),
                ));
            }
            let file_type = month.file_type()?;
            if file_type.is_symlink() || !file_type.is_dir() {
                continue;
            }
            validate_owned_directory_within(
                account_root,
                &month.path(),
                "conversation media month directory",
            )?;
            let inventory_root = nested_directory
                .map(|nested| month.path().join(nested))
                .unwrap_or_else(|| month.path());
            match fs::symlink_metadata(&inventory_root) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            }
            validate_owned_directory_within(
                account_root,
                &inventory_root,
                "conversation media inventory directory",
            )?;
            for entry in fs::read_dir(&inventory_root)? {
                let entry = entry?;
                file_count = file_count.saturating_add(1);
                if file_count > MAXIMUM_CONVERSATION_FILES {
                    return Err(LiveAttachmentError::Unavailable(
                        "conversation media inventory exceeds the bounded file limit".into(),
                    ));
                }
                let file_type = entry.file_type()?;
                if file_type.is_symlink() || !file_type.is_file() {
                    continue;
                }
                let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if !candidate_filename_matches(&name, kind, source_md5, title_basename.as_deref()) {
                    continue;
                }
                add_existing_candidate_path(account_root, entry.path(), paths)?;
                if paths.len() > MAXIMUM_ATTACHMENT_CANDIDATES {
                    return Err(LiveAttachmentError::Unavailable(
                        "attachment candidate count exceeds the fixed inspection limit".into(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn candidate_filename_matches(
    name: &str,
    kind: AttachmentKind,
    source_md5: Option<&str>,
    title_basename: Option<&str>,
) -> bool {
    let lower = name.to_ascii_lowercase();
    let md5_matches = source_md5.is_some_and(|md5| lower.contains(md5));
    match kind {
        AttachmentKind::Video => {
            let extension = Path::new(name)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            md5_matches && matches!(extension.as_str(), "mp4" | "mov" | "m4v" | "mkv" | "avi")
        }
        AttachmentKind::Document => {
            md5_matches || title_basename.is_some_and(|title| name.eq_ignore_ascii_case(title))
        }
        _ => false,
    }
}

fn validate_path_segment(value: &str, description: &str) -> Result<(), LiveAttachmentError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 16 * 1024
        || path.is_absolute()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(LiveAttachmentError::UnsafeSource(format!(
            "{description} is not a safe single path segment"
        )));
    }
    Ok(())
}

impl AttachmentSource {
    fn open(account_root: &Path) -> Result<Self, LiveAttachmentError> {
        let metadata = fs::symlink_metadata(account_root)
            .map_err(|_| LiveAttachmentError::UnsafeSource("account root is unavailable".into()))?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(LiveAttachmentError::UnsafeSource(
                "account root must be a current-user-owned real directory".into(),
            ));
        }
        let account_root = account_root.canonicalize().map_err(|_| {
            LiveAttachmentError::UnsafeSource("account root could not be canonicalized".into())
        })?;
        Ok(Self { account_root })
    }

    fn resolve_candidates(
        &self,
        conversation: &str,
        source_md5: &str,
    ) -> Result<Vec<Candidate>, LiveAttachmentError> {
        self.resolve_image_candidates(conversation, source_md5, None)
    }

    fn resolve_image_candidates(
        &self,
        conversation: &str,
        source_md5: &str,
        message_id: Option<&str>,
    ) -> Result<Vec<Candidate>, LiveAttachmentError> {
        let attach_root = self.account_root.join("msg/attach");
        validate_owned_directory_within(
            &self.account_root,
            &attach_root,
            "message attachment root",
        )?;
        let conversation_hash = format!("{:x}", md5::compute(conversation.as_bytes()));
        let conversation_root = attach_root.join(conversation_hash);
        match fs::symlink_metadata(&conversation_root) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
            Ok(_) => validate_owned_directory_within(
                &self.account_root,
                &conversation_root,
                "conversation attachment directory",
            )?,
        }

        let mut candidates = Vec::new();
        let mut directory_count = 0usize;
        let mut file_count = 0usize;
        for directory in fs::read_dir(&conversation_root)? {
            let directory = directory?;
            directory_count += 1;
            if directory_count > MAXIMUM_CONVERSATION_DIRECTORIES {
                return Err(LiveAttachmentError::Unavailable(
                    "conversation attachment directory exceeds the bounded scan limit".into(),
                ));
            }
            if !directory.file_type()?.is_dir() {
                continue;
            }
            validate_owned_directory_within(
                &self.account_root,
                &directory.path(),
                "conversation attachment month directory",
            )?;
            let image_directory = directory.path().join("Img");
            match fs::symlink_metadata(&image_directory) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
                Ok(_) => validate_owned_directory_within(
                    &self.account_root,
                    &image_directory,
                    "image directory",
                )?,
            }
            for entry in fs::read_dir(&image_directory)? {
                let entry = entry?;
                file_count += 1;
                if file_count > MAXIMUM_CONVERSATION_FILES {
                    return Err(LiveAttachmentError::Unavailable(
                        "conversation image inventory exceeds the bounded scan limit".into(),
                    ));
                }
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                if !name.starts_with(source_md5) || !name.ends_with(".dat") {
                    continue;
                }
                let path = entry.path();
                let relative_path = path
                    .strip_prefix(&self.account_root)
                    .map_err(|_| {
                        LiveAttachmentError::UnsafeSource(
                            "attachment candidate escaped the selected account".into(),
                        )
                    })?
                    .to_path_buf();
                validate_relative_path(&relative_path)?;
                candidates.push(inspect_file_candidate(
                    &self.account_root,
                    path,
                    relative_path,
                    conversation,
                    source_md5,
                    AttachmentKind::Image,
                    message_id,
                )?);
            }
        }
        candidates.sort_by(|left, right| {
            candidate_relative_path(left).cmp(candidate_relative_path(right))
        });
        Ok(candidates)
    }
}

fn inspect_file_candidate(
    account_root: &Path,
    path: PathBuf,
    relative_path: PathBuf,
    conversation: &str,
    source_md5: &str,
    kind: AttachmentKind,
    message_id: Option<&str>,
) -> Result<Candidate, LiveAttachmentError> {
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.len() == 0
        || metadata.len() > kind.maximum_source_bytes()
    {
        return Err(LiveAttachmentError::UnsafeSource(
            "attachment candidate is not a bounded current-user-owned regular file".into(),
        ));
    }
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(account_root) {
        return Err(LiveAttachmentError::UnsafeSource(
            "attachment candidate escaped the selected account".into(),
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)?;
    let mut prefix = [0u8; 32];
    let count = file.read(&mut prefix)?;
    let image_format = (kind == AttachmentKind::Image)
        .then(|| wx_media::detect_dat_format(&prefix[..count]))
        .flatten();
    let source_format = detect_source_format(&prefix[..count], path.extension());
    let version = FileVersion::from_metadata(&metadata);
    let attachment_id = candidate_identity(
        conversation,
        source_md5,
        &relative_path,
        &metadata,
        kind,
        message_id,
    );
    Ok(Candidate {
        source: CandidateSource::File {
            path,
            relative_path,
            version,
        },
        attachment_id,
        byte_count: metadata.len(),
        source_format,
        image_format,
    })
}

fn read_file_candidate(candidate: &Candidate) -> Result<Vec<u8>, LiveAttachmentError> {
    let CandidateSource::File { path, version, .. } = &candidate.source else {
        return Err(LiveAttachmentError::Decode(
            "the selected candidate is not a filesystem artifact".into(),
        ));
    };
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let before = file.metadata()?;
    if !version.matches(&before) {
        return Err(LiveAttachmentError::SourceChanged);
    }
    let mut data = Vec::with_capacity(before.len() as usize);
    Read::by_ref(&mut file)
        .take(candidate.byte_count + 1)
        .read_to_end(&mut data)?;
    if data.len() as u64 != before.len() || data.len() as u64 > candidate.byte_count {
        return Err(LiveAttachmentError::SourceChanged);
    }
    let after = file.metadata()?;
    if !version.matches(&after) {
        return Err(LiveAttachmentError::SourceChanged);
    }
    Ok(data)
}

fn publish_file_candidate(
    protected_root: &Path,
    output: &Path,
    candidate: &Candidate,
    maximum_bytes: u64,
) -> Result<(u64, String), LiveAttachmentError> {
    let CandidateSource::File { path, version, .. } = &candidate.source else {
        return Err(LiveAttachmentError::Decode(
            "the selected candidate is not a filesystem artifact".into(),
        ));
    };
    let (parent, final_output) = validate_new_output(protected_root, output)?;
    let mut source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let before = source.metadata()?;
    if !version.matches(&before) || before.len() > maximum_bytes {
        return Err(LiveAttachmentError::SourceChanged);
    }
    let mut temporary = tempfile::Builder::new()
        .prefix(".greenbubbles-attachment-")
        .tempfile_in(&parent)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 128 * 1024];
    let mut byte_count = 0u64;
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        byte_count = byte_count.saturating_add(read as u64);
        if byte_count > maximum_bytes || byte_count > before.len() {
            return Err(LiveAttachmentError::SourceChanged);
        }
        digest.update(&buffer[..read]);
        temporary.as_file_mut().write_all(&buffer[..read])?;
    }
    let after = source.metadata()?;
    if byte_count != before.len() || !version.matches(&after) {
        return Err(LiveAttachmentError::SourceChanged);
    }
    temporary.as_file_mut().flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(&final_output).map_err(|_| {
        LiveAttachmentError::Output("attachment output could not be published atomically".into())
    })?;
    File::open(parent)?.sync_all()?;
    Ok((byte_count, hex::encode(digest.finalize())))
}

fn validate_new_output(
    protected_root: &Path,
    output: &Path,
) -> Result<(PathBuf, PathBuf), LiveAttachmentError> {
    if fs::symlink_metadata(output).is_ok() {
        return Err(LiveAttachmentError::Output(
            "output already exists; overwrite is not supported".into(),
        ));
    }
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let parent = validate_owner_only_directory(parent, "output parent")?;
    let protected_root = protected_root.canonicalize().map_err(|_| {
        LiveAttachmentError::Output("protected source root could not be canonicalized".into())
    })?;
    if parent.starts_with(protected_root) {
        return Err(LiveAttachmentError::Output(
            "attachment output must be outside the protected source root".into(),
        ));
    }
    let file_name = output
        .file_name()
        .ok_or_else(|| LiveAttachmentError::Output("output has no final filename".into()))?;
    validate_path_segment(
        file_name.to_str().ok_or_else(|| {
            LiveAttachmentError::Output("output filename is not valid UTF-8".into())
        })?,
        "output filename",
    )
    .map_err(|_| {
        LiveAttachmentError::Output("output filename is not a safe path segment".into())
    })?;
    Ok((parent.clone(), parent.join(file_name)))
}

fn publish_output(
    protected_root: &Path,
    output: &Path,
    data: &[u8],
) -> Result<(), LiveAttachmentError> {
    let (parent, final_output) = validate_new_output(protected_root, output)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".greenbubbles-attachment-")
        .tempfile_in(&parent)?;
    temporary.as_file_mut().write_all(data)?;
    temporary.as_file_mut().flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(&final_output).map_err(|_| {
        LiveAttachmentError::Output("decoded output could not be published atomically".into())
    })?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn validate_owned_real_directory(
    path: &Path,
    description: &str,
) -> Result<(), LiveAttachmentError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| LiveAttachmentError::UnsafeSource(format!("{description} is unavailable")))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(LiveAttachmentError::UnsafeSource(format!(
            "{description} must be a current-user-owned real directory"
        )));
    }
    Ok(())
}

fn validate_owned_directory_within(
    account_root: &Path,
    path: &Path,
    description: &str,
) -> Result<(), LiveAttachmentError> {
    validate_owned_real_directory(path, description)?;
    let canonical = path.canonicalize().map_err(|_| {
        LiveAttachmentError::UnsafeSource(format!("{description} could not be canonicalized"))
    })?;
    if canonical != path || !canonical.starts_with(account_root) {
        return Err(LiveAttachmentError::UnsafeSource(format!(
            "{description} contains a symbolic link or escapes the selected account"
        )));
    }
    Ok(())
}

fn validate_owner_only_directory(
    path: &Path,
    description: &str,
) -> Result<PathBuf, LiveAttachmentError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| LiveAttachmentError::Output(format!("{description} is unavailable")))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & SQLITE_OWNER_MASK != 0
    {
        return Err(LiveAttachmentError::Output(format!(
            "{description} must be a current-user-owned owner-only real directory"
        )));
    }
    Ok(path.canonicalize()?)
}

fn validate_relative_path(path: &Path) -> Result<(), LiveAttachmentError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(LiveAttachmentError::UnsafeSource(
            "attachment relative path is invalid".into(),
        ));
    }
    Ok(())
}

fn normalize_md5(value: &str) -> Result<String, LiveAttachmentError> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(LiveAttachmentError::InvalidArgument(
            "--md5 must contain exactly 32 hexadecimal characters".into(),
        ));
    }
    Ok(value.to_ascii_lowercase())
}

fn validate_conversation(value: &str) -> Result<(), LiveAttachmentError> {
    if value.is_empty() || value.len() > 4096 || value.contains('\0') {
        return Err(LiveAttachmentError::InvalidArgument(
            "conversation identifier is empty or outside safe limits".into(),
        ));
    }
    Ok(())
}

fn validate_attachment_id(value: &str) -> Result<(), LiveAttachmentError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(LiveAttachmentError::InvalidArgument(
            "attachment identity is malformed".into(),
        ));
    }
    Ok(())
}

fn candidate_identity(
    conversation: &str,
    source_md5: &str,
    relative_path: &Path,
    metadata: &fs::Metadata,
    kind: AttachmentKind,
    message_id: Option<&str>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(if message_id.is_some() {
        b"greenbubbles-live-message-attachment-v1\0".as_slice()
    } else {
        b"greenbubbles-live-attachment-v1\0".as_slice()
    });
    digest.update(kind.name().as_bytes());
    digest.update([0]);
    digest.update(conversation.as_bytes());
    digest.update([0]);
    if let Some(message_id) = message_id {
        digest.update(message_id.as_bytes());
        digest.update([0]);
    }
    digest.update(source_md5.as_bytes());
    digest.update([0]);
    digest.update(relative_path.as_os_str().as_encoded_bytes());
    digest.update(metadata.dev().to_le_bytes());
    digest.update(metadata.ino().to_le_bytes());
    digest.update(metadata.len().to_le_bytes());
    digest.update(metadata.mtime().to_le_bytes());
    digest.update(metadata.mtime_nsec().to_le_bytes());
    hex::encode(digest.finalize())
}

fn preferred_candidate<'a>(candidates: &'a [Candidate], source_md5: &str) -> Option<&'a Candidate> {
    let high_definition = format!("{source_md5}_h.dat");
    let exact = format!("{source_md5}.dat");
    candidates
        .iter()
        .find(|candidate| {
            candidate_file_path(candidate)
                .file_name()
                .is_some_and(|name| name == std::ffi::OsStr::new(&high_definition))
        })
        .or_else(|| {
            candidates.iter().find(|candidate| {
                candidate_file_path(candidate)
                    .file_name()
                    .is_some_and(|name| name == std::ffi::OsStr::new(&exact))
            })
        })
        .or_else(|| candidates.first())
}

fn preferred_message_candidate(
    candidates: &[Candidate],
    kind: AttachmentKind,
) -> Option<&Candidate> {
    match kind {
        AttachmentKind::Image => candidates
            .iter()
            .find(|candidate| {
                candidate_file_path(candidate)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains("_h."))
            })
            .or_else(|| {
                candidates.iter().find(|candidate| {
                    candidate_file_path(candidate)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| !name.contains("_t."))
                })
            })
            .or_else(|| candidates.first()),
        AttachmentKind::Voice | AttachmentKind::Video | AttachmentKind::Document => {
            candidates.first()
        }
    }
}

fn detect_source_format(prefix: &[u8], extension: Option<&std::ffi::OsStr>) -> String {
    if prefix.starts_with(b"\x07\x08V1\x08\x07") {
        return "wechat-dat-v1".into();
    }
    if prefix.starts_with(b"\x07\x08V2\x08\x07") {
        return "wechat-dat-v2".into();
    }
    if prefix.starts_with(b"\x02#!SILK_V3") || prefix.starts_with(b"#!SILK_V3") {
        return "silk".into();
    }
    if prefix.starts_with(&[0xff, 0xd8, 0xff]) {
        return "jpg".into();
    }
    if prefix.starts_with(&[0x89, b'P', b'N', b'G']) {
        return "png".into();
    }
    if prefix.starts_with(b"GIF8") {
        return "gif".into();
    }
    if prefix.len() >= 12 && prefix.starts_with(b"RIFF") && &prefix[8..12] == b"WEBP" {
        return "webp".into();
    }
    if prefix.len() >= 8 && &prefix[4..8] == b"ftyp" {
        return "mp4".into();
    }
    if prefix.starts_with(b"%PDF-") {
        return "pdf".into();
    }
    if prefix.starts_with(b"PK\x03\x04") {
        return "zip".into();
    }
    if prefix.starts_with(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]) {
        return "ole-compound-document".into();
    }
    extension
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
        .unwrap_or_else(|| "binary".into())
}

fn candidate_file_path(candidate: &Candidate) -> &Path {
    match &candidate.source {
        CandidateSource::File { path, .. } => path,
        CandidateSource::Voice { .. } => Path::new(""),
    }
}

fn candidate_relative_path(candidate: &Candidate) -> &Path {
    match &candidate.source {
        CandidateSource::File { relative_path, .. } => relative_path,
        CandidateSource::Voice { .. } => Path::new(""),
    }
}

impl FileVersion {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            byte_count: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        }
    }

    fn matches(self, metadata: &fs::Metadata) -> bool {
        self.device == metadata.dev()
            && self.inode == metadata.ino()
            && self.byte_count == metadata.len()
            && self.modified_seconds == metadata.mtime()
            && self.modified_nanoseconds == metadata.mtime_nsec()
    }
}

fn dat_format_name(format: Option<wx_media::DatFormat>) -> &'static str {
    match format {
        Some(wx_media::DatFormat::V1) => "v1FixedKeyAes",
        Some(wx_media::DatFormat::V2) => "v2AccountKeyAes",
        Some(wx_media::DatFormat::Xor) | None => "legacyXorOrUnknown",
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use rusqlite::{params, Connection};

    use super::*;
    use crate::live_query::{list_messages, QueryDatabaseAccess};

    #[test]
    fn inspects_and_materializes_one_xor_image_without_releasing_source_path() {
        let fixture = tempfile::tempdir().unwrap();
        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let account = fixture.path().join("account");
        let conversation = "wxid_friend";
        let md5 = "0123456789abcdef0123456789abcdef";
        let conversation_hash = format!("{:x}", md5::compute(conversation.as_bytes()));
        let image_directory = account
            .join("msg/attach")
            .join(conversation_hash)
            .join("2026-08")
            .join("Img");
        fs::create_dir_all(&image_directory).unwrap();
        let decoded = b"\xff\xd8\xffsynthetic-jpeg";
        let key = 0x5Au8;
        let encrypted = decoded.iter().map(|byte| byte ^ key).collect::<Vec<_>>();
        fs::write(image_directory.join(format!("{md5}_h.dat")), encrypted).unwrap();

        let inspection = inspect_image_attachment(&account, conversation, md5).unwrap();
        assert_eq!(inspection.availability, "downloaded");
        assert_eq!(inspection.candidate_count, 1);
        assert!(!inspection.source_path_released);

        let output = fixture.path().join("decoded.jpg");
        let result = materialize_image_attachment(
            &account,
            conversation,
            md5,
            inspection.preferred_attachment_id.as_deref().unwrap(),
            &output,
        )
        .unwrap();
        assert_eq!(result.decoded_format, "jpg");
        assert_eq!(fs::read(output).unwrap(), decoded);
        assert!(!result.source_path_released);
        assert!(!result.output_path_released);
    }

    #[test]
    fn message_bound_voice_video_and_document_materialize_exactly_one_artifact() {
        let fixture = message_attachment_fixture();
        let source = LiveQuerySource::open(
            &fixture.account.join("db_storage"),
            QueryDatabaseAccess::Decrypted,
        )
        .unwrap();
        let page = list_messages(&source, &fixture.conversation, 10, None).unwrap();
        let message_id = |message_type: u32, message_subtype: u32| {
            page.items
                .iter()
                .find(|item| {
                    item.message_type == message_type && item.message_subtype == message_subtype
                })
                .unwrap()
                .id
                .clone()
        };

        let cases = [
            (
                AttachmentKind::Voice,
                message_id(34, 0),
                fixture.voice.clone(),
                "silk",
            ),
            (
                AttachmentKind::Video,
                message_id(43, 0),
                fixture.video.clone(),
                "mp4",
            ),
            (
                AttachmentKind::Document,
                message_id(49, 6),
                fixture.document.clone(),
                "pdf",
            ),
        ];
        for (kind, message_id, expected, format) in cases {
            let inspection = inspect_message_attachment(
                Some(&fixture.account),
                &source,
                &fixture.conversation,
                &message_id,
                kind,
            )
            .unwrap();
            assert_eq!(inspection.kind, kind.name());
            assert_eq!(inspection.candidate_count, 1);
            assert_eq!(inspection.preferred_source_format.as_deref(), Some(format));
            assert!(!inspection.source_path_released);
            let output = fixture.output.join(format!("materialized-{}", kind.name()));
            let materialized = materialize_message_attachment(
                Some(&fixture.account),
                &source,
                &fixture.conversation,
                &message_id,
                kind,
                inspection.preferred_attachment_id.as_deref().unwrap(),
                &output,
            )
            .unwrap();
            assert_eq!(materialized.kind, kind.name());
            assert_eq!(materialized.decoded_format, format);
            assert_eq!(
                materialized.decoded_sha256,
                hex::encode(Sha256::digest(&expected))
            );
            assert_eq!(fs::read(&output).unwrap(), expected);
            assert_eq!(fs::metadata(&output).unwrap().mode() & 0o077, 0);
            assert!(!materialized.source_path_released);
            assert!(!materialized.output_path_released);
        }
    }

    #[test]
    fn message_attachment_identity_is_bound_to_source_conversation_message_and_kind() {
        let fixture = message_attachment_fixture();
        let source = LiveQuerySource::open(
            &fixture.account.join("db_storage"),
            QueryDatabaseAccess::Decrypted,
        )
        .unwrap();
        let page = list_messages(&source, &fixture.conversation, 10, None).unwrap();
        let video = page
            .items
            .iter()
            .find(|item| item.message_type == 43)
            .unwrap();
        let voice = page
            .items
            .iter()
            .find(|item| item.message_type == 34)
            .unwrap();
        let inspection = inspect_message_attachment(
            Some(&fixture.account),
            &source,
            &fixture.conversation,
            &video.id,
            AttachmentKind::Video,
        )
        .unwrap();
        let attachment_id = inspection.preferred_attachment_id.unwrap();

        assert!(inspect_message_attachment(
            Some(&fixture.account),
            &source,
            &fixture.conversation,
            &video.id,
            AttachmentKind::Document,
        )
        .is_err());
        assert!(materialize_message_attachment(
            Some(&fixture.account),
            &source,
            &fixture.conversation,
            &voice.id,
            AttachmentKind::Voice,
            &attachment_id,
            &fixture.output.join("wrong-message"),
        )
        .is_err());
        assert!(inspect_message_attachment(
            Some(&fixture.account),
            &source,
            "wxid_other",
            &video.id,
            AttachmentKind::Video,
        )
        .is_err());
        assert!(!fixture.output.join("wrong-message").exists());
    }

    struct MessageAttachmentFixture {
        _directory: tempfile::TempDir,
        account: PathBuf,
        output: PathBuf,
        conversation: String,
        voice: Vec<u8>,
        video: Vec<u8>,
        document: Vec<u8>,
    }

    fn message_attachment_fixture() -> MessageAttachmentFixture {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let account = directory.path().join("account");
        let database_root = account.join("db_storage");
        let output = directory.path().join("output");
        for path in [
            database_root.join("contact"),
            database_root.join("session"),
            database_root.join("message"),
            database_root.join("media"),
            database_root.join("hardlink"),
            output.clone(),
        ] {
            fs::create_dir_all(&path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let conversation = "wxid_media_friend".to_string();
        Connection::open(database_root.join("contact/contact.db"))
            .unwrap()
            .execute_batch(
                "CREATE TABLE contact(username TEXT PRIMARY KEY, remark TEXT, nick_name TEXT, alias TEXT);
                 INSERT INTO contact VALUES ('wxid_sender', 'Sender', '', '');",
            )
            .unwrap();
        Connection::open(database_root.join("session/session.db"))
            .unwrap()
            .execute_batch(
                "CREATE TABLE SessionTable(username TEXT, sort_timestamp INTEGER, summary BLOB);
                 INSERT INTO SessionTable VALUES ('wxid_media_friend', 1, 'media');",
            )
            .unwrap();

        let message_table = format!("Msg_{:x}", md5::compute(conversation.as_bytes()));
        let message = Connection::open(database_root.join("message/message_0.db")).unwrap();
        message
            .execute_batch(&format!(
                "CREATE TABLE Name2Id(user_name TEXT);
                 INSERT INTO Name2Id(rowid, user_name) VALUES (1, 'wxid_sender');
                 CREATE TABLE [{message_table}](
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
        let video_md5 = "11111111111111111111111111111111";
        let document_md5 = "22222222222222222222222222222222";
        let video_packed = wx_db::encode_packed_info_for_test(None, Some(video_md5));
        let document_xml = format!(
            "<msg><appmsg><title>report.pdf</title><fileext>pdf</fileext><totallen>24</totallen><md5>{document_md5}</md5></appmsg></msg>"
        );
        for (server_id, sort_sequence, local_type, content, packed) in [
            (3002, 300, 34_i64, Vec::new(), Vec::new()),
            (3003, 200, 43_i64, Vec::new(), video_packed),
            (
                3004,
                100,
                ((6_i64) << 32) | 49,
                document_xml.into_bytes(),
                Vec::new(),
            ),
        ] {
            message
                .execute(
                    &format!(
                        "INSERT INTO [{message_table}](server_id, sort_seq, local_type, real_sender_id, create_time, status, message_content, packed_info_data, WCDB_CT_message_content) VALUES (?1, ?2, ?3, 1, ?2, 0, ?4, ?5, 0)"
                    ),
                    params![server_id, sort_sequence, local_type, content, packed],
                )
                .unwrap();
        }
        drop(message);

        let voice = b"\x02#!SILK_V3synthetic-lossless-voice".to_vec();
        let media = Connection::open(database_root.join("media/media_0.db")).unwrap();
        media
            .execute_batch("CREATE TABLE VoiceInfo(svr_id INTEGER, voice_data BLOB);")
            .unwrap();
        media
            .execute(
                "INSERT INTO VoiceInfo VALUES (?1, ?2)",
                params![3002_i64, &voice],
            )
            .unwrap();
        drop(media);

        let hardlink = Connection::open(database_root.join("hardlink/hardlink.db")).unwrap();
        hardlink
            .execute_batch(
                "CREATE TABLE dir2id(rowid INTEGER PRIMARY KEY, username TEXT);
                 INSERT INTO dir2id VALUES (1, 'wxid_media_friend');
                 INSERT INTO dir2id VALUES (2, '2026-08');
                 CREATE TABLE video_hardlink_info_v3(md5 TEXT, file_name TEXT, file_size INTEGER, modify_time INTEGER, dir1 INTEGER, dir2 INTEGER);
                 CREATE TABLE file_hardlink_info_v3(md5 TEXT, file_name TEXT, file_size INTEGER, modify_time INTEGER, dir1 INTEGER, dir2 INTEGER);",
            )
            .unwrap();
        hardlink
            .execute(
                "INSERT INTO video_hardlink_info_v3 VALUES (?1, 'custom-video.mp4', 24, 1, 1, 2)",
                [video_md5],
            )
            .unwrap();
        hardlink
            .execute(
                "INSERT INTO file_hardlink_info_v3 VALUES (?1, 'report.pdf', 24, 1, 1, 2)",
                [document_md5],
            )
            .unwrap();
        drop(hardlink);

        let video = b"\x00\x00\x00\x18ftypmp42synthetic-video".to_vec();
        let document = b"%PDF-1.7 synthetic report".to_vec();
        let video_directory = account
            .join("msg/attach")
            .join(&conversation)
            .join("2026-08")
            .join("Video");
        let document_directory = account.join("msg/file").join(&conversation).join("2026-08");
        fs::create_dir_all(&video_directory).unwrap();
        fs::create_dir_all(&document_directory).unwrap();
        fs::write(video_directory.join("custom-video.mp4"), &video).unwrap();
        fs::write(document_directory.join("report.pdf"), &document).unwrap();

        MessageAttachmentFixture {
            _directory: directory,
            account,
            output,
            conversation,
            voice,
            video,
            document,
        }
    }
}
