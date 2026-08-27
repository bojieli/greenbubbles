use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use greenbubbles_restore::{
    audit::audit_archive, prepare_catalog, restore_catalog, RestorationOptions, SnapshotEntry,
    SnapshotFileRole, SnapshotManifest,
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
             CREATE TABLE Msg_29a6db07e8bbdb53f5d54cc3c309f3f1(
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
             INSERT INTO Msg_29a6db07e8bbdb53f5d54cc3c309f3f1
             VALUES (10, 20, 30, 1, 1, 1700000000, 2, x'68656c6c6f', x'0102', 0);
             INSERT INTO Msg_29a6db07e8bbdb53f5d54cc3c309f3f1
             VALUES (11, 21, 31, 123456, 1, 1700000001, 2, x'00ff', NULL, 0);
             CREATE TABLE MessageShadow(local_id INTEGER, opaque_payload BLOB);
             CREATE TABLE Preference(key TEXT, value BLOB);",
        )
        .unwrap();
    let merged_type = (19_i64 << 32) | 49_i64;
    let merged_xml = r#"<msg><appmsg><type>19</type><title>Forwarded history</title><recorditem><![CDATA[<recordinfo><datalist><dataitem datatype="1" dataid="child-1"><sourcename>Alice</sourcename><sourcetime>2026-08-27</sourcetime><datadesc>Hello</datadesc></dataitem><dataitem datatype="49" dataid="child-2"><content>&lt;msg&gt;&lt;appmsg&gt;&lt;title&gt;Nested link&lt;/title&gt;&lt;/appmsg&gt;&lt;/msg&gt;</content></dataitem></datalist></recordinfo>]]></recorditem></appmsg></msg>"#;
    let channel_type = (51_i64 << 32) | 49_i64;
    let channel_xml = r#"<msg xmlns:f="urn:finder"><appmsg><type>51</type><title>Channel clip</title><f:finderFeed id="feed-1"><objectId>123</objectId><mediaList><media><mediaType>4</mediaType><url>https://example.invalid/video</url><thumbUrl>https://example.invalid/thumb</thumbUrl><width>1080</width><height>1920</height></media></mediaList></f:finderFeed></appmsg></msg>"#;
    connection
        .execute(
            "INSERT INTO Msg_29a6db07e8bbdb53f5d54cc3c309f3f1
             VALUES (12, 22, 32, ?1, 1, 1700000002, 2, ?2, NULL, 0)",
            rusqlite::params![merged_type, merged_xml.as_bytes()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO Msg_29a6db07e8bbdb53f5d54cc3c309f3f1
             VALUES (13, 23, 33, ?1, 1, 1700000003, 2, ?2, NULL, 0)",
            rusqlite::params![channel_type, channel_xml.as_bytes()],
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
        client_build: None,
        acquisition: None,
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
            defer_media: false,
        },
    )
    .unwrap();

    assert!(report.integrity.row_equation_holds());
    assert_eq!(report.integrity.source_row_count, 4);
    assert_eq!(report.integrity.restored_row_count, 4);
    assert_eq!(report.integrity.rejected_row_count, 0);
    assert_eq!(report.integrity.unknown_payload_count, 1);
    assert_eq!(report.integrity.semantic_gap_count, 1);
    assert_eq!(report.integrity.message_candidate_gap_count, 1);
    assert!(!report.completion.semantic_message_coverage_complete);
    assert!(!report.completion.full_restoration_achieved);
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
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0]["contentBase64"], json!("aGVsbG8="));
    assert_eq!(lines[1]["contentBase64"], json!("AP8="));
    assert_eq!(
        lines[0]["rawColumns"]["packed_info_data"],
        json!({"storageClass": "blobBase64", "value": "AQI="})
    );
    let merged = lines
        .iter()
        .find(|message| message["subType"] == 19)
        .unwrap();
    assert_eq!(merged["semanticDecodeState"], "complete");
    assert!(merged["semanticGapReason"].is_null());
    assert_eq!(
        merged["typedPayload"]["value"]["MergedMessages"]["normalized_xml"]
            ["embeddedDocumentCount"],
        2
    );
    assert!(
        merged["typedPayload"]["value"]["MergedMessages"]["normalized_xml"]["nodeCount"]
            .as_u64()
            .unwrap()
            > 10
    );
    let channel = lines
        .iter()
        .find(|message| message["subType"] == 51)
        .unwrap();
    assert_eq!(channel["semanticDecodeState"], "complete");
    let channel_projection =
        serde_json::to_string(&channel["typedPayload"]["value"]["ChannelVideo"]["normalized_xml"])
            .unwrap();
    assert!(channel_projection.contains("urn:finder"));
    assert!(channel_projection.contains("thumbUrl"));
    let coverage: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("coverage.json")).unwrap()).unwrap();
    assert_eq!(coverage["formatVersion"], json!(3));
    assert_eq!(
        coverage["schemaProfileFingerprint"].as_str().unwrap().len(),
        64
    );
    let all_tables = coverage["allTables"].as_array().unwrap();
    assert_eq!(all_tables.len(), 4);
    assert!(all_tables.iter().all(|table| {
        table["schemaFingerprint"]
            .as_str()
            .is_some_and(|fingerprint| fingerprint.len() == 64)
    }));
    assert!(all_tables.iter().any(|table| {
        table["sourceTableName"] == "Name2Id" && table["role"] == "knownAuxiliary"
    }));
    assert!(all_tables.iter().any(|table| {
        table["sourceTableName"] == "MessageShadow" && table["role"] == "unhandledMessageCandidate"
    }));
    assert!(all_tables
        .iter()
        .any(|table| { table["sourceTableName"] == "Preference" && table["role"] == "other" }));

    let audit = audit_archive(&output).unwrap();
    assert_eq!(audit.format_version, 2);
    assert!(audit.report_matches_archive);
    assert!(audit.all_artifact_references_resolve);
    assert!(audit.all_resolved_relationships_resolve);
    assert!(audit.all_recorded_artifact_files_match);
    assert_eq!(audit.message_count, 4);
    assert!(!audit.full_restoration_verified);
    assert!(audit.completion_evidence.row_accounting_complete);
    assert!(audit.completion_evidence.non_empty_message_corpus_observed);
    assert!(
        !audit
            .completion_evidence
            .observed_message_type_coverage_complete
    );
    assert!(!audit.completion_evidence.verified_local_media_observed);
    assert!(
        audit
            .completion_evidence
            .external_authorization_attestation_required
    );
    assert!(
        audit
            .completion_evidence
            .disposable_scenario_attestation_required
    );
    assert!(audit.completion_evidence.observed_corpus_scope_only);
    assert!(Path::new(&report.messages_path).is_absolute());
    assert!(Path::new(&report.report_path).is_absolute());

    let message_path = output.join("messages.ndjson");
    let mut nested_tampered = lines.clone();
    let merged_index = nested_tampered
        .iter()
        .position(|message| message["subType"] == 19)
        .unwrap();
    nested_tampered[merged_index]["typedPayload"]["value"]["MergedMessages"]["normalized_xml"]
        ["nodeCount"] = json!(0);
    let bytes = nested_tampered
        .iter()
        .map(|message| serde_json::to_string(message).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&message_path, bytes).unwrap();
    assert!(audit_archive(&output)
        .unwrap_err()
        .to_string()
        .contains("projection differs from its source XML"));

    let mut provenance_tampered = lines.clone();
    provenance_tampered[0]["sourceTableName"] = json!("substituted-source-table");
    let bytes = provenance_tampered
        .iter()
        .map(|message| serde_json::to_string(message).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&message_path, bytes).unwrap();
    assert!(audit_archive(&output)
        .unwrap_err()
        .to_string()
        .contains("row provenance disagrees"));

    let mut identity_tampered = lines.clone();
    identity_tampered[0]["conversationSourceIdentifierBase64"] = json!("b3RoZXI=");
    let bytes = identity_tampered
        .iter()
        .map(|message| serde_json::to_string(message).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&message_path, bytes).unwrap();
    assert!(audit_archive(&output)
        .unwrap_err()
        .to_string()
        .contains("account-scoped and source-deterministic"));

    let mut tampered = lines;
    tampered[0]["contentBase64"] = json!("not-base64!");
    let bytes = tampered
        .iter()
        .map(|message| serde_json::to_string(message).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(message_path, bytes).unwrap();
    assert!(audit_archive(&output)
        .unwrap_err()
        .to_string()
        .contains("malformed source-preserving base64"));
}
