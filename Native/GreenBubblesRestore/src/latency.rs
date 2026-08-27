use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{RestoreError, SnapshotAcquisitionMode};

const MAX_PRIVATE_REPORT_BYTES: u64 = 64 * 1_024 * 1_024;
const MAX_SAMPLE_ARRAY_BYTES: u64 = 16 * 1_024 * 1_024;
const MAX_SAMPLE_COUNT: usize = 10_000;
const MAX_STAGE_DURATION_MILLISECONDS: u64 = 7 * 24 * 60 * 60 * 1_000;
const TEXT_FRESHNESS_OBJECTIVE_MILLISECONDS: u64 = 60_000;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LatencyEvidenceLimitation {
    SourcePersistenceStartNotObserved,
    InterCommandDelayNotMeasured,
    DisposableScenarioNotAttributed,
    CompleteRestorationNotAchieved,
    SourceDidNotAdvance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SnapshotLatencyStages {
    pub planning_duration_milliseconds: u64,
    pub acquisition_duration_milliseconds: u64,
    pub total_duration_milliseconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OfflineLatencyStages {
    pub input_validation_duration_milliseconds: u64,
    pub catalog_preparation_duration_milliseconds: u64,
    pub restoration_duration_milliseconds: u64,
    pub publication_validation_duration_milliseconds: u64,
    pub total_duration_milliseconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FollowerLatencyStages {
    pub apply_duration_milliseconds: u64,
    pub publication_to_checkpoint_milliseconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LatencyEvidenceSample {
    pub format_version: u32,
    pub privacy_safe_summary: bool,
    pub acquisition_mode: SnapshotAcquisitionMode,
    pub generation: u64,
    pub snapshot: SnapshotLatencyStages,
    pub offline: OfflineLatencyStages,
    pub follower: FollowerLatencyStages,
    pub active_processing_duration_milliseconds: u64,
    pub restoration_complete: bool,
    pub source_advanced: bool,
    pub source_row_count: u64,
    pub restored_row_count: u64,
    pub rejected_row_count: u64,
    pub semantic_gap_count: u64,
    pub message_candidate_gap_count: u64,
    pub missing_artifact_count: u64,
    pub artifact_decode_gap_count: u64,
    pub publication_to_checkpoint_within_objective: bool,
    pub text_freshness_objective_milliseconds: u64,
    pub full_end_to_end_objective_proven: bool,
    pub limitations: Vec<LatencyEvidenceLimitation>,
    pub stage_bindings_verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LatencyPercentiles {
    pub minimum_milliseconds: u64,
    pub p50_milliseconds: u64,
    pub p95_milliseconds: u64,
    pub maximum_milliseconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LatencyEvidenceSummary {
    pub format_version: u32,
    pub privacy_safe_summary: bool,
    pub sample_count: u64,
    pub complete_restoration_sample_count: u64,
    pub source_advanced_sample_count: u64,
    pub acquisition_mode_counts: std::collections::BTreeMap<String, u64>,
    pub active_processing: LatencyPercentiles,
    pub publication_to_checkpoint: LatencyPercentiles,
    pub every_publication_to_checkpoint_within_objective: bool,
    pub text_freshness_objective_milliseconds: u64,
    pub full_end_to_end_objective_proven: bool,
    pub limitations: Vec<LatencyEvidenceLimitation>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotCommandReportInput {
    format_version: u32,
    database_set_count: u64,
    manifest: SnapshotManifestBindingInput,
    automatically_cleaned_up: bool,
    snapshot_directory: Option<String>,
    planning_duration_milliseconds: u64,
    acquisition_duration_milliseconds: u64,
    total_duration_milliseconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotManifestBindingInput {
    manifest_format_version: u32,
    source_fingerprint: String,
    acquisition: Option<SnapshotAcquisitionBindingInput>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotAcquisitionBindingInput {
    mode: SnapshotAcquisitionMode,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OfflineReportInput {
    format_version: u32,
    privacy_safe_summary: bool,
    acquisition_mode: SnapshotAcquisitionMode,
    previous_chain_verified: bool,
    previous_archive_verified: bool,
    incremental_fragment_verified: bool,
    authoritative_archive_verified: bool,
    generation: u64,
    full_restoration_achieved: bool,
    source_row_count: u64,
    restored_row_count: u64,
    rejected_row_count: u64,
    semantic_gap_count: u64,
    message_candidate_gap_count: u64,
    missing_artifact_count: u64,
    artifact_decode_gap_count: u64,
    input_validation_duration_milliseconds: u64,
    catalog_preparation_duration_milliseconds: u64,
    restoration_duration_milliseconds: u64,
    publication_validation_duration_milliseconds: u64,
    total_duration_milliseconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FollowerReportInput {
    format_version: u32,
    privacy_safe_summary: bool,
    generation: u64,
    outcome: String,
    source_advanced: bool,
    restoration_complete: bool,
    apply_duration_milliseconds: u64,
    publication_to_checkpoint_milliseconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HandoffBindingInput {
    format_version: u32,
    generation: u64,
    source_fingerprint: String,
    published_at_unix_nanoseconds: Option<String>,
}

pub fn compose_latency_evidence_sample(
    snapshot_report_path: &Path,
    offline_report_path: &Path,
    follower_report_path: &Path,
    handoff_path: &Path,
) -> Result<LatencyEvidenceSample, RestoreError> {
    let snapshot: SnapshotCommandReportInput =
        read_private_json(snapshot_report_path, MAX_PRIVATE_REPORT_BYTES)?;
    let offline: OfflineReportInput =
        read_private_json(offline_report_path, MAX_PRIVATE_REPORT_BYTES)?;
    let follower: FollowerReportInput =
        read_private_json(follower_report_path, MAX_PRIVATE_REPORT_BYTES)?;
    let handoff: HandoffBindingInput = read_private_json(handoff_path, MAX_PRIVATE_REPORT_BYTES)?;

    let acquisition = snapshot.manifest.acquisition.ok_or_else(|| {
        RestoreError::Integrity(
            "latency evidence requires complete snapshot acquisition evidence".to_string(),
        )
    })?;
    if snapshot.format_version != 2
        || snapshot.manifest.manifest_format_version != 3
        || snapshot.database_set_count == 0
        || snapshot.automatically_cleaned_up
        || snapshot
            .snapshot_directory
            .as_deref()
            .is_none_or(str::is_empty)
    {
        return Err(RestoreError::Integrity(
            "latency evidence requires a preserved format-2 snapshot report".to_string(),
        ));
    }
    if offline.format_version != 2
        || !offline.privacy_safe_summary
        || follower.format_version != 2
        || !follower.privacy_safe_summary
        || handoff.format_version != 3
        || handoff
            .published_at_unix_nanoseconds
            .as_deref()
            .and_then(|value| value.parse::<u128>().ok())
            .is_none_or(|value| value == 0)
    {
        return Err(RestoreError::Integrity(
            "latency evidence input report format is unsupported".to_string(),
        ));
    }
    if acquisition.mode != offline.acquisition_mode
        || offline.generation == 0
        || offline.generation != follower.generation
        || offline.generation != handoff.generation
        || snapshot.manifest.source_fingerprint.is_empty()
        || snapshot.manifest.source_fingerprint != handoff.source_fingerprint
    {
        return Err(RestoreError::Integrity(
            "latency evidence reports do not describe one published generation".to_string(),
        ));
    }
    let offline_transition_verified = match offline.acquisition_mode {
        SnapshotAcquisitionMode::Bootstrap => {
            !offline.previous_chain_verified
                && !offline.previous_archive_verified
                && !offline.incremental_fragment_verified
        }
        SnapshotAcquisitionMode::Incremental => {
            offline.previous_chain_verified
                && offline.previous_archive_verified
                && offline.incremental_fragment_verified
        }
        SnapshotAcquisitionMode::IntegrityScan => {
            offline.previous_chain_verified
                && offline.previous_archive_verified
                && !offline.incremental_fragment_verified
        }
    };
    if !offline.authoritative_archive_verified || !offline_transition_verified {
        return Err(RestoreError::Integrity(
            "offline latency evidence does not prove its publication transition".to_string(),
        ));
    }
    if !matches!(follower.outcome.as_str(), "bootstrapped" | "synchronized") {
        return Err(RestoreError::Integrity(
            "latency evidence requires an actual follower application".to_string(),
        ));
    }
    let publication_to_checkpoint =
        follower
            .publication_to_checkpoint_milliseconds
            .ok_or_else(|| {
                RestoreError::Integrity(
                    "latency evidence has no publication-to-checkpoint measurement".to_string(),
                )
            })?;
    if offline.full_restoration_achieved != follower.restoration_complete {
        return Err(RestoreError::Integrity(
            "offline and follower restoration completion disagree".to_string(),
        ));
    }
    let accounted_rows = offline
        .restored_row_count
        .checked_add(offline.rejected_row_count)
        .ok_or_else(|| RestoreError::Integrity("row accounting overflowed".to_string()))?;
    if accounted_rows != offline.source_row_count {
        return Err(RestoreError::Integrity(
            "offline latency evidence has inconsistent row accounting".to_string(),
        ));
    }
    if offline.full_restoration_achieved
        && (offline.rejected_row_count != 0
            || offline.semantic_gap_count != 0
            || offline.message_candidate_gap_count != 0
            || offline.missing_artifact_count != 0
            || offline.artifact_decode_gap_count != 0)
    {
        return Err(RestoreError::Integrity(
            "complete restoration conflicts with reported coverage gaps".to_string(),
        ));
    }

    let snapshot_stages = SnapshotLatencyStages {
        planning_duration_milliseconds: snapshot.planning_duration_milliseconds,
        acquisition_duration_milliseconds: snapshot.acquisition_duration_milliseconds,
        total_duration_milliseconds: snapshot.total_duration_milliseconds,
    };
    let offline_stages = OfflineLatencyStages {
        input_validation_duration_milliseconds: offline.input_validation_duration_milliseconds,
        catalog_preparation_duration_milliseconds: offline
            .catalog_preparation_duration_milliseconds,
        restoration_duration_milliseconds: offline.restoration_duration_milliseconds,
        publication_validation_duration_milliseconds: offline
            .publication_validation_duration_milliseconds,
        total_duration_milliseconds: offline.total_duration_milliseconds,
    };
    validate_stage_durations(
        &snapshot_stages,
        &offline_stages,
        follower.apply_duration_milliseconds,
        publication_to_checkpoint,
    )?;
    let active_processing_duration_milliseconds = snapshot
        .total_duration_milliseconds
        .checked_add(offline.total_duration_milliseconds)
        .and_then(|value| value.checked_add(follower.apply_duration_milliseconds))
        .ok_or_else(|| {
            RestoreError::Integrity("latency evidence duration overflowed".to_string())
        })?;
    let mut limitations = vec![
        LatencyEvidenceLimitation::SourcePersistenceStartNotObserved,
        LatencyEvidenceLimitation::InterCommandDelayNotMeasured,
        LatencyEvidenceLimitation::DisposableScenarioNotAttributed,
    ];
    if !offline.full_restoration_achieved {
        limitations.push(LatencyEvidenceLimitation::CompleteRestorationNotAchieved);
    }
    if !follower.source_advanced {
        limitations.push(LatencyEvidenceLimitation::SourceDidNotAdvance);
    }

    Ok(LatencyEvidenceSample {
        format_version: 1,
        privacy_safe_summary: true,
        acquisition_mode: acquisition.mode,
        generation: offline.generation,
        snapshot: snapshot_stages,
        offline: offline_stages,
        follower: FollowerLatencyStages {
            apply_duration_milliseconds: follower.apply_duration_milliseconds,
            publication_to_checkpoint_milliseconds: publication_to_checkpoint,
        },
        active_processing_duration_milliseconds,
        restoration_complete: offline.full_restoration_achieved,
        source_advanced: follower.source_advanced,
        source_row_count: offline.source_row_count,
        restored_row_count: offline.restored_row_count,
        rejected_row_count: offline.rejected_row_count,
        semantic_gap_count: offline.semantic_gap_count,
        message_candidate_gap_count: offline.message_candidate_gap_count,
        missing_artifact_count: offline.missing_artifact_count,
        artifact_decode_gap_count: offline.artifact_decode_gap_count,
        publication_to_checkpoint_within_objective: publication_to_checkpoint
            <= TEXT_FRESHNESS_OBJECTIVE_MILLISECONDS,
        text_freshness_objective_milliseconds: TEXT_FRESHNESS_OBJECTIVE_MILLISECONDS,
        full_end_to_end_objective_proven: false,
        limitations,
        stage_bindings_verified: true,
    })
}

pub fn summarize_latency_evidence_samples(
    sample_array_path: &Path,
) -> Result<LatencyEvidenceSummary, RestoreError> {
    let samples: Vec<LatencyEvidenceSample> =
        read_private_json(sample_array_path, MAX_SAMPLE_ARRAY_BYTES)?;
    if samples.is_empty() || samples.len() > MAX_SAMPLE_COUNT {
        return Err(RestoreError::Integrity(
            "latency evidence summary requires 1 to 10000 samples".to_string(),
        ));
    }
    let mut active = Vec::with_capacity(samples.len());
    let mut publication = Vec::with_capacity(samples.len());
    let mut mode_counts = std::collections::BTreeMap::<String, u64>::new();
    let mut limitations = std::collections::BTreeSet::new();
    let mut complete = 0_u64;
    let mut advanced = 0_u64;
    for sample in &samples {
        validate_composed_sample(sample)?;
        active.push(sample.active_processing_duration_milliseconds);
        publication.push(sample.follower.publication_to_checkpoint_milliseconds);
        *mode_counts
            .entry(acquisition_mode_name(sample.acquisition_mode).to_string())
            .or_default() += 1;
        complete = complete.saturating_add(u64::from(sample.restoration_complete));
        advanced = advanced.saturating_add(u64::from(sample.source_advanced));
        limitations.extend(sample.limitations.iter().cloned());
    }
    limitations.insert(LatencyEvidenceLimitation::SourcePersistenceStartNotObserved);
    limitations.insert(LatencyEvidenceLimitation::InterCommandDelayNotMeasured);
    limitations.insert(LatencyEvidenceLimitation::DisposableScenarioNotAttributed);
    Ok(LatencyEvidenceSummary {
        format_version: 1,
        privacy_safe_summary: true,
        sample_count: samples.len() as u64,
        complete_restoration_sample_count: complete,
        source_advanced_sample_count: advanced,
        acquisition_mode_counts: mode_counts,
        active_processing: percentiles(active)?,
        publication_to_checkpoint: percentiles(publication)?,
        every_publication_to_checkpoint_within_objective: samples
            .iter()
            .all(|sample| sample.publication_to_checkpoint_within_objective),
        text_freshness_objective_milliseconds: TEXT_FRESHNESS_OBJECTIVE_MILLISECONDS,
        full_end_to_end_objective_proven: false,
        limitations: limitations.into_iter().collect(),
    })
}

fn validate_stage_durations(
    snapshot: &SnapshotLatencyStages,
    offline: &OfflineLatencyStages,
    follower_apply: u64,
    publication_to_checkpoint: u64,
) -> Result<(), RestoreError> {
    let snapshot_components = snapshot
        .planning_duration_milliseconds
        .checked_add(snapshot.acquisition_duration_milliseconds)
        .ok_or_else(|| RestoreError::Integrity("snapshot duration overflowed".to_string()))?;
    let offline_components = offline
        .input_validation_duration_milliseconds
        .checked_add(offline.catalog_preparation_duration_milliseconds)
        .and_then(|value| value.checked_add(offline.restoration_duration_milliseconds))
        .and_then(|value| value.checked_add(offline.publication_validation_duration_milliseconds))
        .ok_or_else(|| RestoreError::Integrity("offline duration overflowed".to_string()))?;
    let durations = [
        snapshot.planning_duration_milliseconds,
        snapshot.acquisition_duration_milliseconds,
        snapshot.total_duration_milliseconds,
        offline.input_validation_duration_milliseconds,
        offline.catalog_preparation_duration_milliseconds,
        offline.restoration_duration_milliseconds,
        offline.publication_validation_duration_milliseconds,
        offline.total_duration_milliseconds,
        follower_apply,
        publication_to_checkpoint,
    ];
    if snapshot.total_duration_milliseconds < snapshot_components
        || offline.total_duration_milliseconds < offline_components
        || durations
            .iter()
            .any(|duration| *duration > MAX_STAGE_DURATION_MILLISECONDS)
    {
        return Err(RestoreError::Integrity(
            "latency evidence stage durations are inconsistent".to_string(),
        ));
    }
    Ok(())
}

fn validate_composed_sample(sample: &LatencyEvidenceSample) -> Result<(), RestoreError> {
    if sample.format_version != 1
        || !sample.privacy_safe_summary
        || sample.generation == 0
        || sample.text_freshness_objective_milliseconds != TEXT_FRESHNESS_OBJECTIVE_MILLISECONDS
        || sample.full_end_to_end_objective_proven
        || !sample.stage_bindings_verified
        || sample.publication_to_checkpoint_within_objective
            != (sample.follower.publication_to_checkpoint_milliseconds
                <= TEXT_FRESHNESS_OBJECTIVE_MILLISECONDS)
    {
        return Err(RestoreError::Integrity(
            "latency evidence sample is malformed".to_string(),
        ));
    }
    validate_stage_durations(
        &sample.snapshot,
        &sample.offline,
        sample.follower.apply_duration_milliseconds,
        sample.follower.publication_to_checkpoint_milliseconds,
    )?;
    let active = sample
        .snapshot
        .total_duration_milliseconds
        .checked_add(sample.offline.total_duration_milliseconds)
        .and_then(|value| value.checked_add(sample.follower.apply_duration_milliseconds))
        .ok_or_else(|| RestoreError::Integrity("latency sample duration overflowed".to_string()))?;
    let accounted = sample
        .restored_row_count
        .checked_add(sample.rejected_row_count)
        .ok_or_else(|| RestoreError::Integrity("latency sample rows overflowed".to_string()))?;
    if active != sample.active_processing_duration_milliseconds
        || accounted != sample.source_row_count
        || (sample.restoration_complete
            && (sample.rejected_row_count != 0
                || sample.semantic_gap_count != 0
                || sample.message_candidate_gap_count != 0
                || sample.missing_artifact_count != 0
                || sample.artifact_decode_gap_count != 0))
        || (!sample.restoration_complete
            && !sample
                .limitations
                .contains(&LatencyEvidenceLimitation::CompleteRestorationNotAchieved))
        || (!sample.source_advanced
            && !sample
                .limitations
                .contains(&LatencyEvidenceLimitation::SourceDidNotAdvance))
        || !sample
            .limitations
            .contains(&LatencyEvidenceLimitation::SourcePersistenceStartNotObserved)
        || !sample
            .limitations
            .contains(&LatencyEvidenceLimitation::InterCommandDelayNotMeasured)
        || !sample
            .limitations
            .contains(&LatencyEvidenceLimitation::DisposableScenarioNotAttributed)
    {
        return Err(RestoreError::Integrity(
            "latency evidence sample is internally inconsistent".to_string(),
        ));
    }
    Ok(())
}

fn percentiles(mut values: Vec<u64>) -> Result<LatencyPercentiles, RestoreError> {
    if values.is_empty() {
        return Err(RestoreError::Integrity(
            "latency percentile input is empty".to_string(),
        ));
    }
    values.sort_unstable();
    Ok(LatencyPercentiles {
        minimum_milliseconds: values[0],
        p50_milliseconds: nearest_rank(&values, 50),
        p95_milliseconds: nearest_rank(&values, 95),
        maximum_milliseconds: values[values.len() - 1],
    })
}

fn nearest_rank(values: &[u64], percentile: usize) -> u64 {
    let rank = values.len().saturating_mul(percentile).div_ceil(100);
    values[rank.saturating_sub(1).min(values.len() - 1)]
}

fn acquisition_mode_name(mode: SnapshotAcquisitionMode) -> &'static str {
    match mode {
        SnapshotAcquisitionMode::Bootstrap => "bootstrap",
        SnapshotAcquisitionMode::Incremental => "incremental",
        SnapshotAcquisitionMode::IntegrityScan => "integrityScan",
    }
}

fn read_private_json<T: serde::de::DeserializeOwned>(
    path: &Path,
    maximum_bytes: u64,
) -> Result<T, RestoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.nlink() != 1
        || metadata.len() == 0
        || metadata.len() > maximum_bytes
    {
        return Err(RestoreError::Integrity(
            "latency evidence input must be one bounded owner-only regular file".to_string(),
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let before = file.metadata()?;
    let mut bytes = Vec::with_capacity(before.len() as usize);
    let mut bounded = (&mut file).take(maximum_bytes + 1);
    bounded.read_to_end(&mut bytes)?;
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
            "latency evidence input changed during verification".to_string(),
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::PathBuf;

    #[test]
    fn composes_only_bound_aggregate_stage_evidence() {
        let fixture = private_fixture();
        let snapshot = fixture.join("snapshot.json");
        let offline = fixture.join("offline.json");
        let follower = fixture.join("follower.json");
        let handoff = fixture.join("handoff.json");
        write_private_json(&snapshot, &snapshot_json());
        write_private_json(&offline, &offline_json());
        write_private_json(&follower, &follower_json());
        write_private_json(&handoff, &handoff_json());

        let sample =
            compose_latency_evidence_sample(&snapshot, &offline, &follower, &handoff).unwrap();
        assert_eq!(sample.generation, 7);
        assert_eq!(sample.active_processing_duration_milliseconds, 1_500);
        assert!(sample.publication_to_checkpoint_within_objective);
        assert!(!sample.full_end_to_end_objective_proven);
        assert_eq!(sample.limitations.len(), 3);
        let serialized = serde_json::to_string(&sample).unwrap();
        for private in ["private-source", "/private/snapshot", "/private/archive"] {
            assert!(!serialized.contains(private));
        }
    }

    #[test]
    fn rejects_cross_generation_incomplete_and_inconsistent_inputs() {
        let fixture = private_fixture();
        let snapshot = fixture.join("snapshot.json");
        let offline = fixture.join("offline.json");
        let follower = fixture.join("follower.json");
        let handoff = fixture.join("handoff.json");
        write_private_json(&snapshot, &snapshot_json());
        write_private_json(&offline, &offline_json());
        write_private_json(&follower, &follower_json());
        let mut wrong_handoff = handoff_json();
        wrong_handoff["generation"] = serde_json::json!(8);
        write_private_json(&handoff, &wrong_handoff);
        assert!(compose_latency_evidence_sample(&snapshot, &offline, &follower, &handoff).is_err());

        let mut wrong_offline = offline_json();
        wrong_offline["generation"] = serde_json::json!(8);
        overwrite_private_json(&offline, &wrong_offline);
        let mut matching_handoff = handoff_json();
        matching_handoff["generation"] = serde_json::json!(8);
        overwrite_private_json(&handoff, &matching_handoff);
        assert!(compose_latency_evidence_sample(&snapshot, &offline, &follower, &handoff).is_err());

        overwrite_private_json(&offline, &offline_json());
        overwrite_private_json(&handoff, &handoff_json());
        let mut cleaned_snapshot = snapshot_json();
        cleaned_snapshot["automaticallyCleanedUp"] = serde_json::json!(true);
        overwrite_private_json(&snapshot, &cleaned_snapshot);
        assert!(compose_latency_evidence_sample(&snapshot, &offline, &follower, &handoff).is_err());
    }

    #[test]
    fn summarizes_nearest_rank_without_claiming_end_to_end_latency() {
        let fixture = private_fixture();
        let samples_path = fixture.join("samples.json");
        let mut samples = Vec::new();
        for value in 1..=20_u64 {
            let mut sample = sample_fixture();
            sample.follower.publication_to_checkpoint_milliseconds = value * 1_000;
            sample.publication_to_checkpoint_within_objective = true;
            sample.snapshot.total_duration_milliseconds = value;
            sample.snapshot.planning_duration_milliseconds = 0;
            sample.snapshot.acquisition_duration_milliseconds = value;
            sample.active_processing_duration_milliseconds = value
                + sample.offline.total_duration_milliseconds
                + sample.follower.apply_duration_milliseconds;
            samples.push(sample);
        }
        write_private_json(&samples_path, &samples);
        let summary = summarize_latency_evidence_samples(&samples_path).unwrap();
        assert_eq!(summary.sample_count, 20);
        assert_eq!(summary.publication_to_checkpoint.p50_milliseconds, 10_000);
        assert_eq!(summary.publication_to_checkpoint.p95_milliseconds, 19_000);
        assert!(!summary.full_end_to_end_objective_proven);
        assert_eq!(summary.limitations.len(), 3);
    }

    #[test]
    fn refuses_non_private_or_symlinked_reports() {
        let fixture = private_fixture();
        let report = fixture.join("report.json");
        write_private_json(&report, &snapshot_json());
        fs::set_permissions(&report, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_private_json::<serde_json::Value>(&report, MAX_PRIVATE_REPORT_BYTES).is_err());
        fs::remove_file(&report).unwrap();
        std::os::unix::fs::symlink("missing", &report).unwrap();
        assert!(read_private_json::<serde_json::Value>(&report, MAX_PRIVATE_REPORT_BYTES).is_err());
    }

    fn sample_fixture() -> LatencyEvidenceSample {
        LatencyEvidenceSample {
            format_version: 1,
            privacy_safe_summary: true,
            acquisition_mode: SnapshotAcquisitionMode::Incremental,
            generation: 1,
            snapshot: SnapshotLatencyStages {
                planning_duration_milliseconds: 10,
                acquisition_duration_milliseconds: 20,
                total_duration_milliseconds: 30,
            },
            offline: OfflineLatencyStages {
                input_validation_duration_milliseconds: 10,
                catalog_preparation_duration_milliseconds: 20,
                restoration_duration_milliseconds: 30,
                publication_validation_duration_milliseconds: 40,
                total_duration_milliseconds: 100,
            },
            follower: FollowerLatencyStages {
                apply_duration_milliseconds: 50,
                publication_to_checkpoint_milliseconds: 1_000,
            },
            active_processing_duration_milliseconds: 180,
            restoration_complete: true,
            source_advanced: true,
            source_row_count: 1,
            restored_row_count: 1,
            rejected_row_count: 0,
            semantic_gap_count: 0,
            message_candidate_gap_count: 0,
            missing_artifact_count: 0,
            artifact_decode_gap_count: 0,
            publication_to_checkpoint_within_objective: true,
            text_freshness_objective_milliseconds: 60_000,
            full_end_to_end_objective_proven: false,
            limitations: vec![
                LatencyEvidenceLimitation::SourcePersistenceStartNotObserved,
                LatencyEvidenceLimitation::InterCommandDelayNotMeasured,
                LatencyEvidenceLimitation::DisposableScenarioNotAttributed,
            ],
            stage_bindings_verified: true,
        }
    }

    fn private_fixture() -> PathBuf {
        let path = tempfile::tempdir().unwrap().keep();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn write_private_json(path: &Path, value: &impl Serialize) {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        serde_json::to_writer_pretty(&mut file, value).unwrap();
        file.write_all(b"\n").unwrap();
    }

    fn overwrite_private_json(path: &Path, value: &impl Serialize) {
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        serde_json::to_writer_pretty(&mut file, value).unwrap();
        file.write_all(b"\n").unwrap();
    }

    fn snapshot_json() -> serde_json::Value {
        serde_json::json!({
            "formatVersion": 2,
            "databaseSetCount": 3,
            "manifest": {
                "manifestFormatVersion": 3,
                "sourceFingerprint": "private-source",
                "acquisition": { "mode": "incremental" },
                "privatePath": "/private/snapshot"
            },
            "automaticallyCleanedUp": false,
            "snapshotDirectory": "/private/snapshot",
            "planningDurationMilliseconds": 100,
            "acquisitionDurationMilliseconds": 200,
            "totalDurationMilliseconds": 300
        })
    }

    fn offline_json() -> serde_json::Value {
        serde_json::json!({
            "formatVersion": 2,
            "privacySafeSummary": true,
            "acquisitionMode": "incremental",
            "previousChainVerified": true,
            "previousArchiveVerified": true,
            "incrementalFragmentVerified": true,
            "authoritativeArchiveVerified": true,
            "generation": 7,
            "fullRestorationAchieved": true,
            "sourceRowCount": 10,
            "restoredRowCount": 10,
            "rejectedRowCount": 0,
            "semanticGapCount": 0,
            "messageCandidateGapCount": 0,
            "missingArtifactCount": 0,
            "artifactDecodeGapCount": 0,
            "inputValidationDurationMilliseconds": 100,
            "catalogPreparationDurationMilliseconds": 200,
            "restorationDurationMilliseconds": 300,
            "publicationValidationDurationMilliseconds": 100,
            "totalDurationMilliseconds": 700
        })
    }

    fn follower_json() -> serde_json::Value {
        serde_json::json!({
            "formatVersion": 2,
            "privacySafeSummary": true,
            "generation": 7,
            "outcome": "synchronized",
            "sourceAdvanced": true,
            "restorationComplete": true,
            "applyDurationMilliseconds": 500,
            "publicationToCheckpointMilliseconds": 800,
            "privatePath": "/private/archive"
        })
    }

    fn handoff_json() -> serde_json::Value {
        serde_json::json!({
            "formatVersion": 3,
            "generation": 7,
            "sourceFingerprint": "private-source",
            "publishedAtUnixNanoseconds": "1000000000",
            "archiveDirectory": "/private/archive"
        })
    }
}
