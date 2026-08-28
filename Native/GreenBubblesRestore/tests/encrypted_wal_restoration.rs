use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

use greenbubbles_restore::manifest::{
    PathReference, SnapshotEntry, SnapshotFileRole, SnapshotManifest, SourceFileFingerprint,
};
use greenbubbles_restore::{
    audit::audit_archive, prepare_catalog, prepare_catalog_with_progress, restore_catalog,
    restore_catalog_with_progress, ClientBuildCompatibilityState, DatabaseKeySet,
    DatabasePassphrase, DatabaseUnlockMaterial, ProgressEvent, ProgressObserver, ProgressPhase,
    ProgressState, RestorationArchiveScope, RestorationOptions, StorageFamily,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

#[test]
fn decrypts_sqlcipher4_and_applies_committed_wal_frames() {
    let fixture = tempfile::tempdir().unwrap();
    let live = fixture.path().join("live.db");
    let connection = Connection::open(&live).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA';
             PRAGMA cipher_compatibility = 4;
             PRAGMA journal_mode = WAL;
             PRAGMA wal_autocheckpoint = 0;
             CREATE TABLE Name2Id(user_name TEXT);
             INSERT INTO Name2Id(rowid, user_name) VALUES (1, 'wxid_alice');
             CREATE TABLE Msg_29a6db07e8bbdb53f5d54cc3c309f3f1(
               local_id INTEGER, server_id INTEGER, sort_seq INTEGER,
               local_type INTEGER, real_sender_id INTEGER, create_time INTEGER,
               status INTEGER, message_content BLOB
             );
             PRAGMA wal_checkpoint(TRUNCATE);
             INSERT INTO Msg_29a6db07e8bbdb53f5d54cc3c309f3f1
             VALUES (10, 20, 30, 1, 1, 1700000000, 2, x'68656c6c6f');",
        )
        .unwrap();

    let live_wal = live.with_file_name("live.db-wal");
    assert!(live_wal.is_file());
    assert!(fs::metadata(&live_wal).unwrap().len() > 32);

    let snapshot = fixture.path().join("snapshot-test");
    let set_dir = snapshot.join("sets/0000");
    fs::create_dir_all(&set_dir).unwrap();
    let database = set_dir.join("database.db");
    let wal = set_dir.join("database.db-wal");
    fs::copy(&live, &database).unwrap();
    fs::copy(&live_wal, &wal).unwrap();

    let mut manifest = SnapshotManifest {
        manifest_format_version: 2,
        snapshot_id: "00000000-0000-4000-8000-000000000002".to_string(),
        created_at: "2026-08-27T00:00:00Z".to_string(),
        source_fingerprint: "encrypted-fixture-fingerprint".to_string(),
        account_binding: None,
        client_build: None,
        acquisition: None,
        entries: vec![
            entry(
                &database,
                "sets/0000/database.db",
                SnapshotFileRole::Database,
            ),
            entry(
                &wal,
                "sets/0000/database.db-wal",
                SnapshotFileRole::WriteAheadLog,
            ),
        ],
    };
    fs::write(
        snapshot.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let passphrase = DatabasePassphrase::from_bytes(*b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
    let catalog = prepare_catalog(&snapshot, Some(&passphrase)).unwrap();
    assert_eq!(catalog.databases.len(), 1);
    assert_eq!(
        catalog.databases[0].storage_family,
        StorageFamily::WcdbSqlcipher4
    );

    let output = fixture.path().join("restored");
    let report = restore_catalog(
        &catalog,
        &RestorationOptions {
            output_directory: output,
            account_root: None,
            defer_media: false,
        },
    )
    .unwrap();
    assert_eq!(report.integrity.source_row_count, 1);
    assert_eq!(report.integrity.restored_row_count, 1);
    assert_eq!(report.integrity.rejected_row_count, 0);
    assert!(report.completion.semantic_message_coverage_complete);
    assert!(!report.completion.full_restoration_achieved);
    assert_eq!(
        report.client_build_compatibility.state,
        ClientBuildCompatibilityState::Missing
    );

    let salt = wx_decrypt::read_db_salt(&live).unwrap();
    let encryption_key = wx_decrypt::kdf::derive_enc_key(
        passphrase.expose_for_database_operation(),
        &salt,
        &wx_decrypt::MACOS_4_1_7_31,
    );
    let unavailable_dir = snapshot.join("sets/0001");
    fs::create_dir_all(&unavailable_dir).unwrap();
    let unavailable_database = unavailable_dir.join("database.db");
    let mut unavailable_bytes = fs::read(&live).unwrap();
    unavailable_bytes[0] ^= 0x01;
    fs::write(&unavailable_database, unavailable_bytes).unwrap();
    let mut unavailable_entry = entry(
        &unavailable_database,
        "sets/0001/database.db",
        SnapshotFileRole::Database,
    );
    unavailable_entry.source_set_id = "set2".to_string();
    unavailable_entry.logical_path = "third_app_icon/third_app_icon.db".to_string();
    manifest.entries.push(unavailable_entry);
    fs::write(
        snapshot.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let key_file = fixture.path().join("exported-keys.json");
    fs::write(
        &key_file,
        serde_json::to_vec(&serde_json::json!({
            "stale-layout\\renamed-message.db": {
                "enc_key": hex::encode(encryption_key),
                "salt": hex::encode(salt),
                "size_mb": 1.0
            },
            "_db_dir": "/untrusted/metadata/is/ignored"
        }))
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600)).unwrap();
    let exported_keys = DatabaseKeySet::load(&key_file).unwrap();
    let progress = CapturingProgress::default();
    let direct_catalog = prepare_catalog_with_progress(
        &snapshot,
        DatabaseUnlockMaterial::ExportedKeys(&exported_keys),
        &progress,
    )
    .unwrap();
    let selection = direct_catalog
        .available_database_selection
        .as_ref()
        .unwrap();
    assert_eq!(selection.selected_database_count, 1);
    assert_eq!(selection.unavailable_database_count, 1);
    assert_eq!(
        selection.unavailable_databases[0].logical_path,
        "third_app_icon/third_app_icon.db"
    );
    let events = progress.events.lock().unwrap();
    assert!(events.iter().any(|event| {
        event.phase == ProgressPhase::KeyValidation && event.state == ProgressState::Completed
    }));
    assert!(events.iter().any(|event| {
        event.phase == ProgressPhase::KeyValidation
            && event.database_key_match_method.as_deref() == Some("uniqueSaltRelocation")
            && event.storage_family.as_deref() == Some("wcdbSqlcipher4")
            && event.database_byte_count.is_some_and(|bytes| bytes > 0)
            && event
                .write_ahead_log_byte_count
                .is_some_and(|bytes| bytes > 0)
    }));
    assert!(events.iter().any(|event| {
        event.phase == ProgressPhase::DatabasePreparation
            && event.state == ProgressState::Completed
            && event.table_count.is_some()
    }));
    assert!(events.iter().any(|event| {
        event.phase == ProgressPhase::DatabasePreparation
            && event.operation == "decryptDatabase"
            && event.state == ProgressState::Completed
    }));
    assert!(events.iter().any(|event| {
        event.phase == ProgressPhase::DatabasePreparation
            && event.operation == "scanWriteAheadLog"
            && event.state == ProgressState::Completed
    }));
    assert!(events.iter().any(|event| {
        event.phase == ProgressPhase::DatabasePreparation
            && event.operation == "applyWriteAheadLog"
            && event.state == ProgressState::Completed
            && event
                .write_ahead_log_frame_count
                .is_some_and(|count| count > 0)
    }));
    drop(events);
    let direct_output = fixture.path().join("restored-from-exported-keys");
    let direct_report = restore_catalog_with_progress(
        &direct_catalog,
        &RestorationOptions {
            output_directory: direct_output.clone(),
            account_root: None,
            defer_media: false,
        },
        &progress,
    )
    .unwrap();
    assert_eq!(direct_report.integrity.source_row_count, 1);
    assert_eq!(direct_report.integrity.restored_row_count, 1);
    assert_eq!(direct_report.integrity.rejected_row_count, 0);
    assert_eq!(
        direct_report.archive_scope,
        RestorationArchiveScope::PartialDatabaseCoverage
    );
    let database_coverage = direct_report.database_coverage.as_ref().unwrap();
    assert!(database_coverage.is_valid());
    assert_eq!(database_coverage.total_database_count, 2);
    assert_eq!(database_coverage.restored_database_count, 1);
    assert_eq!(database_coverage.unavailable_database_count, 1);
    assert!(direct_report.replica_mutation_eligible());
    let storage = direct_report.storage.as_ref().unwrap();
    assert!(storage.source_byte_count > 0);
    assert!(storage.estimated_archive_byte_count > storage.source_byte_count);
    assert!(storage.estimated_staging_byte_count > 0);
    assert_eq!(
        storage.estimated_peak_byte_count,
        storage
            .estimated_archive_byte_count
            .saturating_add(storage.estimated_staging_byte_count)
    );
    assert!(storage.available_free_byte_count_at_start >= storage.required_free_byte_count);
    assert!(storage.peak_staging_file_byte_count > 0);
    assert!(storage.staged_uncompressed_byte_count > 0);
    assert!(storage.staged_compressed_byte_count > 0);
    assert!(storage.actual_archive_byte_count > 0);
    assert_eq!(
        storage.actual_archive_byte_count,
        directory_file_byte_count(&direct_output)
    );
    assert!(fs::read_dir(&direct_output).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".staging-")
    }));
    assert!(
        audit_archive(&direct_output)
            .unwrap()
            .report_matches_archive
    );
    let events = progress.events.lock().unwrap();
    assert!(events.iter().any(|event| {
        event.phase == ProgressPhase::RecordPlanning
            && event.state == ProgressState::Completed
            && event.operation == "countDatabaseRecords"
            && event.completed == 2
            && event.total == 2
            && event.table_count == Some(2)
            && event.source_record_count == Some(1)
            && event.restored_record_count.is_none()
    }));
    assert!(events.iter().any(|event| {
        event.phase == ProgressPhase::RecordPlanning
            && event.state == ProgressState::Completed
            && event.operation == "preflightRestorationStorage"
            && event.source_byte_count.is_some_and(|bytes| bytes > 0)
            && event
                .estimated_archive_byte_count
                .is_some_and(|bytes| bytes > 0)
            && event
                .estimated_staging_byte_count
                .is_some_and(|bytes| bytes > 0)
            && event
                .estimated_peak_byte_count
                .is_some_and(|bytes| bytes > 0)
            && event
                .available_free_byte_count
                .zip(event.required_free_byte_count)
                .is_some_and(|(available, required)| available >= required)
    }));
    assert!(events.iter().any(|event| {
        event.phase == ProgressPhase::RecordRestoration
            && event.state == ProgressState::Completed
            && event.restored_record_count == Some(1)
            && event.staging_file_byte_count.is_some_and(|bytes| bytes > 0)
            && event
                .staged_uncompressed_byte_count
                .is_some_and(|bytes| bytes > 0)
            && event
                .staged_compressed_byte_count
                .is_some_and(|bytes| bytes > 0)
    }));
    assert!(events.iter().any(|event| {
        event.phase == ProgressPhase::ArchiveFinalization
            && event.state == ProgressState::Completed
            && event.restored_record_count == Some(1)
            && event
                .published_archive_byte_count
                .is_some_and(|bytes| bytes > 0)
    }));
    drop(events);

    let report_path = direct_output.join("report.json");
    let mut tampered: serde_json::Value =
        serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    let recorded = tampered["storage"]["actualArchiveByteCount"]
        .as_u64()
        .unwrap();
    tampered["storage"]["actualArchiveByteCount"] = serde_json::json!(recorded.saturating_add(1));
    fs::write(&report_path, serde_json::to_vec_pretty(&tampered).unwrap()).unwrap();
    assert!(audit_archive(&direct_output)
        .unwrap_err()
        .to_string()
        .contains("archive byte count"));

    drop(connection);
}

#[derive(Default)]
struct CapturingProgress {
    events: Mutex<Vec<ProgressEvent>>,
}

impl ProgressObserver for CapturingProgress {
    fn observe(&self, event: ProgressEvent) {
        self.events.lock().unwrap().push(event);
    }
}

fn entry(path: &std::path::Path, relative: &str, role: SnapshotFileRole) -> SnapshotEntry {
    let bytes = fs::read(path).unwrap();
    SnapshotEntry {
        source: PathReference {
            opaque_id: format!("source-{relative}"),
            path: None,
        },
        source_set_id: "set1".to_string(),
        logical_path: match role {
            SnapshotFileRole::Database => "message/message_0.db",
            SnapshotFileRole::WriteAheadLog => "message/message_0.db-wal",
            SnapshotFileRole::SharedMemory => "message/message_0.db-shm",
        }
        .to_string(),
        relative_path: relative.to_string(),
        role,
        fingerprint: SourceFileFingerprint {
            device_id: 1,
            file_id: 1,
            byte_count: bytes.len() as i64,
            modified_seconds: 0,
            modified_nanoseconds: 0,
        },
        sha256: hex::encode(Sha256::digest(&bytes)),
    }
}

fn directory_file_byte_count(path: &std::path::Path) -> u64 {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                directory_file_byte_count(&entry.path())
            } else {
                metadata.len()
            }
        })
        .sum()
}
