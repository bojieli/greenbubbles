use std::fs;

use greenbubbles_restore::manifest::{
    PathReference, SnapshotEntry, SnapshotFileRole, SnapshotManifest, SourceFileFingerprint,
};
use greenbubbles_restore::{
    prepare_catalog, restore_catalog, DatabasePassphrase, RestorationOptions, StorageFamily,
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

    let manifest = SnapshotManifest {
        manifest_format_version: 1,
        snapshot_id: "00000000-0000-4000-8000-000000000002".to_string(),
        created_at: "2026-08-27T00:00:00Z".to_string(),
        source_fingerprint: "encrypted-fixture-fingerprint".to_string(),
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
        },
    )
    .unwrap();
    assert_eq!(report.integrity.source_row_count, 1);
    assert_eq!(report.integrity.restored_row_count, 1);
    assert_eq!(report.integrity.rejected_row_count, 0);
    assert!(report.completion.semantic_message_coverage_complete);
    assert!(report.completion.full_restoration_achieved);

    drop(connection);
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
