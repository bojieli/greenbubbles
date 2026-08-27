use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::acquisition_audit::audit_acquisition_chain;
use crate::archive::{ensure_private_directory, load_report};
use crate::audit::audit_archive;
use crate::follow::{capture_publication_predecessor, publish_replica_handoff_next_if_current};
use crate::merge::merge_incremental_archive;
use crate::{
    prepare_catalog, restore_catalog, DatabasePassphrase, RestorationArchiveScope,
    RestorationMediaPhase, RestorationOptions, RestoreError, SnapshotAcquisitionMode,
    SnapshotManifest,
};

#[derive(Debug, Clone)]
pub struct OfflineRestorePublishOptions {
    pub output_archive: PathBuf,
    pub handoff_path: PathBuf,
    pub previous_snapshot: Option<PathBuf>,
    pub previous_archive: Option<PathBuf>,
    pub account_root: Option<PathBuf>,
    pub defer_media: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OfflineRestorePublishReport {
    pub format_version: u32,
    pub privacy_safe_summary: bool,
    pub acquisition_mode: SnapshotAcquisitionMode,
    pub previous_chain_verified: bool,
    pub previous_archive_verified: bool,
    pub incremental_fragment_verified: bool,
    pub authoritative_archive_verified: bool,
    pub generation: u64,
    pub media_phase: RestorationMediaPhase,
    pub full_restoration_achieved: bool,
    pub source_row_count: u64,
    pub restored_row_count: u64,
    pub rejected_row_count: u64,
    pub semantic_gap_count: u64,
    pub message_candidate_gap_count: u64,
    pub missing_artifact_count: u64,
    pub artifact_decode_gap_count: u64,
    pub input_validation_duration_milliseconds: u64,
    pub catalog_preparation_duration_milliseconds: u64,
    pub restoration_duration_milliseconds: u64,
    pub publication_validation_duration_milliseconds: u64,
    pub total_duration_milliseconds: u64,
}

pub fn restore_snapshot_and_publish(
    snapshot: &Path,
    options: &OfflineRestorePublishOptions,
    passphrase: Option<&DatabasePassphrase>,
) -> Result<OfflineRestorePublishReport, RestoreError> {
    let total_started = Instant::now();
    let manifest = SnapshotManifest::load(snapshot)?;
    let manifest_binding = serde_json::to_vec(&manifest)?;
    let compatibility = manifest.client_build_compatibility();
    if !compatibility.production_compatible {
        return Err(RestoreError::Integrity(
            "offline publication requires the exact pinned production client build".to_string(),
        ));
    }
    let acquisition = manifest.acquisition.as_ref().ok_or_else(|| {
        RestoreError::Integrity(
            "offline publication requires complete snapshot acquisition evidence".to_string(),
        )
    })?;

    let output_parent = options
        .output_archive
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath("output archive has no parent".to_string()))?;
    let output_parent = if output_parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        output_parent
    };
    ensure_private_directory(output_parent)?;
    if fs::symlink_metadata(&options.output_archive).is_ok() {
        return Err(RestoreError::Integrity(
            "offline publication output archive already exists".to_string(),
        ));
    }

    let requires_previous = acquisition.previous_source_fingerprint.is_some();
    if requires_previous
        && (options.previous_snapshot.is_none() || options.previous_archive.is_none())
    {
        return Err(RestoreError::Integrity(
            "non-bootstrap publication requires both the previous snapshot and authoritative archive"
                .to_string(),
        ));
    } else if !requires_previous
        && (options.previous_snapshot.is_some() || options.previous_archive.is_some())
    {
        return Err(RestoreError::Integrity(
            "bootstrap publication must not claim a previous snapshot or archive".to_string(),
        ));
    }

    let mut previous_chain_verified = false;
    let mut previous_archive_verified = false;
    if let (Some(previous_snapshot), Some(previous_archive)) = (
        options.previous_snapshot.as_deref(),
        options.previous_archive.as_deref(),
    ) {
        audit_acquisition_chain(previous_snapshot, snapshot)?;
        let previous_audit = audit_archive(previous_archive)?;
        if previous_audit.archive_scope != RestorationArchiveScope::Authoritative {
            return Err(RestoreError::Integrity(
                "previous publication input is not an authoritative archive".to_string(),
            ));
        }
        let previous_report = load_report(previous_archive)?;
        if acquisition.previous_source_fingerprint.as_deref()
            != Some(previous_report.source_fingerprint.as_str())
        {
            return Err(RestoreError::Integrity(
                "previous authoritative archive does not match the snapshot baseline".to_string(),
            ));
        }
        previous_chain_verified = true;
        previous_archive_verified = true;
    }
    let publication_predecessor = capture_publication_predecessor(
        &options.handoff_path,
        options.previous_archive.as_deref(),
    )?;
    let input_validation_duration_milliseconds = elapsed_milliseconds(total_started);

    let catalog_started = Instant::now();
    let catalog = prepare_catalog(snapshot, passphrase)?;
    if serde_json::to_vec(&catalog.manifest)? != manifest_binding {
        return Err(RestoreError::Integrity(
            "snapshot manifest changed during offline publication".to_string(),
        ));
    }
    let catalog_preparation_duration_milliseconds = elapsed_milliseconds(catalog_started);
    let restoration_started = Instant::now();
    let mut incremental_fragment_verified = false;
    match acquisition.mode {
        SnapshotAcquisitionMode::Incremental => {
            let staging = tempfile::Builder::new()
                .prefix(".greenbubbles-offline-publish-")
                .tempdir_in(output_parent)?;
            fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))?;
            let fragment = staging.path().join("incremental-fragment");
            let fragment_report = restore_catalog(
                &catalog,
                &RestorationOptions {
                    output_directory: fragment.clone(),
                    account_root: options.account_root.clone(),
                    defer_media: options.defer_media,
                },
            )?;
            if fragment_report.archive_scope != RestorationArchiveScope::IncrementalFragment {
                return Err(RestoreError::Integrity(
                    "incremental snapshot did not restore as a bounded fragment".to_string(),
                ));
            }
            audit_archive(&fragment)?;
            incremental_fragment_verified = true;
            merge_incremental_archive(
                options.previous_archive.as_deref().ok_or_else(|| {
                    RestoreError::Integrity(
                        "incremental publication has no previous archive".to_string(),
                    )
                })?,
                &fragment,
                &options.output_archive,
            )?;
        }
        SnapshotAcquisitionMode::Bootstrap | SnapshotAcquisitionMode::IntegrityScan => {
            let report = restore_catalog(
                &catalog,
                &RestorationOptions {
                    output_directory: options.output_archive.clone(),
                    account_root: options.account_root.clone(),
                    defer_media: options.defer_media,
                },
            )?;
            if report.archive_scope != RestorationArchiveScope::Authoritative {
                return Err(RestoreError::Integrity(
                    "full snapshot did not restore as an authoritative archive".to_string(),
                ));
            }
        }
    }
    let restoration_duration_milliseconds = elapsed_milliseconds(restoration_started);

    let publication_started = Instant::now();
    let archive_audit = audit_archive(&options.output_archive)?;
    if archive_audit.archive_scope != RestorationArchiveScope::Authoritative
        || !archive_audit.report_matches_archive
        || !archive_audit.all_artifact_references_resolve
        || !archive_audit.all_resolved_relationships_resolve
        || !archive_audit.all_recorded_artifact_files_match
    {
        return Err(RestoreError::Integrity(
            "restored archive did not satisfy the independent publication audit".to_string(),
        ));
    }
    let report = load_report(&options.output_archive)?;
    let receipt = publish_replica_handoff_next_if_current(
        &options.output_archive,
        &options.handoff_path,
        &publication_predecessor,
    )?;
    let publication_validation_duration_milliseconds = elapsed_milliseconds(publication_started);
    Ok(OfflineRestorePublishReport {
        format_version: 2,
        privacy_safe_summary: true,
        acquisition_mode: acquisition.mode,
        previous_chain_verified,
        previous_archive_verified,
        incremental_fragment_verified,
        authoritative_archive_verified: true,
        generation: receipt.generation,
        media_phase: report.media_phase,
        full_restoration_achieved: report.completion.full_restoration_achieved,
        source_row_count: report.integrity.source_row_count,
        restored_row_count: report.integrity.restored_row_count,
        rejected_row_count: report.integrity.rejected_row_count,
        semantic_gap_count: report.integrity.semantic_gap_count,
        message_candidate_gap_count: report.integrity.message_candidate_gap_count,
        missing_artifact_count: report.integrity.missing_artifact_count,
        artifact_decode_gap_count: report.integrity.artifact_decode_gap_count,
        input_validation_duration_milliseconds,
        catalog_preparation_duration_milliseconds,
        restoration_duration_milliseconds,
        publication_validation_duration_milliseconds,
        total_duration_milliseconds: elapsed_milliseconds(total_started),
    })
}

fn elapsed_milliseconds(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
