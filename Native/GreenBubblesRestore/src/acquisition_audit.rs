use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::archive::{ensure_private_directory, ensure_private_regular_file};
use crate::manifest::{SnapshotAcquisitionMode, SnapshotManifest, SnapshotSourceSetInventory};
use crate::{preflight_snapshot, RestoreError};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcquisitionChainAuditReport {
    pub format_version: u32,
    pub privacy_safe_summary: bool,
    pub chain_verified: bool,
    pub account_binding_unchanged: bool,
    pub client_build_unchanged: bool,
    pub previous_client_build_production_compatible: bool,
    pub current_client_build_production_compatible: bool,
    pub mode: SnapshotAcquisitionMode,
    pub previous_source_set_count: u64,
    pub current_source_set_count: u64,
    pub reported_changed_source_set_count: u64,
    pub content_changed_source_set_count: u64,
    pub reconciliation_source_set_count: u64,
    pub deleted_source_set_count: u64,
    pub previous_copied_entry_count: u64,
    pub current_copied_entry_count: u64,
    pub current_copied_database_count: u64,
    pub current_copied_database_passphrase_required: bool,
    pub baseline_fingerprint_matches: bool,
    pub deletion_classification_exact: bool,
    pub incremental_change_classification_exact: bool,
    pub reconciliation_sets_unchanged: bool,
}

pub fn audit_acquisition_chain(
    previous_snapshot: &Path,
    current_snapshot: &Path,
) -> Result<AcquisitionChainAuditReport, RestoreError> {
    for snapshot in [previous_snapshot, current_snapshot] {
        ensure_private_directory(snapshot)?;
        ensure_private_regular_file(&snapshot.join("manifest.json"))?;
    }
    preflight_snapshot(previous_snapshot)?;
    let current_preflight = preflight_snapshot(current_snapshot)?;
    let previous = SnapshotManifest::load(previous_snapshot)?;
    let current = SnapshotManifest::load(current_snapshot)?;
    let previous_acquisition = previous
        .acquisition
        .as_ref()
        .ok_or_else(|| integrity("previous snapshot has no authoritative source inventory"))?;
    let current_acquisition = current
        .acquisition
        .as_ref()
        .ok_or_else(|| integrity("current snapshot has no acquisition evidence"))?;
    let account_binding_unchanged = previous.account_binding == current.account_binding;
    if !account_binding_unchanged {
        return Err(integrity(
            "current acquisition account binding does not match the supplied baseline",
        ));
    }
    if current_acquisition.mode == SnapshotAcquisitionMode::Bootstrap {
        return Err(integrity(
            "a bootstrap snapshot cannot be audited as a continuation",
        ));
    }
    let baseline_fingerprint_matches = current_acquisition.previous_source_fingerprint.as_deref()
        == Some(previous.source_fingerprint.as_str());
    if !baseline_fingerprint_matches {
        return Err(integrity(
            "current acquisition does not continue the supplied baseline",
        ));
    }
    let client_build_unchanged = current.client_build == previous.client_build;
    let previous_compatibility = previous.client_build_compatibility();
    let current_compatibility = current.client_build_compatibility();
    if !previous_compatibility.production_compatible || !current_compatibility.production_compatible
    {
        return Err(integrity(
            "acquisition chain contains a client outside the signed WeChat 4.1+ compatibility family",
        ));
    }

    let previous_sets = keyed_inventory(&previous_acquisition.source_sets)?;
    let current_sets = keyed_inventory(&current_acquisition.source_sets)?;
    let previous_ids = previous_sets.keys().copied().collect::<BTreeSet<_>>();
    let current_ids = current_sets.keys().copied().collect::<BTreeSet<_>>();
    let derived_deleted = previous_ids
        .difference(&current_ids)
        .copied()
        .collect::<BTreeSet<_>>();
    let reported_deleted = current_acquisition
        .deleted_source_set_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let deletion_classification_exact = derived_deleted == reported_deleted;
    if !deletion_classification_exact {
        return Err(integrity(
            "deleted source-set classification does not match the baseline",
        ));
    }

    let content_changed = current_sets
        .iter()
        .filter_map(|(identifier, current_set)| {
            (previous_sets.get(identifier).copied() != Some(*current_set)).then_some(*identifier)
        })
        .collect::<BTreeSet<_>>();
    let reported_changed = current_acquisition
        .changed_source_set_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let incremental_change_classification_exact = current_acquisition.mode
        != SnapshotAcquisitionMode::Incremental
        || content_changed == reported_changed;
    if !incremental_change_classification_exact {
        return Err(integrity(
            "incremental changed-set classification does not match source inventories",
        ));
    }

    let reconciliation = current_acquisition
        .reconciliation_source_set_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let reconciliation_sets_unchanged = reconciliation.iter().all(|identifier| {
        previous_sets.get(identifier).copied() == current_sets.get(identifier).copied()
    });
    if !reconciliation_sets_unchanged {
        return Err(integrity(
            "a reconciliation-only source set changed relative to the baseline",
        ));
    }

    Ok(AcquisitionChainAuditReport {
        format_version: 3,
        privacy_safe_summary: true,
        chain_verified: true,
        account_binding_unchanged,
        client_build_unchanged,
        previous_client_build_production_compatible: previous_compatibility.production_compatible,
        current_client_build_production_compatible: current_compatibility.production_compatible,
        mode: current_acquisition.mode,
        previous_source_set_count: previous_sets.len() as u64,
        current_source_set_count: current_sets.len() as u64,
        reported_changed_source_set_count: reported_changed.len() as u64,
        content_changed_source_set_count: content_changed.len() as u64,
        reconciliation_source_set_count: reconciliation.len() as u64,
        deleted_source_set_count: reported_deleted.len() as u64,
        previous_copied_entry_count: previous.entries.len() as u64,
        current_copied_entry_count: current.entries.len() as u64,
        current_copied_database_count: current_preflight.copied_database_count as u64,
        current_copied_database_passphrase_required: current_preflight
            .copied_database_passphrase_required,
        baseline_fingerprint_matches,
        deletion_classification_exact,
        incremental_change_classification_exact,
        reconciliation_sets_unchanged,
    })
}

fn keyed_inventory(
    source_sets: &[SnapshotSourceSetInventory],
) -> Result<BTreeMap<&str, &SnapshotSourceSetInventory>, RestoreError> {
    let mut result = BTreeMap::new();
    for source_set in source_sets {
        if result
            .insert(source_set.source_set_id.as_str(), source_set)
            .is_some()
        {
            return Err(integrity(
                "acquisition inventory contains a duplicate source set",
            ));
        }
    }
    Ok(result)
}

fn integrity(message: impl Into<String>) -> RestoreError {
    RestoreError::Integrity(message.into())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use sha2::{Digest, Sha256};

    use super::audit_acquisition_chain;
    use crate::manifest::{
        source_inventory_fingerprint, source_inventory_fingerprint_with_account_binding,
        supported_client_build, PathReference, SnapshotAccountBinding,
        SnapshotAccountBindingEvidence, SnapshotAcquisitionEvidence, SnapshotAcquisitionMode,
        SnapshotEntry, SnapshotFileRole, SnapshotManifest, SnapshotSourceFileInventory,
        SnapshotSourceSetInventory, SourceFileFingerprint,
    };

    #[test]
    fn verifies_exact_incremental_change_classification() {
        let fixture = tempfile::tempdir().unwrap();
        let previous = fixture.path().join("previous");
        let current = fixture.path().join("current");
        write_snapshot(&previous, None, b"previous", true);
        let previous_manifest = SnapshotManifest::load(&previous).unwrap();
        write_snapshot(
            &current,
            Some(previous_manifest.source_fingerprint.clone()),
            b"current",
            true,
        );
        let report = audit_acquisition_chain(&previous, &current).unwrap();
        assert!(report.chain_verified);
        assert!(report.account_binding_unchanged);
        assert_eq!(report.reported_changed_source_set_count, 1);
        assert_eq!(report.content_changed_source_set_count, 1);

        let mut updated_client = SnapshotManifest::load(&current).unwrap();
        let build = updated_client.client_build.as_mut().unwrap();
        build.marketing_version = "4.1.13".to_string();
        build.build_version = "new-build".to_string();
        build.executable_sha256 = "0".repeat(64);
        build.code_directory_sha256 = "1".repeat(64);
        fs::write(
            current.join("manifest.json"),
            serde_json::to_vec_pretty(&updated_client).unwrap(),
        )
        .unwrap();
        let updated_report = audit_acquisition_chain(&previous, &current).unwrap();
        assert!(updated_report.chain_verified);
        assert!(!updated_report.client_build_unchanged);
        assert!(updated_report.previous_client_build_production_compatible);
        assert!(updated_report.current_client_build_production_compatible);

        write_snapshot(
            &current,
            Some(previous_manifest.source_fingerprint),
            b"current",
            false,
        );
        assert!(audit_acquisition_chain(&previous, &current)
            .unwrap_err()
            .to_string()
            .contains("changed-set classification"));
    }

    #[test]
    fn rejects_a_continuation_with_a_different_account_binding() {
        let fixture = tempfile::tempdir().unwrap();
        let previous = fixture.path().join("previous-bound");
        let current = fixture.path().join("current-bound");
        write_snapshot(&previous, None, b"previous", true);
        let previous_manifest = bind_snapshot(
            &previous,
            SnapshotAccountBinding {
                format_version: 1,
                account_id: "a".repeat(64),
                self_source_identifier_base64: "d3hpZF9maXh0dXJl".to_string(),
                evidence: SnapshotAccountBindingEvidence::SelectedAccountDirectory,
            },
            None,
        );
        write_snapshot(
            &current,
            Some(previous_manifest.source_fingerprint.clone()),
            b"current",
            true,
        );
        bind_snapshot(
            &current,
            SnapshotAccountBinding {
                format_version: 1,
                account_id: "b".repeat(64),
                self_source_identifier_base64: "d3hpZF9vdGhlcg==".to_string(),
                evidence: SnapshotAccountBindingEvidence::SelectedAccountDirectory,
            },
            Some(previous_manifest.source_fingerprint),
        );

        assert!(audit_acquisition_chain(&previous, &current)
            .unwrap_err()
            .to_string()
            .contains("account binding"));
    }

    fn bind_snapshot(
        directory: &Path,
        binding: SnapshotAccountBinding,
        previous_fingerprint: Option<String>,
    ) -> SnapshotManifest {
        let mut manifest = SnapshotManifest::load(directory).unwrap();
        manifest.manifest_format_version = 4;
        manifest.account_binding = Some(binding.clone());
        let acquisition = manifest.acquisition.as_mut().unwrap();
        acquisition.previous_source_fingerprint = previous_fingerprint;
        manifest.source_fingerprint =
            source_inventory_fingerprint_with_account_binding(&acquisition.source_sets, &binding);
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        manifest
    }

    fn write_snapshot(
        directory: &Path,
        previous_fingerprint: Option<String>,
        marker: &[u8],
        select_changed: bool,
    ) {
        if !directory.exists() {
            fs::create_dir(directory).unwrap();
        }
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        let relative_path = "sets/0000/database.db";
        let database_path = directory.join(relative_path);
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let mut bytes = b"SQLite format 3\0".to_vec();
        bytes.extend_from_slice(marker);
        fs::write(&database_path, &bytes).unwrap();
        fs::set_permissions(&database_path, fs::Permissions::from_mode(0o600)).unwrap();
        let fingerprint = SourceFileFingerprint {
            device_id: 1,
            file_id: 1,
            byte_count: bytes.len() as i64,
            modified_seconds: 1,
            modified_nanoseconds: 0,
        };
        let digest = hex::encode(Sha256::digest(&bytes));
        let source_set = SnapshotSourceSetInventory {
            source_set_id: "set-a".to_string(),
            logical_path: "message/message_0.db".to_string(),
            files: vec![SnapshotSourceFileInventory {
                role: SnapshotFileRole::Database,
                fingerprint: fingerprint.clone(),
                content_sha256: Some(digest.clone()),
            }],
        };
        let source_sets = vec![source_set];
        let is_bootstrap = previous_fingerprint.is_none();
        let selected = is_bootstrap || select_changed;
        let manifest = SnapshotManifest {
            manifest_format_version: 3,
            snapshot_id: "synthetic-snapshot".to_string(),
            created_at: "2026-08-27T00:00:00Z".to_string(),
            source_fingerprint: source_inventory_fingerprint(&source_sets),
            account_binding: None,
            client_build: Some(supported_client_build()),
            acquisition: Some(SnapshotAcquisitionEvidence {
                format_version: 2,
                mode: if is_bootstrap {
                    SnapshotAcquisitionMode::Bootstrap
                } else {
                    SnapshotAcquisitionMode::Incremental
                },
                previous_source_fingerprint: previous_fingerprint,
                reconciliation_window_seconds: 900,
                changed_source_set_ids: selected.then(|| "set-a".to_string()).into_iter().collect(),
                reconciliation_source_set_ids: Vec::new(),
                deleted_source_set_ids: Vec::new(),
                source_sets,
                last_integrity_scan_at: Some("2026-08-27T00:00:00Z".to_string()),
            }),
            entries: selected
                .then(|| SnapshotEntry {
                    source: PathReference {
                        opaque_id: "synthetic-source".to_string(),
                        path: None,
                    },
                    source_set_id: "set-a".to_string(),
                    logical_path: "message/message_0.db".to_string(),
                    relative_path: relative_path.to_string(),
                    role: SnapshotFileRole::Database,
                    fingerprint,
                    sha256: digest,
                })
                .into_iter()
                .collect(),
        };
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        fs::set_permissions(
            directory.join("manifest.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
}
