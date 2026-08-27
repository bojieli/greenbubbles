use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::archive::{ensure_private_directory, ensure_private_regular_file, load_report};
use crate::audit::audit_archive;
use crate::replica::{
    bootstrap_replica, replica_matches_authoritative_archive, replica_status, synchronize_replica,
};
use crate::{ReplicaKey, RestoreError};

const HANDOFF_FORMAT_VERSION: u32 = 2;
const FOLLOW_STATE_FORMAT_VERSION: u32 = 1;
const MAX_CONTROL_FILE_BYTES: u64 = 64 * 1_024;
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
    match (handoff_path.try_exists()?, canonical_previous) {
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
        PublicationPredecessor::Absent if !handoff_path.try_exists()? => 1,
        PublicationPredecessor::Current {
            handoff_sha256,
            archive_directory,
        } if handoff_path.try_exists()? => {
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
    if handoff_path.try_exists()? {
        let prior = load_handoff(handoff_path)?;
        if generation <= prior.value.generation {
            return Err(RestoreError::Integrity(
                "replica handoff generation must advance monotonically".to_string(),
            ));
        }
    }
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
    };
    write_atomic_owner_json(handoff_path, &handoff, "handoff")?;
    Ok(ReplicaHandoffReceipt {
        format_version: 1,
        privacy_safe_summary: true,
        generation,
        handoff_written: true,
        authoritative_archive_required: true,
    })
}

pub fn follow_replica_once(
    handoff_path: &Path,
    state_path: &Path,
    replica_path: &Path,
    key: &ReplicaKey,
) -> Result<ReplicaFollowReport, RestoreError> {
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
    let prior_state = state_path
        .try_exists()?
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
                format_version: 1,
                privacy_safe_summary: true,
                generation: state.generation,
                outcome: ReplicaFollowOutcome::AlreadyApplied,
                idempotent: true,
                source_advanced: false,
                added_count: 0,
                changed_count: 0,
                removed_count: 0,
                restoration_complete: before.restoration_complete.unwrap_or(false),
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
                "existing replica cannot be adopted without an exact authoritative archive"
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
        format_version: 1,
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
    let prior_state = state_path
        .try_exists()?
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

    let replica_present = replica_path.try_exists()?;
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
    Ok(ReplicaFollowerStatus {
        format_version: 1,
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
        restoration_complete: replica
            .as_ref()
            .and_then(|status| status.restoration_complete),
        archive_validation_deferred_until_application: true,
    })
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
    if value.format_version != HANDOFF_FORMAT_VERSION
        || value.generation == 0
        || value.archive_directory.is_empty()
        || value.source_fingerprint.is_empty()
        || !valid_sha256(&value.report_sha256)
        || !valid_sha256(&value.archive_seal_sha256)
        || value.archive_file_count == 0
    {
        return Err(RestoreError::Integrity(
            "replica archive handoff is malformed".to_string(),
        ));
    }
    Ok(LoadedHandoff {
        value,
        sha256: hex::encode(Sha256::digest(bytes)),
    })
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
        || report.archive_scope != crate::RestorationArchiveScope::Authoritative
    {
        return Err(RestoreError::Integrity(
            "replica handoff requires an authoritative restoration archive".to_string(),
        ));
    }
    Ok(())
}

fn read_owner_only_control_file(path: &Path) -> Result<Vec<u8>, RestoreError> {
    ensure_private_regular_file(path)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let before = file.metadata()?;
    if before.len() > MAX_CONTROL_FILE_BYTES {
        return Err(RestoreError::Integrity(
            "replica control file exceeds the size limit".to_string(),
        ));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(MAX_CONTROL_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() as u64 != before.len()
        || bytes.len() as u64 > MAX_CONTROL_FILE_BYTES
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
    let parent = path.parent().ok_or_else(|| {
        RestoreError::UnsafePath("replica control path has no parent".to_string())
    })?;
    ensure_private_directory(parent)?;
    if path.try_exists()? {
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
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, value)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
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

    fn minimal_archive(parent: &Path, name: &str, source_fingerprint: &str) -> PathBuf {
        let archive = parent.join(name);
        fs::create_dir(&archive).unwrap();
        fs::set_permissions(&archive, fs::Permissions::from_mode(0o700)).unwrap();
        let report = RestorationReport {
            format_version: 2,
            account_id: "synthetic-account".to_string(),
            source_fingerprint: source_fingerprint.to_string(),
            client_build_compatibility: Default::default(),
            acquisition: None,
            archive_scope: RestorationArchiveScope::Authoritative,
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
