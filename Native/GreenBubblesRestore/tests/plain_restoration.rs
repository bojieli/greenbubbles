use std::fs;
use std::os::unix::fs::PermissionsExt;

use greenbubbles_restore::{
    prepare_catalog, restore_catalog, RestorationOptions, SnapshotEntry, SnapshotFileRole,
    SnapshotManifest,
};
use rusqlite::Connection;
use serde_json::json;
use sha2::{Digest, Sha256};

#[test]
fn restores_every_plain_source_row_and_preserves_raw_payloads() {
    let fixture = tempfile::tempdir().unwrap();
    let snapshot = fixture.path().join("snapshot-test");
    fs::create_dir_all(snapshot.join("sets/0000")).unwrap();
    let database = snapshot.join("sets/0000/database.db");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE Name2Id(user_name TEXT);
             INSERT INTO Name2Id(rowid, user_name) VALUES (1, 'wxid_alice');
             CREATE TABLE Msg_0693e4da7db9e29637c64b95cc5162ca(
               local_id INTEGER,
               server_id INTEGER,
               sort_seq INTEGER,
               local_type INTEGER,
               real_sender_id INTEGER,
               create_time INTEGER,
               status INTEGER,
               message_content BLOB,
               packed_info_data BLOB,
               WCDB_CT_message_content INTEGER
             );
             INSERT INTO Msg_0693e4da7db9e29637c64b95cc5162ca
             VALUES (10, 20, 30, 1, 1, 1700000000, 2, x'68656c6c6f', x'0102', 0);
             INSERT INTO Msg_0693e4da7db9e29637c64b95cc5162ca
             VALUES (11, 21, 31, 123456, 1, 1700000001, 2, x'00ff', NULL, 0);",
        )
        .unwrap();
    drop(connection);

    let bytes = fs::read(&database).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));
    let metadata = fs::metadata(&database).unwrap();
    let manifest = SnapshotManifest {
        manifest_format_version: 1,
        snapshot_id: "00000000-0000-4000-8000-000000000001".to_string(),
        created_at: "2026-08-27T00:00:00Z".to_string(),
        source_fingerprint: "fixture-fingerprint".to_string(),
        entries: vec![SnapshotEntry {
            source: greenbubbles_restore::manifest::PathReference {
                opaque_id: "source".to_string(),
                path: None,
            },
            source_set_id: "set1".to_string(),
            logical_path: "message/message_0.db".to_string(),
            relative_path: "sets/0000/database.db".to_string(),
            role: SnapshotFileRole::Database,
            fingerprint: greenbubbles_restore::manifest::SourceFileFingerprint {
                device_id: 1,
                file_id: 1,
                byte_count: metadata.len() as i64,
                modified_seconds: 0,
                modified_nanoseconds: 0,
            },
            sha256: digest,
        }],
    };
    fs::write(
        snapshot.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let catalog = prepare_catalog(&snapshot, None).unwrap();
    let output = fixture.path().join("restored");
    let report = restore_catalog(
        &catalog,
        &RestorationOptions {
            output_directory: output.clone(),
            account_root: None,
        },
    )
    .unwrap();

    assert!(report.integrity.row_equation_holds());
    assert_eq!(report.integrity.source_row_count, 2);
    assert_eq!(report.integrity.restored_row_count, 2);
    assert_eq!(report.integrity.rejected_row_count, 0);
    assert_eq!(report.integrity.unknown_payload_count, 1);
    assert_eq!(
        fs::metadata(output.join("messages.ndjson"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let lines: Vec<serde_json::Value> = fs::read_to_string(output.join("messages.ndjson"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["contentBase64"], json!("aGVsbG8="));
    assert_eq!(lines[1]["contentBase64"], json!("AP8="));
    assert_eq!(
        lines[0]["rawColumns"]["packed_info_data"],
        json!({"storageClass": "blobBase64", "value": "AQI="})
    );
}
