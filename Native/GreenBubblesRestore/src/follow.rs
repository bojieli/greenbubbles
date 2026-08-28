use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::archive::{ensure_private_directory, ensure_private_regular_file, load_report};
use crate::audit::audit_archive;
use crate::replica::{
    bootstrap_replica, replica_matches_authoritative_archive, replica_status, synchronize_replica,
};
use crate::{ReplicaKey, RestoreError};

const HANDOFF_FORMAT_VERSION: u32 = 3;
const FOLLOW_STATE_FORMAT_VERSION: u32 = 1;
const PUBLICATION_HISTORY_FORMAT_VERSION: u32 = 1;
const MAX_CONTROL_FILE_BYTES: u64 = 64 * 1_024;
const MAX_PUBLICATION_HISTORY_BYTES: u64 = 4 * 1_024 * 1_024;
const MAX_PUBLICATION_HISTORY_ENTRIES: usize = 4_096;
const MAX_ARCHIVE_SEAL_FILE_COUNT: usize = 1_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReplicaArchiveHandoff {
    pub format_version: u32,
    pub generation: u64,
    pub archive_directory: String,
    pub source_fingerprint: String,
    pub report_sha256: String,
    pub archive_seal_sha256: String,
    pub archive_file_count: u64,
    pub archive_byte_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at_unix_nanoseconds: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReplicaFollowState {
    format_version: u32,
    replica_id: String,
    generation: u64,
    handoff_sha256: String,
    source_fingerprint: String,
    checkpoint_revision: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReplicaFollowOutcome {
    Bootstrapped,
    Synchronized,
    AlreadyApplied,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReplicaHandoffReceipt {
    pub format_version: u32,
    pub privacy_safe_summary: bool,
    pub generation: u64,
    pub handoff_written: bool,
    pub authoritative_archive_required: bool,
    pub replica_eligible_archive_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReplicaPublicationHistory {
    format_version: u32,
    entries: Vec<ReplicaPublicationHistoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReplicaPublicationHistoryEntry {
    handoff: ReplicaArchiveHandoff,
    handoff_sha256: String,
    handoff_value_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quarantine_directory: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReplicaArchiveQuarantineReport {
    pub format_version: u32,
    pub privacy_safe_summary: bool,
    pub current_generation: u64,
    pub protected_publication_count: u64,
    pub retained_archive_count: u64,
    pub newly_quarantined_archive_count: u64,
    pub already_quarantined_archive_count: u64,
    pub shared_with_protected_generation_count: u64,
    pub recoverable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReplicaArchiveRestoreReport {
    pub format_version: u32,
    pub privacy_safe_summary: bool,
    pub requested_generation: u64,
    pub restored_archive_count: u64,
    pub restored_publication_count: u64,
    pub archive_verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReplicaFollowReport {
    pub format_version: u32,
    pub privacy_safe_summary: bool,
    pub generation: u64,
    pub outcome: ReplicaFollowOutcome,
    pub idempotent: bool,
    pub source_advanced: bool,
    pub added_count: u64,
    pub changed_count: u64,
    pub removed_count: u64,
    pub restoration_complete: bool,
    pub apply_duration_milliseconds: u64,
    pub publication_to_checkpoint_milliseconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReplicaFollowerHealth {
    Uninitialized,
    Pending,
    Current,
    StateRecoveryRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReplicaFollowerStatus {
    pub format_version: u32,
    pub privacy_safe_summary: bool,
    pub health: ReplicaFollowerHealth,
    pub published_generation: u64,
    pub applied_generation: Option<u64>,
    pub generation_lag: u64,
    pub state_present: bool,
    pub replica_present: bool,
    pub replica_initialized: bool,
    pub checkpoint_age_seconds: Option<u64>,
    pub published_generation_age_seconds: Option<u64>,
    pub publication_to_checkpoint_milliseconds: Option<u64>,
    pub restoration_complete: Option<bool>,
    pub archive_validation_deferred_until_application: bool,
}

pub fn publish_replica_handoff(
    archive_directory: &Path,
    handoff_path: &Path,
    generation: u64,
) -> Result<ReplicaHandoffReceipt, RestoreError> {
    if generation == 0 {
        return Err(RestoreError::Integrity(
            "replica handoff generation must be positive".to_string(),
        ));
    }
    let canonical_archive = canonical_publish_archive(archive_directory, handoff_path)?;
    let _lock = ControlLock::acquire(handoff_path, "handoff")?;
    publish_replica_handoff_locked(&canonical_archive, handoff_path, generation)
}

pub(crate) enum PublicationPredecessor {
    Absent,
    Current {
        handoff_sha256: String,
        archive_directory: PathBuf,
    },
}

pub(crate) fn capture_publication_predecessor(
    handoff_path: &Path,
    expected_previous_archive: Option<&Path>,
) -> Result<PublicationPredecessor, RestoreError> {
    let canonical_previous = expected_previous_archive
        .map(|previous| canonical_publish_archive(previous, handoff_path))
        .transpose()?;
    let _lock = ControlLock::acquire(handoff_path, "handoff")?;
    match (path_entry_exists(handoff_path)?, canonical_previous) {
        (false, None) => Ok(PublicationPredecessor::Absent),
        (true, Some(previous)) => {
            let current = load_handoff(handoff_path)?;
            verify_handoff_archive(&current, &previous)?;
            Ok(PublicationPredecessor::Current {
                handoff_sha256: current.sha256,
                archive_directory: previous,
            })
        }
        (true, None) => Err(RestoreError::Integrity(
            "bootstrap publication cannot replace an existing handoff".to_string(),
        )),
        (false, Some(_)) => Err(RestoreError::Integrity(
            "continuation publication has no current predecessor handoff".to_string(),
        )),
    }
}

pub(crate) fn publish_replica_handoff_next_if_current(
    archive_directory: &Path,
    handoff_path: &Path,
    predecessor: &PublicationPredecessor,
) -> Result<ReplicaHandoffReceipt, RestoreError> {
    let canonical_archive = canonical_publish_archive(archive_directory, handoff_path)?;
    let _lock = ControlLock::acquire(handoff_path, "handoff")?;
    let generation = match predecessor {
        PublicationPredecessor::Absent if !path_entry_exists(handoff_path)? => 1,
        PublicationPredecessor::Current {
            handoff_sha256,
            archive_directory,
        } if path_entry_exists(handoff_path)? => {
            let current = load_handoff(handoff_path)?;
            if current.sha256 != *handoff_sha256 {
                return Err(RestoreError::Integrity(
                    "publication predecessor changed while the next archive was prepared"
                        .to_string(),
                ));
            }
            verify_handoff_archive(&current, archive_directory)?;
            current.value.generation.checked_add(1).ok_or_else(|| {
                RestoreError::Integrity("replica handoff generation overflowed".to_string())
            })?
        }
        _ => {
            return Err(RestoreError::Integrity(
                "publication predecessor changed while the next archive was prepared".to_string(),
            ));
        }
    };
    publish_replica_handoff_locked(&canonical_archive, handoff_path, generation)
}

fn verify_handoff_archive(
    loaded: &LoadedHandoff,
    canonical_archive: &Path,
) -> Result<(), RestoreError> {
    if Path::new(&loaded.value.archive_directory) != canonical_archive {
        return Err(RestoreError::Integrity(
            "publication predecessor is no longer the current handoff archive".to_string(),
        ));
    }
    let report = load_report(canonical_archive)?;
    require_authoritative_handoff_report(&report)?;
    if report.format_version >= 3 {
        audit_archive(canonical_archive)?;
    }
    let report_bytes = read_owner_only_control_file(&canonical_archive.join("report.json"))?;
    let seal = archive_seal(canonical_archive)?;
    if report.source_fingerprint != loaded.value.source_fingerprint
        || hex::encode(Sha256::digest(report_bytes)) != loaded.value.report_sha256
        || seal.sha256 != loaded.value.archive_seal_sha256
        || seal.file_count != loaded.value.archive_file_count
        || seal.byte_count != loaded.value.archive_byte_count
    {
        return Err(RestoreError::Integrity(
            "publication predecessor no longer matches its current handoff".to_string(),
        ));
    }
    Ok(())
}

fn canonical_publish_archive(
    archive_directory: &Path,
    handoff_path: &Path,
) -> Result<PathBuf, RestoreError> {
    ensure_private_directory(archive_directory)?;
    let canonical_archive = fs::canonicalize(archive_directory)?;
    ensure_private_directory(&canonical_archive)?;
    ensure_target_outside_archive(&canonical_archive, handoff_path, "handoff")?;
    Ok(canonical_archive)
}

fn publish_replica_handoff_locked(
    canonical_archive: &Path,
    handoff_path: &Path,
    generation: u64,
) -> Result<ReplicaHandoffReceipt, RestoreError> {
    let prior = if path_entry_exists(handoff_path)? {
        let prior = load_handoff(handoff_path)?;
        if generation <= prior.value.generation {
            return Err(RestoreError::Integrity(
                "replica handoff generation must advance monotonically".to_string(),
            ));
        }
        Some(prior)
    } else {
        None
    };
    let mut publication_history =
        load_or_reconcile_publication_history(handoff_path, prior.as_ref())?;
    let report = load_report(canonical_archive)?;
    require_authoritative_handoff_report(&report)?;
    if report.format_version >= 3 {
        audit_archive(canonical_archive)?;
    }
    let report_path = canonical_archive.join("report.json");
    let report_bytes = read_owner_only_control_file(&report_path)?;
    let seal = archive_seal(canonical_archive)?;
    let handoff = ReplicaArchiveHandoff {
        format_version: HANDOFF_FORMAT_VERSION,
        generation,
        archive_directory: canonical_archive
            .to_str()
            .ok_or_else(|| RestoreError::UnsafePath("archive path is not valid UTF-8".to_string()))?
            .to_string(),
        source_fingerprint: report.source_fingerprint,
        report_sha256: hex::encode(Sha256::digest(report_bytes)),
        archive_seal_sha256: seal.sha256,
        archive_file_count: seal.file_count,
        archive_byte_count: seal.byte_count,
        published_at_unix_nanoseconds: Some(unix_nanoseconds()?.to_string()),
    };
    reject_reused_mutated_archive_path(&publication_history, &handoff)?;
    if publication_history.entries.len() >= MAX_PUBLICATION_HISTORY_ENTRIES {
        return Err(RestoreError::Integrity(
            "replica publication history reached its entry limit".to_string(),
        ));
    }
    let handoff_sha256 = owner_json_sha256(&handoff)?;
    write_atomic_owner_json(handoff_path, &handoff, "handoff")?;
    publication_history
        .entries
        .push(ReplicaPublicationHistoryEntry {
            handoff_value_sha256: owner_json_sha256(&handoff)?,
            handoff,
            handoff_sha256,
            quarantine_directory: None,
        });
    write_publication_history(handoff_path, &publication_history)?;
    Ok(ReplicaHandoffReceipt {
        format_version: 2,
        privacy_safe_summary: true,
        generation,
        handoff_written: true,
        authoritative_archive_required: false,
        replica_eligible_archive_required: true,
    })
}

pub fn quarantine_retired_replica_archives(
    handoff_path: &Path,
    quarantine_directory: &Path,
    retain_publications: usize,
) -> Result<ReplicaArchiveQuarantineReport, RestoreError> {
    if retain_publications < 2 {
        return Err(RestoreError::Integrity(
            "archive retention must protect at least the current and previous publications"
                .to_string(),
        ));
    }
    let _lock = ControlLock::acquire(handoff_path, "handoff")?;
    let current = load_handoff(handoff_path)?;
    let mut history = load_or_reconcile_publication_history(handoff_path, Some(&current))?;
    let quarantine_root = canonical_private_directory(quarantine_directory, "quarantine")?;
    let protected_start = history.entries.len().saturating_sub(retain_publications);
    let protected_paths = history.entries[protected_start..]
        .iter()
        .map(|entry| entry.handoff.archive_directory.clone())
        .collect::<BTreeSet<_>>();
    let groups = publication_path_groups(&history)?;

    for path in &protected_paths {
        let indices = groups.get(path).ok_or_else(|| {
            RestoreError::Integrity("protected publication disappeared from history".to_string())
        })?;
        require_group_retained_and_verified(&history, indices)?;
        reject_nested_paths(Path::new(path), &quarantine_root)?;
    }

    let mut newly_quarantined = 0_u64;
    let mut already_quarantined = 0_u64;
    let mut shared_with_protected = 0_u64;
    let mut eligible = Vec::new();
    for (archive_path, indices) in &groups {
        let contains_retired_publication = indices.iter().any(|index| *index < protected_start);
        if !contains_retired_publication {
            continue;
        }
        if protected_paths.contains(archive_path) {
            shared_with_protected = shared_with_protected.saturating_add(1);
            continue;
        }
        let quarantine_path = group_quarantine_path(&history, indices, &quarantine_root)?;
        reject_nested_paths(Path::new(archive_path), &quarantine_root)?;
        match group_location(&history, indices)? {
            GroupLocation::Quarantined(recorded) => {
                if recorded != quarantine_path {
                    return Err(RestoreError::Integrity(
                        "publication history names an unexpected quarantine location".to_string(),
                    ));
                }
                verify_quarantined_group(&history, indices, &recorded, Path::new(archive_path))?;
                already_quarantined = already_quarantined.saturating_add(1);
            }
            GroupLocation::Retained => {
                let original = Path::new(archive_path);
                match (
                    path_entry_exists(original)?,
                    path_entry_exists(&quarantine_path)?,
                ) {
                    (true, false) => {
                        verify_retained_group(&history, indices, original)?;
                        require_same_filesystem(original, &quarantine_root)?;
                        eligible.push((archive_path.clone(), indices.clone(), quarantine_path));
                    }
                    (false, true) => {
                        verify_quarantined_group(&history, indices, &quarantine_path, original)?;
                        mark_group_quarantined(&mut history, indices, &quarantine_path)?;
                        write_publication_history(handoff_path, &history)?;
                        newly_quarantined = newly_quarantined.saturating_add(1);
                    }
                    (true, true) => {
                        return Err(RestoreError::Integrity(
                            "archive exists in both retained and quarantine locations".to_string(),
                        ));
                    }
                    (false, false) => {
                        return Err(RestoreError::Integrity(
                            "retired archive is missing from both retained and quarantine locations"
                                .to_string(),
                        ));
                    }
                }
            }
        }
    }

    for (archive_path, indices, quarantine_path) in eligible {
        let original = Path::new(&archive_path);
        fs::rename(original, &quarantine_path)?;
        sync_rename_parents(original, &quarantine_path)?;
        verify_quarantined_group(&history, &indices, &quarantine_path, original)?;
        mark_group_quarantined(&mut history, &indices, &quarantine_path)?;
        write_publication_history(handoff_path, &history)?;
        newly_quarantined = newly_quarantined.saturating_add(1);
    }

    let retained_archive_count = publication_path_groups(&history)?
        .values()
        .filter(|indices| {
            matches!(
                group_location(&history, indices),
                Ok(GroupLocation::Retained)
            )
        })
        .count() as u64;
    Ok(ReplicaArchiveQuarantineReport {
        format_version: 1,
        privacy_safe_summary: true,
        current_generation: current.value.generation,
        protected_publication_count: history.entries.len().min(retain_publications) as u64,
        retained_archive_count,
        newly_quarantined_archive_count: newly_quarantined,
        already_quarantined_archive_count: already_quarantined,
        shared_with_protected_generation_count: shared_with_protected,
        recoverable: true,
    })
}

pub fn restore_quarantined_replica_archive(
    handoff_path: &Path,
    quarantine_directory: &Path,
    generation: u64,
) -> Result<ReplicaArchiveRestoreReport, RestoreError> {
    if generation == 0 {
        return Err(RestoreError::Integrity(
            "archive restoration generation must be positive".to_string(),
        ));
    }
    let _lock = ControlLock::acquire(handoff_path, "handoff")?;
    let current = load_handoff(handoff_path)?;
    let mut history = load_or_reconcile_publication_history(handoff_path, Some(&current))?;
    let quarantine_root = canonical_private_directory(quarantine_directory, "quarantine")?;
    let requested_index = history
        .entries
        .iter()
        .position(|entry| entry.handoff.generation == generation)
        .ok_or_else(|| {
            RestoreError::Integrity(
                "requested generation is not present in publication history".to_string(),
            )
        })?;
    let archive_path = history.entries[requested_index]
        .handoff
        .archive_directory
        .clone();
    let groups = publication_path_groups(&history)?;
    let indices = groups.get(&archive_path).ok_or_else(|| {
        RestoreError::Integrity("requested publication path disappeared from history".to_string())
    })?;
    let GroupLocation::Quarantined(recorded_quarantine) = group_location(&history, indices)? else {
        return Err(RestoreError::Integrity(
            "requested generation is not quarantined".to_string(),
        ));
    };
    if recorded_quarantine.parent() != Some(quarantine_root.as_path()) {
        return Err(RestoreError::UnsafePath(
            "requested generation belongs to a different quarantine directory".to_string(),
        ));
    }
    if recorded_quarantine != group_quarantine_path(&history, indices, &quarantine_root)? {
        return Err(RestoreError::Integrity(
            "publication history names an unexpected quarantine location".to_string(),
        ));
    }
    let original = Path::new(&archive_path);
    reject_nested_paths(original, &quarantine_root)?;
    let original_parent = original.parent().ok_or_else(|| {
        RestoreError::UnsafePath("restored archive path has no parent".to_string())
    })?;
    ensure_private_directory(original_parent)?;

    let restored_archive_count = match (
        path_entry_exists(original)?,
        path_entry_exists(&recorded_quarantine)?,
    ) {
        (false, true) => {
            verify_quarantined_group(&history, indices, &recorded_quarantine, original)?;
            require_same_filesystem(&recorded_quarantine, original_parent)?;
            fs::rename(&recorded_quarantine, original)?;
            sync_rename_parents(&recorded_quarantine, original)?;
            if let Err(error) = verify_retained_group(&history, indices, original) {
                if fs::rename(original, &recorded_quarantine).is_ok() {
                    let _ = sync_rename_parents(original, &recorded_quarantine);
                }
                return Err(error);
            }
            1
        }
        (true, false) => {
            verify_retained_group(&history, indices, original)?;
            0
        }
        (true, true) => {
            return Err(RestoreError::Integrity(
                "archive exists in both retained and quarantine locations".to_string(),
            ));
        }
        (false, false) => {
            return Err(RestoreError::Integrity(
                "archive is missing from both retained and quarantine locations".to_string(),
            ));
        }
    };
    for index in indices {
        history.entries[*index].quarantine_directory = None;
    }
    write_publication_history(handoff_path, &history)?;
    Ok(ReplicaArchiveRestoreReport {
        format_version: 1,
        privacy_safe_summary: true,
        requested_generation: generation,
        restored_archive_count,
        restored_publication_count: indices.len() as u64,
        archive_verified: true,
    })
}

pub fn follow_replica_once(
    handoff_path: &Path,
    state_path: &Path,
    replica_path: &Path,
    key: &ReplicaKey,
) -> Result<ReplicaFollowReport, RestoreError> {
    let apply_started = Instant::now();
    let loaded = load_handoff(handoff_path)?;
    let archive_directory = PathBuf::from(&loaded.value.archive_directory);
    if !archive_directory.is_absolute() {
        return Err(RestoreError::UnsafePath(
            "replica handoff archive path must be absolute".to_string(),
        ));
    }
    ensure_private_directory(&archive_directory)?;
    let canonical_archive = fs::canonicalize(&archive_directory)?;
    if canonical_archive != archive_directory {
        return Err(RestoreError::Integrity(
            "replica handoff archive path is not canonical".to_string(),
        ));
    }
    ensure_target_outside_archive(&canonical_archive, handoff_path, "handoff")?;
    ensure_target_outside_archive(&canonical_archive, state_path, "follow state")?;
    ensure_target_outside_archive(&canonical_archive, replica_path, "replica")?;
    let _lock = ControlLock::acquire(state_path, "follow-state")?;
    let locked_handoff = load_handoff(handoff_path)?;
    if locked_handoff.sha256 != loaded.sha256 {
        return Err(RestoreError::Integrity(
            "replica handoff changed while follower state was being locked; retry".to_string(),
        ));
    }
    let report = load_report(&archive_directory)?;
    require_authoritative_handoff_report(&report)?;
    if report.format_version >= 3 {
        audit_archive(&archive_directory)?;
    }
    let report_bytes = read_owner_only_control_file(&archive_directory.join("report.json"))?;
    let seal_before = archive_seal(&archive_directory)?;
    if report.source_fingerprint != loaded.value.source_fingerprint
        || hex::encode(Sha256::digest(report_bytes)) != loaded.value.report_sha256
        || seal_before.sha256 != loaded.value.archive_seal_sha256
        || seal_before.file_count != loaded.value.archive_file_count
        || seal_before.byte_count != loaded.value.archive_byte_count
    {
        return Err(RestoreError::Integrity(
            "replica handoff no longer matches its restoration report".to_string(),
        ));
    }

    let before = replica_status(replica_path, key)?;
    let prior_state = path_entry_exists(state_path)?
        .then(|| load_follow_state(state_path))
        .transpose()?;
    if let Some(state) = prior_state.as_ref() {
        if state.replica_id != before.replica_id {
            return Err(RestoreError::Integrity(
                "replica follow state belongs to a replacement replica".to_string(),
            ));
        }
        if loaded.value.generation < state.generation
            || (loaded.value.generation == state.generation
                && loaded.sha256 != state.handoff_sha256)
        {
            return Err(RestoreError::Integrity(
                "replica handoff is a rollback or generation equivocation".to_string(),
            ));
        }
        if loaded.value.generation == state.generation {
            if before.current_source_fingerprint.as_deref()
                != Some(state.source_fingerprint.as_str())
                || before.checkpoint_revision.as_deref() != Some(state.checkpoint_revision.as_str())
            {
                return Err(RestoreError::Integrity(
                    "replica checkpoint diverged from the applied follow state".to_string(),
                ));
            }
            return Ok(ReplicaFollowReport {
                format_version: 2,
                privacy_safe_summary: true,
                generation: state.generation,
                outcome: ReplicaFollowOutcome::AlreadyApplied,
                idempotent: true,
                source_advanced: false,
                added_count: 0,
                changed_count: 0,
                removed_count: 0,
                restoration_complete: before.restoration_complete.unwrap_or(false),
                apply_duration_milliseconds: elapsed_milliseconds(apply_started),
                publication_to_checkpoint_milliseconds: publication_to_checkpoint_milliseconds(
                    &loaded.value,
                    before.last_checkpoint_unix_nanoseconds,
                ),
            });
        }
    }

    let (outcome, idempotent, added, changed, removed) = if before.account_id.is_none() {
        let bootstrap = bootstrap_replica(&archive_directory, replica_path, key)?;
        (
            ReplicaFollowOutcome::Bootstrapped,
            bootstrap.idempotent,
            bootstrap
                .conversation_count
                .saturating_add(bootstrap.participant_count)
                .saturating_add(bootstrap.message_count)
                .saturating_add(bootstrap.artifact_count)
                .saturating_add(bootstrap.cached_moment_count)
                .saturating_add(bootstrap.cached_moment_interaction_count),
            0,
            0,
        )
    } else if prior_state.is_none() {
        if !replica_matches_authoritative_archive(&archive_directory, replica_path, key)? {
            return Err(RestoreError::Integrity(
                "existing replica cannot be adopted without an exact replica-eligible archive"
                    .to_string(),
            ));
        }
        (ReplicaFollowOutcome::Synchronized, true, 0, 0, 0)
    } else {
        let sync = synchronize_replica(&archive_directory, replica_path, key)?;
        (
            ReplicaFollowOutcome::Synchronized,
            sync.idempotent,
            sync.added_count,
            sync.changed_count,
            sync.removed_count,
        )
    };
    let after = replica_status(replica_path, key)?;
    let seal_after = archive_seal(&archive_directory)?;
    if seal_after.sha256 != loaded.value.archive_seal_sha256
        || seal_after.file_count != loaded.value.archive_file_count
        || seal_after.byte_count != loaded.value.archive_byte_count
    {
        return Err(RestoreError::Integrity(
            "published restoration archive changed during replica application".to_string(),
        ));
    }
    let checkpoint_revision = after.checkpoint_revision.clone().ok_or_else(|| {
        RestoreError::Integrity("replica follow did not commit a checkpoint".to_string())
    })?;
    if after.replica_id != before.replica_id
        || after.current_source_fingerprint.as_deref()
            != Some(loaded.value.source_fingerprint.as_str())
        || after.account_id.as_deref() != Some(report.account_id.as_str())
    {
        return Err(RestoreError::Integrity(
            "replica follow result does not match the published archive".to_string(),
        ));
    }
    let publication_to_checkpoint_milliseconds = publication_to_checkpoint_milliseconds(
        &loaded.value,
        after.last_checkpoint_unix_nanoseconds,
    );
    let state = ReplicaFollowState {
        format_version: FOLLOW_STATE_FORMAT_VERSION,
        replica_id: after.replica_id,
        generation: loaded.value.generation,
        handoff_sha256: loaded.sha256,
        source_fingerprint: loaded.value.source_fingerprint,
        checkpoint_revision,
    };
    write_atomic_owner_json(state_path, &state, "follow-state")?;
    Ok(ReplicaFollowReport {
        format_version: 2,
        privacy_safe_summary: true,
        generation: state.generation,
        outcome,
        idempotent,
        source_advanced: before.current_source_fingerprint.as_deref()
            != Some(state.source_fingerprint.as_str()),
        added_count: added,
        changed_count: changed,
        removed_count: removed,
        restoration_complete: after.restoration_complete.unwrap_or(false),
        apply_duration_milliseconds: elapsed_milliseconds(apply_started),
        publication_to_checkpoint_milliseconds,
    })
}

pub fn replica_follower_status(
    handoff_path: &Path,
    state_path: &Path,
    replica_path: &Path,
    key: &ReplicaKey,
) -> Result<ReplicaFollowerStatus, RestoreError> {
    let loaded = load_handoff(handoff_path)?;
    let archive_directory = PathBuf::from(&loaded.value.archive_directory);
    if !archive_directory.is_absolute() {
        return Err(RestoreError::UnsafePath(
            "replica handoff archive path must be absolute".to_string(),
        ));
    }
    ensure_private_directory(&archive_directory)?;
    let canonical_archive = fs::canonicalize(&archive_directory)?;
    if canonical_archive != archive_directory {
        return Err(RestoreError::Integrity(
            "replica handoff archive path is not canonical".to_string(),
        ));
    }
    ensure_target_outside_archive(&canonical_archive, handoff_path, "handoff")?;
    ensure_target_outside_archive(&canonical_archive, state_path, "follow state")?;
    ensure_target_outside_archive(&canonical_archive, replica_path, "replica")?;
    let _lock = ControlLock::acquire(state_path, "follow-state")?;
    let locked_handoff = load_handoff(handoff_path)?;
    if locked_handoff.sha256 != loaded.sha256 {
        return Err(RestoreError::Integrity(
            "replica handoff changed while follower status was being read; retry".to_string(),
        ));
    }
    let prior_state = path_entry_exists(state_path)?
        .then(|| load_follow_state(state_path))
        .transpose()?;
    if let Some(state) = prior_state.as_ref() {
        if loaded.value.generation < state.generation
            || (loaded.value.generation == state.generation
                && loaded.sha256 != state.handoff_sha256)
        {
            return Err(RestoreError::Integrity(
                "replica handoff is a rollback or generation equivocation".to_string(),
            ));
        }
    }

    let replica_present = path_entry_exists(replica_path)?;
    let replica = replica_present
        .then(|| replica_status(replica_path, key))
        .transpose()?;
    let replica_initialized = replica
        .as_ref()
        .is_some_and(|status| status.account_id.is_some());
    if let Some(state) = prior_state.as_ref() {
        let status = replica.as_ref().ok_or_else(|| {
            RestoreError::Integrity("replica follow state has no corresponding replica".to_string())
        })?;
        if state.replica_id != status.replica_id
            || status.current_source_fingerprint.as_deref()
                != Some(state.source_fingerprint.as_str())
            || status.checkpoint_revision.as_deref() != Some(state.checkpoint_revision.as_str())
        {
            return Err(RestoreError::Integrity(
                "replica checkpoint diverged from the applied follow state".to_string(),
            ));
        }
    }

    let applied_generation = prior_state.as_ref().map(|state| state.generation);
    let generation_lag = loaded
        .value
        .generation
        .saturating_sub(applied_generation.unwrap_or(0));
    let health = match (prior_state.as_ref(), replica_initialized, generation_lag) {
        (Some(_), true, 0) => ReplicaFollowerHealth::Current,
        (Some(_), true, _) => ReplicaFollowerHealth::Pending,
        (None, true, _) => ReplicaFollowerHealth::StateRecoveryRequired,
        (None, false, _) => ReplicaFollowerHealth::Uninitialized,
        (Some(_), false, _) => {
            return Err(RestoreError::Integrity(
                "replica follow state references an uninitialized replica".to_string(),
            ));
        }
    };
    let published_generation_age_seconds = loaded
        .value
        .published_at_unix_nanoseconds
        .as_deref()
        .and_then(|value| value.parse::<u128>().ok())
        .and_then(|published| unix_nanoseconds().ok()?.checked_sub(published))
        .map(|nanoseconds| saturating_u64(nanoseconds / 1_000_000_000));
    let publication_to_checkpoint_milliseconds = (generation_lag == 0)
        .then(|| {
            publication_to_checkpoint_milliseconds(
                &loaded.value,
                replica
                    .as_ref()
                    .and_then(|status| status.last_checkpoint_unix_nanoseconds),
            )
        })
        .flatten();
    Ok(ReplicaFollowerStatus {
        format_version: 2,
        privacy_safe_summary: true,
        health,
        published_generation: loaded.value.generation,
        applied_generation,
        generation_lag,
        state_present: prior_state.is_some(),
        replica_present,
        replica_initialized,
        checkpoint_age_seconds: replica
            .as_ref()
            .and_then(|status| status.checkpoint_age_seconds),
        published_generation_age_seconds,
        publication_to_checkpoint_milliseconds,
        restoration_complete: replica
            .as_ref()
            .and_then(|status| status.restoration_complete),
        archive_validation_deferred_until_application: true,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GroupLocation {
    Retained,
    Quarantined(PathBuf),
}

fn publication_history_path(handoff_path: &Path) -> Result<PathBuf, RestoreError> {
    let file_name = handoff_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            RestoreError::UnsafePath("replica handoff filename is invalid".to_string())
        })?;
    Ok(handoff_path.with_file_name(format!("{file_name}.generations.json")))
}

fn load_or_reconcile_publication_history(
    handoff_path: &Path,
    current: Option<&LoadedHandoff>,
) -> Result<ReplicaPublicationHistory, RestoreError> {
    let path = publication_history_path(handoff_path)?;
    let mut changed = false;
    let mut history = if path_entry_exists(&path)? {
        let bytes = read_owner_only_file_limited(&path, MAX_PUBLICATION_HISTORY_BYTES)?;
        serde_json::from_slice::<ReplicaPublicationHistory>(&bytes)?
    } else {
        changed = true;
        ReplicaPublicationHistory {
            format_version: PUBLICATION_HISTORY_FORMAT_VERSION,
            entries: Vec::new(),
        }
    };
    validate_publication_history(&history)?;
    match current {
        None if !history.entries.is_empty() => {
            return Err(RestoreError::Integrity(
                "publication history exists without a current handoff".to_string(),
            ));
        }
        None => {}
        Some(current) => {
            let exact_tail = history.entries.last().is_some_and(|entry| {
                entry.handoff.generation == current.value.generation
                    && entry.handoff_sha256 == current.sha256
                    && entry.quarantine_directory.is_none()
            });
            if !exact_tail {
                let equivalent_tail = history.entries.last().is_some_and(|entry| {
                    entry.handoff.generation == current.value.generation
                        && entry.quarantine_directory.is_none()
                        && same_archive_identity(&entry.handoff, &current.value)
                });
                if equivalent_tail {
                    let tail = history.entries.last_mut().ok_or_else(|| {
                        RestoreError::Integrity(
                            "publication history unexpectedly became empty".to_string(),
                        )
                    })?;
                    tail.handoff = current.value.clone();
                    tail.handoff_sha256 = current.sha256.clone();
                    tail.handoff_value_sha256 = owner_json_sha256(&current.value)?;
                    changed = true;
                } else if history
                    .entries
                    .last()
                    .is_some_and(|entry| entry.handoff.generation >= current.value.generation)
                {
                    return Err(RestoreError::Integrity(
                        "publication history does not match the current handoff".to_string(),
                    ));
                } else {
                    if history.entries.len() >= MAX_PUBLICATION_HISTORY_ENTRIES {
                        return Err(RestoreError::Integrity(
                            "replica publication history reached its entry limit".to_string(),
                        ));
                    }
                    reject_reused_mutated_archive_path(&history, &current.value)?;
                    history.entries.push(ReplicaPublicationHistoryEntry {
                        handoff_value_sha256: owner_json_sha256(&current.value)?,
                        handoff: current.value.clone(),
                        handoff_sha256: current.sha256.clone(),
                        quarantine_directory: None,
                    });
                    changed = true;
                }
            }
        }
    }
    validate_publication_history(&history)?;
    if changed {
        write_publication_history(handoff_path, &history)?;
    }
    Ok(history)
}

fn validate_publication_history(history: &ReplicaPublicationHistory) -> Result<(), RestoreError> {
    if history.format_version != PUBLICATION_HISTORY_FORMAT_VERSION
        || history.entries.len() > MAX_PUBLICATION_HISTORY_ENTRIES
    {
        return Err(RestoreError::Integrity(
            "replica publication history is malformed".to_string(),
        ));
    }
    let mut prior_generation = 0_u64;
    for entry in &history.entries {
        validate_handoff_value(&entry.handoff)?;
        if entry.handoff.generation <= prior_generation
            || !valid_sha256(&entry.handoff_sha256)
            || !valid_sha256(&entry.handoff_value_sha256)
            || entry.handoff_value_sha256 != owner_json_sha256(&entry.handoff)?
            || entry
                .quarantine_directory
                .as_deref()
                .is_some_and(|path| !Path::new(path).is_absolute())
        {
            return Err(RestoreError::Integrity(
                "replica publication history is malformed".to_string(),
            ));
        }
        prior_generation = entry.handoff.generation;
    }
    publication_path_groups(history)?;
    Ok(())
}

fn write_publication_history(
    handoff_path: &Path,
    history: &ReplicaPublicationHistory,
) -> Result<(), RestoreError> {
    validate_publication_history(history)?;
    let bytes = owner_json_bytes(history)?;
    if bytes.len() as u64 > MAX_PUBLICATION_HISTORY_BYTES {
        return Err(RestoreError::Integrity(
            "replica publication history exceeds the size limit".to_string(),
        ));
    }
    write_atomic_owner_bytes(
        &publication_history_path(handoff_path)?,
        &bytes,
        "publication-history",
    )
}

fn publication_path_groups(
    history: &ReplicaPublicationHistory,
) -> Result<BTreeMap<String, Vec<usize>>, RestoreError> {
    let mut groups = BTreeMap::<String, Vec<usize>>::new();
    for (index, entry) in history.entries.iter().enumerate() {
        let path = Path::new(&entry.handoff.archive_directory);
        if !path.is_absolute() {
            return Err(RestoreError::Integrity(
                "publication history archive path is not absolute".to_string(),
            ));
        }
        groups
            .entry(entry.handoff.archive_directory.clone())
            .or_default()
            .push(index);
    }
    for indices in groups.values() {
        let first = &history.entries[indices[0]].handoff;
        if indices
            .iter()
            .skip(1)
            .any(|index| !same_archive_identity(first, &history.entries[*index].handoff))
        {
            return Err(RestoreError::Integrity(
                "one publication archive path has conflicting sealed contents".to_string(),
            ));
        }
        let _ = group_location(history, indices)?;
    }
    Ok(groups)
}

fn same_archive_identity(left: &ReplicaArchiveHandoff, right: &ReplicaArchiveHandoff) -> bool {
    left.archive_directory == right.archive_directory
        && left.source_fingerprint == right.source_fingerprint
        && left.report_sha256 == right.report_sha256
        && left.archive_seal_sha256 == right.archive_seal_sha256
        && left.archive_file_count == right.archive_file_count
        && left.archive_byte_count == right.archive_byte_count
}

fn reject_reused_mutated_archive_path(
    history: &ReplicaPublicationHistory,
    handoff: &ReplicaArchiveHandoff,
) -> Result<(), RestoreError> {
    if history.entries.iter().any(|entry| {
        entry.handoff.archive_directory == handoff.archive_directory
            && !same_archive_identity(&entry.handoff, handoff)
    }) {
        return Err(RestoreError::Integrity(
            "a published archive path cannot be reused for different sealed contents".to_string(),
        ));
    }
    Ok(())
}

fn group_location(
    history: &ReplicaPublicationHistory,
    indices: &[usize],
) -> Result<GroupLocation, RestoreError> {
    let locations = indices
        .iter()
        .map(|index| history.entries[*index].quarantine_directory.as_deref())
        .collect::<BTreeSet<_>>();
    if locations.len() != 1 {
        return Err(RestoreError::Integrity(
            "publications sharing one archive disagree about its location".to_string(),
        ));
    }
    match locations.into_iter().next().flatten() {
        None => Ok(GroupLocation::Retained),
        Some(path) => Ok(GroupLocation::Quarantined(PathBuf::from(path))),
    }
}

fn group_quarantine_path(
    history: &ReplicaPublicationHistory,
    indices: &[usize],
    quarantine_root: &Path,
) -> Result<PathBuf, RestoreError> {
    let latest = history
        .entries
        .get(*indices.last().ok_or_else(|| {
            RestoreError::Integrity("empty publication archive group".to_string())
        })?)
        .ok_or_else(|| {
            RestoreError::Integrity("publication history index is invalid".to_string())
        })?;
    Ok(quarantine_root.join(format!(
        "generation-{:020}-{}",
        latest.handoff.generation,
        &latest.handoff_sha256[..16]
    )))
}

fn require_group_retained_and_verified(
    history: &ReplicaPublicationHistory,
    indices: &[usize],
) -> Result<(), RestoreError> {
    if !matches!(group_location(history, indices)?, GroupLocation::Retained) {
        return Err(RestoreError::Integrity(
            "a protected publication has already been quarantined".to_string(),
        ));
    }
    verify_retained_group(
        history,
        indices,
        Path::new(&history.entries[indices[0]].handoff.archive_directory),
    )
}

fn verify_retained_group(
    history: &ReplicaPublicationHistory,
    indices: &[usize],
    original: &Path,
) -> Result<(), RestoreError> {
    let entry = &history.entries[indices[0]];
    ensure_private_directory(original)?;
    let canonical = fs::canonicalize(original)?;
    if canonical != original {
        return Err(RestoreError::Integrity(
            "retained publication archive path is not canonical".to_string(),
        ));
    }
    verify_handoff_archive(
        &LoadedHandoff {
            value: entry.handoff.clone(),
            sha256: entry.handoff_sha256.clone(),
        },
        &canonical,
    )
}

fn verify_quarantined_group(
    history: &ReplicaPublicationHistory,
    indices: &[usize],
    quarantine_path: &Path,
    original: &Path,
) -> Result<(), RestoreError> {
    if path_entry_exists(original)? {
        return Err(RestoreError::Integrity(
            "quarantined archive still exists at its retained path".to_string(),
        ));
    }
    ensure_private_directory(quarantine_path)?;
    let canonical = fs::canonicalize(quarantine_path)?;
    if canonical != quarantine_path {
        return Err(RestoreError::Integrity(
            "quarantined archive path is not canonical".to_string(),
        ));
    }
    let expected = &history.entries[indices[0]].handoff;
    let seal = archive_seal(&canonical)?;
    if seal.sha256 != expected.archive_seal_sha256
        || seal.file_count != expected.archive_file_count
        || seal.byte_count != expected.archive_byte_count
    {
        return Err(RestoreError::Integrity(
            "quarantined archive no longer matches its publication seal".to_string(),
        ));
    }
    Ok(())
}

fn mark_group_quarantined(
    history: &mut ReplicaPublicationHistory,
    indices: &[usize],
    quarantine_path: &Path,
) -> Result<(), RestoreError> {
    let path = quarantine_path
        .to_str()
        .ok_or_else(|| RestoreError::UnsafePath("quarantine path is not valid UTF-8".to_string()))?
        .to_string();
    for index in indices {
        history.entries[*index].quarantine_directory = Some(path.clone());
    }
    Ok(())
}

fn canonical_private_directory(path: &Path, label: &str) -> Result<PathBuf, RestoreError> {
    ensure_private_directory(path)?;
    let canonical = fs::canonicalize(path)?;
    ensure_private_directory(&canonical)
        .map_err(|_| RestoreError::Integrity(format!("{label} directory is not owner-only")))?;
    Ok(canonical)
}

fn reject_nested_paths(archive: &Path, quarantine_root: &Path) -> Result<(), RestoreError> {
    if archive == quarantine_root
        || archive.starts_with(quarantine_root)
        || quarantine_root.starts_with(archive)
    {
        return Err(RestoreError::UnsafePath(
            "archive and quarantine directories must not contain one another".to_string(),
        ));
    }
    Ok(())
}

fn require_same_filesystem(
    source: &Path,
    destination_directory: &Path,
) -> Result<(), RestoreError> {
    if fs::metadata(source)?.dev() != fs::metadata(destination_directory)?.dev() {
        return Err(RestoreError::UnsafePath(
            "recoverable archive quarantine requires one filesystem".to_string(),
        ));
    }
    Ok(())
}

fn sync_rename_parents(source: &Path, destination: &Path) -> Result<(), RestoreError> {
    let source_parent = source
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath("archive path has no parent".to_string()))?;
    let destination_parent = destination
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath("archive destination has no parent".to_string()))?;
    File::open(source_parent)?.sync_all()?;
    if destination_parent != source_parent {
        File::open(destination_parent)?.sync_all()?;
    }
    Ok(())
}

struct LoadedHandoff {
    value: ReplicaArchiveHandoff,
    sha256: String,
}

struct ControlLock {
    file: File,
}

impl ControlLock {
    fn acquire(target: &Path, label: &str) -> Result<Self, RestoreError> {
        let parent = target.parent().ok_or_else(|| {
            RestoreError::UnsafePath("replica control path has no parent".to_string())
        })?;
        ensure_private_directory(parent)?;
        let file_name = target
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                RestoreError::UnsafePath("replica control filename is invalid".to_string())
            })?;
        let lock_path = parent.join(format!(".{file_name}.{label}.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(lock_path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(RestoreError::Integrity(
                "replica control lock is not an owner-only single-link regular file".to_string(),
            ));
        }
        let descriptor = std::os::fd::AsRawFd::as_raw_fd(&file);
        if unsafe { libc::flock(descriptor, libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self { file })
    }
}

impl Drop for ControlLock {
    fn drop(&mut self) {
        let descriptor = std::os::fd::AsRawFd::as_raw_fd(&self.file);
        let _ = unsafe { libc::flock(descriptor, libc::LOCK_UN) };
    }
}

fn load_handoff(path: &Path) -> Result<LoadedHandoff, RestoreError> {
    let bytes = read_owner_only_control_file(path)?;
    let value: ReplicaArchiveHandoff = serde_json::from_slice(&bytes)?;
    validate_handoff_value(&value)?;
    Ok(LoadedHandoff {
        value,
        sha256: hex::encode(Sha256::digest(bytes)),
    })
}

fn validate_handoff_value(value: &ReplicaArchiveHandoff) -> Result<(), RestoreError> {
    if !matches!(value.format_version, 2 | HANDOFF_FORMAT_VERSION)
        || value.generation == 0
        || value.archive_directory.is_empty()
        || value.source_fingerprint.is_empty()
        || !valid_sha256(&value.report_sha256)
        || !valid_sha256(&value.archive_seal_sha256)
        || value.archive_file_count == 0
        || match value.format_version {
            2 => value.published_at_unix_nanoseconds.is_some(),
            HANDOFF_FORMAT_VERSION => {
                !value
                    .published_at_unix_nanoseconds
                    .as_deref()
                    .is_some_and(|timestamp| {
                        timestamp
                            .parse::<u128>()
                            .is_ok_and(|nanoseconds| nanoseconds > 0)
                    })
            }
            _ => true,
        }
    {
        return Err(RestoreError::Integrity(
            "replica archive handoff is malformed".to_string(),
        ));
    }
    Ok(())
}

fn load_follow_state(path: &Path) -> Result<ReplicaFollowState, RestoreError> {
    let bytes = read_owner_only_control_file(path)?;
    let state: ReplicaFollowState = serde_json::from_slice(&bytes)?;
    if state.format_version != FOLLOW_STATE_FORMAT_VERSION
        || !valid_replica_id(&state.replica_id)
        || state.generation == 0
        || !valid_sha256(&state.handoff_sha256)
        || state.source_fingerprint.is_empty()
        || state.checkpoint_revision.parse::<u128>().is_err()
    {
        return Err(RestoreError::Integrity(
            "replica follow state is malformed".to_string(),
        ));
    }
    Ok(state)
}

fn require_authoritative_handoff_report(
    report: &crate::RestorationReport,
) -> Result<(), RestoreError> {
    if report.account_id.is_empty()
        || report.source_fingerprint.is_empty()
        || !report.replica_mutation_eligible()
    {
        return Err(RestoreError::Integrity(
            "replica handoff requires a replica-eligible restoration archive".to_string(),
        ));
    }
    Ok(())
}

fn read_owner_only_control_file(path: &Path) -> Result<Vec<u8>, RestoreError> {
    read_owner_only_file_limited(path, MAX_CONTROL_FILE_BYTES)
}

fn path_entry_exists(path: &Path) -> Result<bool, RestoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn read_owner_only_file_limited(path: &Path, maximum_bytes: u64) -> Result<Vec<u8>, RestoreError> {
    ensure_private_regular_file(path)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let before = file.metadata()?;
    if before.len() > maximum_bytes {
        return Err(RestoreError::Integrity(
            "replica control file exceeds the size limit".to_string(),
        ));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(maximum_bytes + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() as u64 != before.len()
        || bytes.len() as u64 > maximum_bytes
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(RestoreError::Integrity(
            "replica control file changed during verification".to_string(),
        ));
    }
    Ok(bytes)
}

fn write_atomic_owner_json(
    path: &Path,
    value: &impl Serialize,
    label: &str,
) -> Result<(), RestoreError> {
    write_atomic_owner_bytes(path, &owner_json_bytes(value)?, label)
}

fn owner_json_bytes(value: &impl Serialize) -> Result<Vec<u8>, RestoreError> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn owner_json_sha256(value: &impl Serialize) -> Result<String, RestoreError> {
    Ok(hex::encode(Sha256::digest(owner_json_bytes(value)?)))
}

fn write_atomic_owner_bytes(path: &Path, bytes: &[u8], label: &str) -> Result<(), RestoreError> {
    let parent = path.parent().ok_or_else(|| {
        RestoreError::UnsafePath("replica control path has no parent".to_string())
    })?;
    ensure_private_directory(parent)?;
    if path_entry_exists(path)? {
        ensure_private_regular_file(path)?;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RestoreError::Integrity("system clock predates Unix epoch".to_string()))?
        .as_nanos();
    let temporary = parent.join(format!(
        ".greenbubbles-{label}-{}-{nonce}.tmp",
        std::process::id()
    ));
    let result = (|| -> Result<(), RestoreError> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&temporary)?;
        let mut file = file;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() && temporary.try_exists().unwrap_or(false) {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_replica_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn unix_nanoseconds() -> Result<u128, RestoreError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RestoreError::Integrity("system clock predates Unix epoch".to_string()))?
        .as_nanos())
}

fn elapsed_milliseconds(started: Instant) -> u64 {
    saturating_u64(started.elapsed().as_millis())
}

fn publication_to_checkpoint_milliseconds(
    handoff: &ReplicaArchiveHandoff,
    checkpoint: Option<u128>,
) -> Option<u64> {
    let published = handoff
        .published_at_unix_nanoseconds
        .as_deref()?
        .parse::<u128>()
        .ok()?;
    let elapsed = checkpoint?.checked_sub(published)?;
    Some(saturating_u64(elapsed / 1_000_000))
}

fn saturating_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn ensure_target_outside_archive(
    archive_root: &Path,
    target: &Path,
    label: &str,
) -> Result<(), RestoreError> {
    let parent = target
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath(format!("replica {label} path has no parent")))?;
    ensure_private_directory(parent)?;
    let canonical_parent = fs::canonicalize(parent)?;
    if canonical_parent == archive_root || canonical_parent.starts_with(archive_root) {
        return Err(RestoreError::UnsafePath(format!(
            "replica {label} must live outside the immutable archive"
        )));
    }
    Ok(())
}

struct ArchiveSeal {
    sha256: String,
    file_count: u64,
    byte_count: u64,
}

fn archive_seal(root: &Path) -> Result<ArchiveSeal, RestoreError> {
    ensure_private_directory(root)?;
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|error| RestoreError::Integrity(error.to_string()))?;
        if entry.path() == root {
            continue;
        }
        if entry.file_type().is_symlink()
            || (!entry.file_type().is_dir() && !entry.file_type().is_file())
        {
            return Err(RestoreError::Integrity(
                "restoration archive seal encountered an unsupported filesystem entry".to_string(),
            ));
        }
        if entry.file_type().is_file() {
            files.push(entry.path().to_path_buf());
            if files.len() > MAX_ARCHIVE_SEAL_FILE_COUNT {
                return Err(RestoreError::Integrity(
                    "restoration archive exceeds the seal file-count limit".to_string(),
                ));
            }
        }
    }
    files.sort_by(|left, right| {
        left.strip_prefix(root)
            .unwrap_or(left)
            .cmp(right.strip_prefix(root).unwrap_or(right))
    });
    let mut seal = Sha256::new();
    let mut byte_count = 0_u64;
    for path in &files {
        ensure_private_regular_file(path)?;
        let relative = path.strip_prefix(root).map_err(|_| {
            RestoreError::Integrity("archive seal path escaped its root".to_string())
        })?;
        let relative = relative.to_str().ok_or_else(|| {
            RestoreError::UnsafePath("archive seal path is not valid UTF-8".to_string())
        })?;
        let (size, digest) = digest_stable_file(path)?;
        byte_count = byte_count.checked_add(size).ok_or_else(|| {
            RestoreError::Integrity("archive seal byte count overflowed".to_string())
        })?;
        seal.update(relative.as_bytes());
        seal.update([0]);
        seal.update(size.to_le_bytes());
        seal.update(digest);
    }
    Ok(ArchiveSeal {
        sha256: hex::encode(seal.finalize()),
        file_count: files.len() as u64,
        byte_count,
    })
}

fn digest_stable_file(path: &Path) -> Result<(u64, [u8; 32]), RestoreError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let before = file.metadata()?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 128 * 1_024];
    let mut observed = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        observed = observed.checked_add(count as u64).ok_or_else(|| {
            RestoreError::Integrity("archive seal file size overflowed".to_string())
        })?;
        digest.update(&buffer[..count]);
    }
    let after = file.metadata()?;
    if observed != before.len()
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(RestoreError::Integrity(
            "restoration archive file changed while sealing".to_string(),
        ));
    }
    Ok((observed, digest.finalize().into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        RestorationArchiveScope, RestorationCompletion, RestorationIntegrity,
        RestorationMediaPhase, RestorationReport,
    };

    #[test]
    fn next_publication_rejects_a_predecessor_changed_during_preparation() {
        let fixture = tempfile::tempdir().unwrap();
        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let first = minimal_archive(fixture.path(), "first", "source-first");
        let concurrent = minimal_archive(fixture.path(), "concurrent", "source-concurrent");
        let pending = minimal_archive(fixture.path(), "pending", "source-pending");
        let handoff = fixture.path().join("handoff.json");

        publish_replica_handoff(&first, &handoff, 1).unwrap();
        let mut legacy: serde_json::Value =
            serde_json::from_slice(&fs::read(&handoff).unwrap()).unwrap();
        legacy["formatVersion"] = serde_json::json!(2);
        legacy
            .as_object_mut()
            .unwrap()
            .remove("publishedAtUnixNanoseconds");
        fs::write(&handoff, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();
        let predecessor = capture_publication_predecessor(&handoff, Some(&first)).unwrap();
        publish_replica_handoff(&concurrent, &handoff, 2).unwrap();
        let error =
            publish_replica_handoff_next_if_current(&pending, &handoff, &predecessor).unwrap_err();
        assert!(error
            .to_string()
            .contains("changed while the next archive was prepared"));
        let current = load_handoff(&handoff).unwrap();
        assert_eq!(current.value.generation, 2);
        assert_eq!(
            Path::new(&current.value.archive_directory),
            fs::canonicalize(concurrent).unwrap()
        );
    }

    #[test]
    fn quarantines_only_retired_archives_and_restores_them_by_generation() {
        let fixture = tempfile::tempdir().unwrap();
        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let quarantine = fixture.path().join("quarantine");
        fs::create_dir(&quarantine).unwrap();
        fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)).unwrap();
        let first = minimal_archive(fixture.path(), "first", "source-first");
        let second = minimal_archive(fixture.path(), "second", "source-second");
        let third = minimal_archive(fixture.path(), "third", "source-third");
        let fourth = minimal_archive(fixture.path(), "fourth", "source-fourth");
        let handoff = fixture.path().join("handoff.json");

        publish_replica_handoff(&first, &handoff, 1).unwrap();
        publish_replica_handoff(&second, &handoff, 2).unwrap();
        publish_replica_handoff(&third, &handoff, 3).unwrap();
        publish_replica_handoff(&fourth, &handoff, 4).unwrap();
        let history_path = publication_history_path(&handoff).unwrap();
        assert_eq!(
            fs::metadata(&history_path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        assert!(quarantine_retired_replica_archives(&handoff, &quarantine, 1).is_err());
        let report = quarantine_retired_replica_archives(&handoff, &quarantine, 2).unwrap();
        assert_eq!(report.current_generation, 4);
        assert_eq!(report.protected_publication_count, 2);
        assert_eq!(report.newly_quarantined_archive_count, 2);
        assert!(report.recoverable);
        assert!(!first.exists());
        assert!(!second.exists());
        assert!(third.is_dir());
        assert!(fourth.is_dir());
        assert_eq!(
            Path::new(&load_handoff(&handoff).unwrap().value.archive_directory),
            fs::canonicalize(&fourth).unwrap()
        );
        let serialized = serde_json::to_string(&report).unwrap();
        for private_path in [&first, &second, &third, &fourth, &quarantine] {
            assert!(!serialized.contains(private_path.to_str().unwrap()));
        }

        let repeated = quarantine_retired_replica_archives(&handoff, &quarantine, 2).unwrap();
        assert_eq!(repeated.newly_quarantined_archive_count, 0);
        assert_eq!(repeated.already_quarantined_archive_count, 2);

        let restored = restore_quarantined_replica_archive(&handoff, &quarantine, 1).unwrap();
        assert_eq!(restored.requested_generation, 1);
        assert_eq!(restored.restored_archive_count, 1);
        assert_eq!(restored.restored_publication_count, 1);
        assert!(restored.archive_verified);
        assert!(first.is_dir());
        assert!(third.is_dir());
        assert!(fourth.is_dir());
        let restore_serialized = serde_json::to_string(&restored).unwrap();
        assert!(!restore_serialized.contains(first.to_str().unwrap()));
    }

    #[test]
    fn refuses_to_reuse_a_published_path_for_mutated_contents() {
        let fixture = tempfile::tempdir().unwrap();
        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let archive = minimal_archive(fixture.path(), "archive", "source-first");
        let handoff = fixture.path().join("handoff.json");
        publish_replica_handoff(&archive, &handoff, 1).unwrap();

        let mut report: RestorationReport =
            serde_json::from_slice(&fs::read(archive.join("report.json")).unwrap()).unwrap();
        report.source_fingerprint = "source-mutated".to_string();
        fs::write(
            archive.join("report.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        let error = publish_replica_handoff(&archive, &handoff, 2).unwrap_err();
        assert!(error
            .to_string()
            .contains("cannot be reused for different sealed contents"));
        assert_eq!(load_handoff(&handoff).unwrap().value.generation, 1);
    }

    #[test]
    fn protects_shared_paths_and_recovers_interrupted_quarantine_moves() {
        let fixture = tempfile::tempdir().unwrap();
        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let quarantine = fixture.path().join("quarantine");
        fs::create_dir(&quarantine).unwrap();
        fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)).unwrap();
        let shared = minimal_archive(fixture.path(), "shared", "source-shared");
        let retired = minimal_archive(fixture.path(), "retired", "source-retired");
        let current = minimal_archive(fixture.path(), "current", "source-current");
        let handoff = fixture.path().join("handoff.json");

        publish_replica_handoff(&shared, &handoff, 1).unwrap();
        publish_replica_handoff(&retired, &handoff, 2).unwrap();
        publish_replica_handoff(&shared, &handoff, 3).unwrap();
        publish_replica_handoff(&current, &handoff, 4).unwrap();

        let loaded = load_handoff(&handoff).unwrap();
        let history = load_or_reconcile_publication_history(&handoff, Some(&loaded)).unwrap();
        let groups = publication_path_groups(&history).unwrap();
        let retired_path = fs::canonicalize(&retired).unwrap();
        let retired_indices = groups.get(retired_path.to_str().unwrap()).unwrap();
        let quarantine_root = fs::canonicalize(&quarantine).unwrap();
        let interrupted_destination =
            group_quarantine_path(&history, retired_indices, &quarantine_root).unwrap();
        fs::rename(&retired_path, &interrupted_destination).unwrap();

        let recovered = quarantine_retired_replica_archives(&handoff, &quarantine, 2).unwrap();
        assert_eq!(recovered.newly_quarantined_archive_count, 1);
        assert_eq!(recovered.shared_with_protected_generation_count, 1);
        assert!(shared.is_dir());
        assert!(current.is_dir());
        assert!(!retired.exists());

        fs::rename(&interrupted_destination, &retired_path).unwrap();
        let restored = restore_quarantined_replica_archive(&handoff, &quarantine, 2).unwrap();
        assert_eq!(restored.restored_archive_count, 0);
        assert_eq!(restored.restored_publication_count, 1);
        assert!(retired.is_dir());
        assert!(shared.is_dir());
        assert!(current.is_dir());
    }

    #[test]
    fn quarantine_never_replaces_a_preexisting_filesystem_entry() {
        let fixture = tempfile::tempdir().unwrap();
        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let quarantine = fixture.path().join("quarantine");
        fs::create_dir(&quarantine).unwrap();
        fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)).unwrap();
        let first = minimal_archive(fixture.path(), "first", "source-first");
        let second = minimal_archive(fixture.path(), "second", "source-second");
        let third = minimal_archive(fixture.path(), "third", "source-third");
        let handoff = fixture.path().join("handoff.json");
        publish_replica_handoff(&first, &handoff, 1).unwrap();
        publish_replica_handoff(&second, &handoff, 2).unwrap();
        publish_replica_handoff(&third, &handoff, 3).unwrap();

        let loaded = load_handoff(&handoff).unwrap();
        let history = load_or_reconcile_publication_history(&handoff, Some(&loaded)).unwrap();
        let first_path = fs::canonicalize(&first).unwrap();
        let groups = publication_path_groups(&history).unwrap();
        let indices = groups.get(first_path.to_str().unwrap()).unwrap();
        let quarantine_root = fs::canonicalize(&quarantine).unwrap();
        let destination = group_quarantine_path(&history, indices, &quarantine_root).unwrap();
        std::os::unix::fs::symlink("missing-target", &destination).unwrap();

        assert!(quarantine_retired_replica_archives(&handoff, &quarantine, 2).is_err());
        assert!(first.is_dir());
        assert!(second.is_dir());
        assert!(third.is_dir());
        assert!(fs::symlink_metadata(destination)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    fn minimal_archive(parent: &Path, name: &str, source_fingerprint: &str) -> PathBuf {
        let archive = parent.join(name);
        fs::create_dir(&archive).unwrap();
        fs::set_permissions(&archive, fs::Permissions::from_mode(0o700)).unwrap();
        let report = RestorationReport {
            format_version: 2,
            account_id: "synthetic-account".to_string(),
            self_participant_id: None,
            account_binding_evidence: None,
            storage: None,
            source_fingerprint: source_fingerprint.to_string(),
            client_build_compatibility: Default::default(),
            acquisition: None,
            archive_scope: RestorationArchiveScope::Authoritative,
            database_coverage: None,
            media_phase: RestorationMediaPhase::Resolved,
            messages_path: "unused".to_string(),
            rejections_path: "unused".to_string(),
            artifacts_path: "unused".to_string(),
            conversations_path: "unused".to_string(),
            participants_path: "unused".to_string(),
            cached_moments_path: None,
            cached_moment_interactions_path: None,
            cached_surfaces_path: None,
            coverage_path: "unused".to_string(),
            report_path: "unused".to_string(),
            integrity: RestorationIntegrity::default(),
            completion: RestorationCompletion::evaluate(&RestorationIntegrity::default()),
        };
        let report_path = archive.join("report.json");
        fs::write(&report_path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        fs::set_permissions(report_path, fs::Permissions::from_mode(0o600)).unwrap();
        archive
    }
}
