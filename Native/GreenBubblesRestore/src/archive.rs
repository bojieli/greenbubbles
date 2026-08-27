use std::collections::{BTreeSet, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::{CanonicalMessage, RestorationCompletion, RestorationReport, RestoreError};

const MAX_POLICY_PAGE_SIZE: usize = 1_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationReadPolicy {
    pub format_version: u32,
    pub source_fingerprint: String,
    pub enabled_conversation_ids: BTreeSet<String>,
    pub maximum_page_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationCursor {
    pub format_version: u32,
    pub source_fingerprint: String,
    pub conversation_id: String,
    pub after_ordinal: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPage {
    pub conversation_id: String,
    pub items: Vec<CanonicalMessage>,
    pub next_cursor: Option<String>,
    pub restoration_completion: RestorationCompletion,
}

pub fn create_conversation_policy(
    archive_directory: &Path,
    policy_path: &Path,
    enabled_conversation_ids: BTreeSet<String>,
    maximum_page_size: usize,
) -> Result<ConversationReadPolicy, RestoreError> {
    if enabled_conversation_ids.is_empty() {
        return Err(RestoreError::Integrity(
            "at least one conversation must be explicitly enabled".to_string(),
        ));
    }
    let report = load_report(archive_directory)?;
    let known = load_conversation_ids(archive_directory)?;
    if let Some(unknown) = enabled_conversation_ids
        .iter()
        .find(|identifier| !known.contains(*identifier))
    {
        return Err(RestoreError::Integrity(format!(
            "conversation is not present in the archive: {unknown}"
        )));
    }
    let policy = ConversationReadPolicy {
        format_version: 1,
        source_fingerprint: report.source_fingerprint,
        enabled_conversation_ids,
        maximum_page_size: maximum_page_size.clamp(1, MAX_POLICY_PAGE_SIZE),
    };
    write_owner_only_json(policy_path, &policy)?;
    Ok(policy)
}

pub fn read_conversation_page(
    archive_directory: &Path,
    policy_path: &Path,
    conversation_id: &str,
    cursor: Option<&str>,
    requested_limit: usize,
) -> Result<ConversationPage, RestoreError> {
    ensure_private_regular_file(policy_path)?;
    let policy: ConversationReadPolicy = serde_json::from_slice(&fs::read(policy_path)?)?;
    if policy.format_version != 1 {
        return Err(RestoreError::Integrity(
            "unsupported conversation policy version".to_string(),
        ));
    }
    if !policy.enabled_conversation_ids.contains(conversation_id) {
        return Err(RestoreError::Integrity(
            "conversation is not enabled by the local read policy".to_string(),
        ));
    }
    let report = load_report(archive_directory)?;
    if report.source_fingerprint != policy.source_fingerprint {
        return Err(RestoreError::Integrity(
            "conversation policy belongs to a different restoration archive".to_string(),
        ));
    }
    let cursor = cursor.map(decode_cursor).transpose()?;
    if let Some(cursor) = &cursor {
        if cursor.format_version != 1
            || cursor.source_fingerprint != report.source_fingerprint
            || cursor.conversation_id != conversation_id
        {
            return Err(RestoreError::Integrity(
                "cursor belongs to a different archive or conversation".to_string(),
            ));
        }
    }
    let after = cursor.as_ref().map(|value| value.after_ordinal);
    let limit = requested_limit.clamp(1, policy.maximum_page_size);
    let message_path = archive_directory.join("messages.ndjson");
    ensure_private_regular_file(&message_path)?;
    let reader = BufReader::new(File::open(&message_path)?);
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    let mut has_more = false;
    for line in reader.lines() {
        let line = line?;
        let message: CanonicalMessage = serde_json::from_str(&line)?;
        if !seen.insert(message.canonical_id.clone()) {
            return Err(RestoreError::Integrity(
                "duplicate canonical identity encountered while paging".to_string(),
            ));
        }
        if message.conversation_id != conversation_id
            || after.is_some_and(|ordinal| message.conversation_ordinal <= ordinal)
        {
            continue;
        }
        if items.len() == limit {
            has_more = true;
            break;
        }
        items.push(message);
    }
    let next_cursor = if has_more {
        items.last().map(|message| {
            encode_cursor(&ConversationCursor {
                format_version: 1,
                source_fingerprint: report.source_fingerprint.clone(),
                conversation_id: conversation_id.to_string(),
                after_ordinal: message.conversation_ordinal,
            })
        })
    } else {
        None
    };
    Ok(ConversationPage {
        conversation_id: conversation_id.to_string(),
        items,
        next_cursor,
        restoration_completion: report.completion,
    })
}

fn load_report(archive_directory: &Path) -> Result<RestorationReport, RestoreError> {
    ensure_private_directory(archive_directory)?;
    let path = archive_directory.join("report.json");
    ensure_private_regular_file(&path)?;
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn load_conversation_ids(archive_directory: &Path) -> Result<BTreeSet<String>, RestoreError> {
    let path = archive_directory.join("conversations.ndjson");
    ensure_private_regular_file(&path)?;
    let reader = BufReader::new(File::open(path)?);
    let mut result = BTreeSet::new();
    for line in reader.lines() {
        let value: serde_json::Value = serde_json::from_str(&line?)?;
        let identifier = value
            .get("conversationId")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                RestoreError::Integrity(
                    "conversation archive record is missing its identifier".to_string(),
                )
            })?;
        result.insert(identifier.to_string());
    }
    Ok(result)
}

fn encode_cursor(cursor: &ConversationCursor) -> String {
    let bytes = serde_json::to_vec(cursor).expect("cursor serialization cannot fail");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn decode_cursor(value: &str) -> Result<ConversationCursor, RestoreError> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| RestoreError::Integrity("cursor is not valid base64url".to_string()))?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn ensure_private_directory(path: &Path) -> Result<(), RestoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(RestoreError::Integrity(
            "restoration archive must be an owner-only, non-symlink directory".to_string(),
        ));
    }
    Ok(())
}

fn ensure_private_regular_file(path: &Path) -> Result<(), RestoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(RestoreError::Integrity(format!(
            "private archive input is not an owner-only regular file: {}",
            path.file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("unknown")
        )));
    }
    Ok(())
}

fn write_owner_only_json(path: &Path, value: &impl Serialize) -> Result<(), RestoreError> {
    let parent: PathBuf = path
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath(path.display().to_string()))?
        .to_path_buf();
    ensure_private_directory(&parent)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}
