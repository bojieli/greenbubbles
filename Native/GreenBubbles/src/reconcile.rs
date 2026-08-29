use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::archive::{
    ensure_private_directory, ensure_private_regular_file, load_policy, load_report,
};
use crate::RestoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReconciledEventKind {
    Added,
    Changed,
    Removed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciledMessageEvent {
    pub event_id: String,
    pub kind: ReconciledEventKind,
    pub canonical_id: String,
    pub conversation_id: String,
    pub previous_ordinal: Option<u64>,
    pub current_ordinal: Option<u64>,
    pub previous_record_sha256: Option<String>,
    pub current_record_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationReport {
    pub format_version: u32,
    pub account_id: String,
    pub previous_source_fingerprint: String,
    pub current_source_fingerprint: String,
    pub enabled_conversation_count: u64,
    pub previous_message_count: u64,
    pub current_message_count: u64,
    pub added_count: u64,
    pub changed_count: u64,
    pub removed_count: u64,
    pub duplicate_event_count: u64,
    pub events_path: String,
}

#[derive(Debug, Clone)]
struct RecordFingerprint {
    conversation_id: String,
    ordinal: u64,
    sha256: String,
}

pub fn reconcile_archives(
    previous_archive: &Path,
    current_archive: &Path,
    policy_path: &Path,
    events_path: &Path,
) -> Result<ReconciliationReport, RestoreError> {
    ensure_private_directory(previous_archive)?;
    ensure_private_directory(current_archive)?;
    let previous_report = load_report(previous_archive)?;
    let current_report = load_report(current_archive)?;
    let policy = load_policy(policy_path)?;
    if previous_report.account_id != current_report.account_id
        || previous_report.account_id != policy.account_id
    {
        return Err(RestoreError::Integrity(
            "reconciliation inputs do not belong to the same account".to_string(),
        ));
    }
    let previous = load_message_fingerprints(previous_archive, &policy.enabled_conversation_ids)?;
    let current = load_message_fingerprints(current_archive, &policy.enabled_conversation_ids)?;
    let mut events = Vec::new();
    for (canonical_id, record) in &current {
        match previous.get(canonical_id) {
            None => events.push(event(
                ReconciledEventKind::Added,
                canonical_id,
                None,
                Some(record),
                &previous_report.source_fingerprint,
                &current_report.source_fingerprint,
            )),
            Some(previous_record) if previous_record.sha256 != record.sha256 => events.push(event(
                ReconciledEventKind::Changed,
                canonical_id,
                Some(previous_record),
                Some(record),
                &previous_report.source_fingerprint,
                &current_report.source_fingerprint,
            )),
            Some(_) => {}
        }
    }
    for (canonical_id, record) in &previous {
        if !current.contains_key(canonical_id) {
            events.push(event(
                ReconciledEventKind::Removed,
                canonical_id,
                Some(record),
                None,
                &previous_report.source_fingerprint,
                &current_report.source_fingerprint,
            ));
        }
    }
    events.sort_by(|left, right| {
        (
            &left.conversation_id,
            left.current_ordinal.or(left.previous_ordinal),
            event_kind_rank(left.kind),
            &left.canonical_id,
        )
            .cmp(&(
                &right.conversation_id,
                right.current_ordinal.or(right.previous_ordinal),
                event_kind_rank(right.kind),
                &right.canonical_id,
            ))
    });
    let events_parent = events_path
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath(events_path.display().to_string()))?;
    ensure_private_directory(events_parent)?;
    let mut event_ids = HashSet::new();
    let mut duplicate_event_count = 0_u64;
    let mut writer = owner_only_writer(events_path)?;
    for event in &events {
        if !event_ids.insert(event.event_id.clone()) {
            duplicate_event_count += 1;
            continue;
        }
        serde_json::to_writer(&mut writer, event)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    let added_count = events
        .iter()
        .filter(|event| event.kind == ReconciledEventKind::Added)
        .count() as u64;
    let changed_count = events
        .iter()
        .filter(|event| event.kind == ReconciledEventKind::Changed)
        .count() as u64;
    let removed_count = events
        .iter()
        .filter(|event| event.kind == ReconciledEventKind::Removed)
        .count() as u64;
    Ok(ReconciliationReport {
        format_version: 1,
        account_id: previous_report.account_id,
        previous_source_fingerprint: previous_report.source_fingerprint,
        current_source_fingerprint: current_report.source_fingerprint,
        enabled_conversation_count: policy.enabled_conversation_ids.len() as u64,
        previous_message_count: previous.len() as u64,
        current_message_count: current.len() as u64,
        added_count,
        changed_count,
        removed_count,
        duplicate_event_count,
        events_path: events_path.display().to_string(),
    })
}

fn load_message_fingerprints(
    archive: &Path,
    enabled: &std::collections::BTreeSet<String>,
) -> Result<BTreeMap<String, RecordFingerprint>, RestoreError> {
    let path = archive.join("messages.ndjson");
    ensure_private_regular_file(&path)?;
    let reader = BufReader::new(File::open(path)?);
    let mut result = BTreeMap::new();
    for line in reader.lines() {
        let line = line?;
        let value: serde_json::Value = serde_json::from_str(&line)?;
        let canonical_id = required_string(&value, "canonicalId")?;
        let conversation_id = required_string(&value, "conversationId")?;
        if !enabled.contains(&conversation_id) {
            continue;
        }
        let ordinal = value
            .get("conversationOrdinal")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                RestoreError::Integrity(
                    "message archive record is missing its conversation ordinal".to_string(),
                )
            })?;
        let record = RecordFingerprint {
            conversation_id,
            ordinal,
            sha256: hex::encode(Sha256::digest(serde_json::to_vec(&value)?)),
        };
        if result.insert(canonical_id, record).is_some() {
            return Err(RestoreError::Integrity(
                "duplicate canonical message identity in reconciliation input".to_string(),
            ));
        }
    }
    Ok(result)
}

fn event(
    kind: ReconciledEventKind,
    canonical_id: &str,
    previous: Option<&RecordFingerprint>,
    current: Option<&RecordFingerprint>,
    previous_fingerprint: &str,
    current_fingerprint: &str,
) -> ReconciledMessageEvent {
    let conversation_id = current
        .or(previous)
        .expect("a reconciliation event always has one side")
        .conversation_id
        .clone();
    let identity = format!("{previous_fingerprint}:{current_fingerprint}:{kind:?}:{canonical_id}");
    ReconciledMessageEvent {
        event_id: hex::encode(Sha256::digest(identity.as_bytes())),
        kind,
        canonical_id: canonical_id.to_string(),
        conversation_id,
        previous_ordinal: previous.map(|record| record.ordinal),
        current_ordinal: current.map(|record| record.ordinal),
        previous_record_sha256: previous.map(|record| record.sha256.clone()),
        current_record_sha256: current.map(|record| record.sha256.clone()),
    }
}

fn required_string(value: &serde_json::Value, field: &str) -> Result<String, RestoreError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            RestoreError::Integrity(format!("message archive record is missing {field}"))
        })
}

fn event_kind_rank(kind: ReconciledEventKind) -> u8 {
    match kind {
        ReconciledEventKind::Added => 0,
        ReconciledEventKind::Changed => 1,
        ReconciledEventKind::Removed => 2,
    }
}

fn owner_only_writer(path: &Path) -> Result<BufWriter<File>, RestoreError> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    Ok(BufWriter::new(file))
}
