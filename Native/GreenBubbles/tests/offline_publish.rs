use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use greenbubbles::operator::{restore_snapshot_and_publish, OfflineRestorePublishOptions};
use greenbubbles::{
    audit::audit_archive, ClientBuildFingerprint, SnapshotAcquisitionEvidence,
    SnapshotAcquisitionMode, SnapshotEntry, SnapshotFileRole, SnapshotManifest,
    SnapshotSourceFileInventory, SnapshotSourceSetInventory,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

#[test]
fn restores_audits_merges_and_monotonically_publishes_offline_snapshots() {
    let fixture = tempfile::tempdir().unwrap();
    let private = fixture.path().join("private");
    fs::create_dir(&private).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    let account_root = private.join("account-root");
    fs::create_dir(&account_root).unwrap();
    fs::set_permissions(&account_root, fs::Permissions::from_mode(0o700)).unwrap();
    for child in ["db_storage", "msg"] {
        let path = account_root.join(child);
        fs::create_dir(&path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    let bootstrap_snapshot = build_bootstrap_snapshot(&private, &account_root);
    let preflight_output = Command::new(env!("CARGO_BIN_EXE_greenbubbles"))
        .arg("preflight")
        .arg(&bootstrap_snapshot)
        .arg("--progress-json")
        .output()
        .unwrap();
    assert!(
        preflight_output.status.success(),
        "{}",
        String::from_utf8_lossy(&preflight_output.stderr)
    );
    let preflight_events = String::from_utf8(preflight_output.stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(preflight_events.iter().any(|event| {
        event["fileIndex"].as_u64().is_some()
            && event["fileCount"].as_u64().is_some()
            && event["fileByteCount"].as_u64().is_some()
    }));
    assert_eq!(
        preflight_events.last().unwrap()["workflowCompleted"],
        preflight_events.last().unwrap()["workflowTotal"]
    );

    let probe_progress_path = private.join("probe-progress.ndjson");
    let probe_output = Command::new(env!("CARGO_BIN_EXE_greenbubbles"))
        .arg("probe")
        .arg(&bootstrap_snapshot)
        .arg("--progress-json")
        .arg("--progress-file")
        .arg(&probe_progress_path)
        .output()
        .unwrap();
    assert!(
        probe_output.status.success(),
        "{}",
        String::from_utf8_lossy(&probe_output.stderr)
    );
    let probe_report: serde_json::Value = serde_json::from_slice(&probe_output.stdout).unwrap();
    assert!(probe_report["databaseCount"].as_u64().is_some());
    let progress_events = String::from_utf8(probe_output.stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(!progress_events.is_empty());
    assert!(progress_events.iter().all(|event| {
        event["formatVersion"] == 3
            && event["privacySafe"] == true
            && event["workflowCompleted"].as_u64().is_some()
            && event["workflowTotal"].as_u64().is_some()
            && event["phaseCompleted"].as_u64().is_some()
            && event.get("overallCompleted").is_none()
    }));
    let final_progress = progress_events.last().unwrap();
    assert_eq!(
        final_progress["workflowCompleted"],
        final_progress["workflowTotal"]
    );
    let retained_progress = fs::read_to_string(&probe_progress_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(retained_progress, progress_events);
    assert_eq!(
        fs::metadata(&probe_progress_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0
    );

    let bootstrap_archive = private.join("archive-bootstrap");
    let handoff = private.join("handoff.json");
    let first = restore_snapshot_and_publish(
        &bootstrap_snapshot,
        &OfflineRestorePublishOptions {
            output_archive: bootstrap_archive.clone(),
            handoff_path: handoff.clone(),
            previous_snapshot: None,
            previous_archive: None,
            account_root: Some(account_root.clone()),
            defer_media: false,
        },
        None,
    )
    .unwrap();
    assert_eq!(first.acquisition_mode, SnapshotAcquisitionMode::Bootstrap);
    assert_eq!(first.generation, 1);
    assert!(first.authoritative_archive_verified);
    assert!(!first.previous_chain_verified);
    assert!(audit_archive(&bootstrap_archive).is_ok());

    let incremental_snapshot =
        build_incremental_snapshot(&private, &bootstrap_snapshot, &account_root);
    let incremental_archive = private.join("archive-incremental");
    let output = Command::new(env!("CARGO_BIN_EXE_greenbubbles"))
        .arg("restore-publish")
        .args([&incremental_snapshot, &incremental_archive, &handoff])
        .args(["--previous-snapshot", bootstrap_snapshot.to_str().unwrap()])
        .args(["--previous-archive", bootstrap_archive.to_str().unwrap()])
        .args(["--account-root", account_root.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let human_progress = String::from_utf8_lossy(&output.stderr);
    assert!(human_progress.contains("workflow "));
    assert!(human_progress.contains("phase "));
    assert!(human_progress.contains("auditArchiveLedger"));
    assert!(human_progress.contains("workflow 100.0%"));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["formatVersion"], 2);
    assert_eq!(report["acquisitionMode"], "incremental");
    assert_eq!(report["generation"], 2);
    assert_eq!(report["previousChainVerified"], true);
    assert_eq!(report["previousArchiveVerified"], true);
    assert_eq!(report["incrementalFragmentVerified"], true);
    assert_eq!(report["authoritativeArchiveVerified"], true);
    assert_eq!(report["privacySafeSummary"], true);
    for field in [
        "inputValidationDurationMilliseconds",
        "catalogPreparationDurationMilliseconds",
        "restorationDurationMilliseconds",
        "publicationValidationDurationMilliseconds",
        "totalDurationMilliseconds",
    ] {
        assert!(report[field].as_u64().is_some());
    }
    let output_text = String::from_utf8(output.stdout).unwrap();
    for private_value in [
        bootstrap_snapshot.to_str().unwrap(),
        incremental_snapshot.to_str().unwrap(),
        bootstrap_archive.to_str().unwrap(),
        incremental_archive.to_str().unwrap(),
        account_root.to_str().unwrap(),
    ] {
        assert!(!output_text.contains(private_value));
    }
    assert!(audit_archive(&incremental_archive).is_ok());
    let handoff_json: serde_json::Value =
        serde_json::from_slice(&fs::read(&handoff).unwrap()).unwrap();
    assert_eq!(handoff_json["generation"], 2);
    assert!(handoff_json["publishedAtUnixNanoseconds"]
        .as_str()
        .is_some_and(|value| value.parse::<u128>().is_ok()));

    let integrity_snapshot = build_integrity_snapshot(
        &private,
        &bootstrap_snapshot,
        &incremental_snapshot,
        &account_root,
    );
    let integrity_archive = private.join("archive-integrity");
    let integrity = restore_snapshot_and_publish(
        &integrity_snapshot,
        &OfflineRestorePublishOptions {
            output_archive: integrity_archive.clone(),
            handoff_path: handoff.clone(),
            previous_snapshot: Some(incremental_snapshot.clone()),
            previous_archive: Some(incremental_archive.clone()),
            account_root: Some(account_root.clone()),
            defer_media: false,
        },
        None,
    )
    .unwrap();
    assert_eq!(
        integrity.acquisition_mode,
        SnapshotAcquisitionMode::IntegrityScan
    );
    assert_eq!(integrity.generation, 3);
    assert!(integrity.previous_chain_verified);
    assert!(!integrity.incremental_fragment_verified);
    assert!(audit_archive(&integrity_archive).is_ok());

    let rejected_output = private.join("must-not-exist");
    let error = restore_snapshot_and_publish(
        &incremental_snapshot,
        &OfflineRestorePublishOptions {
            output_archive: rejected_output.clone(),
            handoff_path: handoff.clone(),
            previous_snapshot: None,
            previous_archive: None,
            account_root: Some(account_root),
            defer_media: false,
        },
        None,
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("requires both the previous snapshot"));
    assert!(!rejected_output.exists());
    let unchanged_handoff: serde_json::Value =
        serde_json::from_slice(&fs::read(&handoff).unwrap()).unwrap();
    assert_eq!(unchanged_handoff["generation"], 3);

    let stale_branch_output = private.join("stale-branch");
    let stale_error = restore_snapshot_and_publish(
        &incremental_snapshot,
        &OfflineRestorePublishOptions {
            output_archive: stale_branch_output.clone(),
            handoff_path: handoff.clone(),
            previous_snapshot: Some(bootstrap_snapshot),
            previous_archive: Some(bootstrap_archive),
            account_root: Some(private.join("account-root")),
            defer_media: false,
        },
        None,
    )
    .unwrap_err();
    assert!(stale_error
        .to_string()
        .contains("no longer the current handoff archive"));
    assert!(!stale_branch_output.exists());
    let still_current: serde_json::Value =
        serde_json::from_slice(&fs::read(&handoff).unwrap()).unwrap();
    assert_eq!(still_current["generation"], 3);
}

fn build_bootstrap_snapshot(parent: &Path, account_root: &Path) -> PathBuf {
    let snapshot = private_directory(parent, "snapshot-bootstrap");
    let set_a = create_database(&snapshot, account_root, 0, "set-a", "old-a", 1);
    let set_b = create_database(&snapshot, account_root, 1, "set-b", "keep-b", 2);
    let source_sets = vec![set_a.inventory.clone(), set_b.inventory.clone()];
    let manifest = SnapshotManifest {
        manifest_format_version: 3,
        snapshot_id: "00000000-0000-4000-8000-000000000101".to_string(),
        created_at: "2026-08-27T00:00:00Z".to_string(),
        source_fingerprint: source_inventory_fingerprint(&source_sets),
        account_binding: None,
        client_build: Some(pinned_client()),
        acquisition: Some(SnapshotAcquisitionEvidence {
            format_version: 2,
            mode: SnapshotAcquisitionMode::Bootstrap,
            previous_source_fingerprint: None,
            reconciliation_window_seconds: 86_400,
            changed_source_set_ids: vec!["set-a".to_string(), "set-b".to_string()],
            reconciliation_source_set_ids: Vec::new(),
            deleted_source_set_ids: Vec::new(),
            source_sets,
            last_integrity_scan_at: Some("2026-08-27T00:00:00Z".to_string()),
        }),
        entries: vec![set_a.entry, set_b.entry],
    };
    write_manifest(&snapshot, &manifest);
    snapshot
}

fn build_incremental_snapshot(
    parent: &Path,
    previous_snapshot: &Path,
    account_root: &Path,
) -> PathBuf {
    let previous: SnapshotManifest =
        serde_json::from_slice(&fs::read(previous_snapshot.join("manifest.json")).unwrap())
            .unwrap();
    let snapshot = private_directory(parent, "snapshot-incremental");
    let changed_a = create_database(&snapshot, account_root, 0, "set-a", "new-a", 3);
    let unchanged_b = previous
        .acquisition
        .as_ref()
        .unwrap()
        .source_sets
        .iter()
        .find(|set| set.source_set_id == "set-b")
        .unwrap()
        .clone();
    let source_sets = vec![changed_a.inventory.clone(), unchanged_b];
    let manifest = SnapshotManifest {
        manifest_format_version: 3,
        snapshot_id: "00000000-0000-4000-8000-000000000102".to_string(),
        created_at: "2026-08-27T00:01:00Z".to_string(),
        source_fingerprint: source_inventory_fingerprint(&source_sets),
        account_binding: None,
        client_build: Some(pinned_client()),
        acquisition: Some(SnapshotAcquisitionEvidence {
            format_version: 2,
            mode: SnapshotAcquisitionMode::Incremental,
            previous_source_fingerprint: Some(previous.source_fingerprint),
            reconciliation_window_seconds: 86_400,
            changed_source_set_ids: vec!["set-a".to_string()],
            reconciliation_source_set_ids: Vec::new(),
            deleted_source_set_ids: Vec::new(),
            source_sets,
            last_integrity_scan_at: Some("2026-08-27T00:00:00Z".to_string()),
        }),
        entries: vec![changed_a.entry],
    };
    write_manifest(&snapshot, &manifest);
    snapshot
}

fn build_integrity_snapshot(
    parent: &Path,
    bootstrap_snapshot: &Path,
    incremental_snapshot: &Path,
    account_root: &Path,
) -> PathBuf {
    let previous: SnapshotManifest =
        serde_json::from_slice(&fs::read(incremental_snapshot.join("manifest.json")).unwrap())
            .unwrap();
    let previous_bootstrap: SnapshotManifest =
        serde_json::from_slice(&fs::read(bootstrap_snapshot.join("manifest.json")).unwrap())
            .unwrap();
    let snapshot = private_directory(parent, "snapshot-integrity");
    let source_sets = previous.acquisition.as_ref().unwrap().source_sets.clone();
    let sources = [
        incremental_snapshot.join("sets/0000/database.db"),
        bootstrap_snapshot.join("sets/0001/database.db"),
    ];
    let mut entries = Vec::new();
    for (index, (inventory, source)) in source_sets.iter().zip(sources).enumerate() {
        let relative_path = format!("sets/{index:04}/database.db");
        let target = snapshot.join(&relative_path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(source, &target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let file = inventory.files.first().unwrap();
        let expected_source_path = fs::canonicalize(account_root)
            .unwrap()
            .join("db_storage")
            .join(&inventory.logical_path);
        let opaque_id = hex::encode(Sha256::digest(
            expected_source_path.to_string_lossy().as_bytes(),
        ));
        entries.push(SnapshotEntry {
            source: greenbubbles::manifest::PathReference {
                opaque_id: opaque_id[..24].to_string(),
                path: None,
            },
            source_set_id: inventory.source_set_id.clone(),
            logical_path: inventory.logical_path.clone(),
            relative_path,
            role: SnapshotFileRole::Database,
            fingerprint: file.fingerprint.clone(),
            sha256: file.content_sha256.clone().unwrap(),
        });
    }
    let manifest = SnapshotManifest {
        manifest_format_version: 3,
        snapshot_id: "00000000-0000-4000-8000-000000000103".to_string(),
        created_at: "2026-08-27T00:02:00Z".to_string(),
        source_fingerprint: previous.source_fingerprint.clone(),
        account_binding: None,
        client_build: Some(pinned_client()),
        acquisition: Some(SnapshotAcquisitionEvidence {
            format_version: 2,
            mode: SnapshotAcquisitionMode::IntegrityScan,
            previous_source_fingerprint: Some(previous.source_fingerprint),
            reconciliation_window_seconds: 86_400,
            changed_source_set_ids: vec!["set-a".to_string(), "set-b".to_string()],
            reconciliation_source_set_ids: Vec::new(),
            deleted_source_set_ids: Vec::new(),
            source_sets,
            last_integrity_scan_at: Some("2026-08-27T00:02:00Z".to_string()),
        }),
        entries,
    };
    assert_eq!(
        manifest.client_build, previous_bootstrap.client_build,
        "the full chain remains pinned to one signed build"
    );
    write_manifest(&snapshot, &manifest);
    snapshot
}

struct CreatedSet {
    entry: SnapshotEntry,
    inventory: SnapshotSourceSetInventory,
}

fn create_database(
    snapshot: &Path,
    account_root: &Path,
    index: usize,
    set_id: &str,
    text: &str,
    file_id: u64,
) -> CreatedSet {
    let relative_path = format!("sets/{index:04}/database.db");
    let path = snapshot.join(&relative_path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TABLE Msg_{index}(local_id INTEGER, server_id INTEGER, sort_seq INTEGER, local_type INTEGER, create_time INTEGER, status INTEGER, message_content BLOB, WCDB_CT_message_content INTEGER);\n             INSERT INTO Msg_{index} VALUES ({file_id}, {file_id}, {file_id}, 1, 1700000000, 2, x'{}', 0);",
            hex::encode(text)
        ))
        .unwrap();
    drop(connection);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let bytes = fs::read(&path).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));
    let fingerprint = greenbubbles::manifest::SourceFileFingerprint {
        device_id: 1,
        file_id,
        byte_count: bytes.len() as i64,
        modified_seconds: file_id as i64,
        modified_nanoseconds: 0,
    };
    let logical_path = format!("message/message_{index}.db");
    let expected_source_path = fs::canonicalize(account_root)
        .unwrap()
        .join("db_storage")
        .join(&logical_path);
    let opaque_id = hex::encode(Sha256::digest(
        expected_source_path.to_string_lossy().as_bytes(),
    ));
    CreatedSet {
        entry: SnapshotEntry {
            source: greenbubbles::manifest::PathReference {
                opaque_id: opaque_id[..24].to_string(),
                path: None,
            },
            source_set_id: set_id.to_string(),
            logical_path: logical_path.clone(),
            relative_path,
            role: SnapshotFileRole::Database,
            fingerprint: fingerprint.clone(),
            sha256: digest.clone(),
        },
        inventory: SnapshotSourceSetInventory {
            source_set_id: set_id.to_string(),
            logical_path,
            files: vec![SnapshotSourceFileInventory {
                role: SnapshotFileRole::Database,
                fingerprint,
                content_sha256: Some(digest),
            }],
        },
    }
}

fn private_directory(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn write_manifest(snapshot: &Path, manifest: &SnapshotManifest) {
    let path = snapshot.join("manifest.json");
    fs::write(path.as_path(), serde_json::to_vec_pretty(manifest).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn pinned_client() -> ClientBuildFingerprint {
    ClientBuildFingerprint {
        format_version: 1,
        bundle_identifier: "com.tencent.xinWeChat".to_string(),
        marketing_version: "4.1.13".to_string(),
        build_version: "269579".to_string(),
        executable_sha256: "041f2632f8c9f4208f0b1ad26d574384e0b854952097a851f7d9c7c6f64a8542"
            .to_string(),
        signing_identifier: "com.tencent.xinWeChat".to_string(),
        team_identifier: "5A4RE8SF68".to_string(),
        code_directory_sha256: "c6b9f9587044784456eb96314f685c965fbd7d88bdacb72387284b8df551df4f"
            .to_string(),
        architectures: vec!["arm64".to_string(), "x86_64".to_string()],
        hardened_runtime: true,
        signature_valid: true,
    }
}

fn source_inventory_fingerprint(source_sets: &[SnapshotSourceSetInventory]) -> String {
    let mut hasher = Sha256::new();
    for source_set in source_sets {
        hasher.update(source_set.source_set_id.as_bytes());
        hasher.update([0]);
        hasher.update(source_set.logical_path.as_bytes());
        for file in &source_set.files {
            let role = match file.role {
                SnapshotFileRole::Database => "database",
                SnapshotFileRole::WriteAheadLog => "writeAheadLog",
                SnapshotFileRole::SharedMemory => "sharedMemory",
            };
            let fields = [
                role.to_string(),
                file.fingerprint.device_id.to_string(),
                file.fingerprint.file_id.to_string(),
                file.fingerprint.byte_count.to_string(),
                file.fingerprint.modified_seconds.to_string(),
                file.fingerprint.modified_nanoseconds.to_string(),
                file.content_sha256.clone().unwrap(),
            ];
            for field in fields {
                hasher.update([0x1f]);
                hasher.update(field.as_bytes());
            }
        }
        hasher.update([0x1e]);
    }
    hex::encode(hasher.finalize())
}
