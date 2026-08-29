use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use greenbubbles::{
    audit::audit_archive, prepare_catalog, restore_catalog, AccountHolderBindingEvidence,
    RestorationOptions, SnapshotAccountBinding, SnapshotAccountBindingEvidence,
    SnapshotAcquisitionEvidence, SnapshotAcquisitionMode, SnapshotEntry, SnapshotFileRole,
    SnapshotManifest, SnapshotSourceFileInventory, SnapshotSourceSetInventory,
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
             CREATE TABLE FMessageTable(
               user_name_ TEXT,
               type_ INTEGER,
               timestamp_ INTEGER,
               encrypt_user_name_ TEXT,
               content_ BLOB,
               is_sender_ INTEGER,
               ticket_ BLOB,
               scene_ INTEGER,
               fmessage_detail_buf_ BLOB,
               remark_ TEXT,
               label_ids_ BLOB
             );
             INSERT INTO FMessageTable
             VALUES ('wxid_friend', 37, 1700000004, 'opaque-friend', x'6869', 0,
                     x'0102', 17, x'0304', 'synthetic', x'0506');
             INSERT INTO FMessageTable
             VALUES ('wxid_friend_2', 65, 1700000005, '', x'68656c6c6f', 1,
                     x'0708', 18, x'090a', NULL, NULL);
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
        account_binding: None,
        client_build: None,
        acquisition: None,
        entries: vec![SnapshotEntry {
            source: greenbubbles::manifest::PathReference {
                opaque_id: "source".to_string(),
                path: None,
            },
            source_set_id: "set1".to_string(),
            logical_path: "message/message_0.db".to_string(),
            relative_path: "sets/0000/database.db".to_string(),
            role: SnapshotFileRole::Database,
            fingerprint: greenbubbles::manifest::SourceFileFingerprint {
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
    assert_eq!(report.integrity.source_row_count, 6);
    assert_eq!(report.integrity.restored_row_count, 6);
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
    assert_eq!(lines.len(), 6);
    let first_ordinary = lines
        .iter()
        .find(|message| {
            message["sourceTableName"] == "Msg_29a6db07e8bbdb53f5d54cc3c309f3f1"
                && message["sourceRowId"] == 1
        })
        .unwrap();
    let second_ordinary = lines
        .iter()
        .find(|message| {
            message["sourceTableName"] == "Msg_29a6db07e8bbdb53f5d54cc3c309f3f1"
                && message["sourceRowId"] == 2
        })
        .unwrap();
    assert_eq!(first_ordinary["contentBase64"], json!("aGVsbG8="));
    assert_eq!(second_ordinary["contentBase64"], json!("AP8="));
    assert_eq!(
        first_ordinary["rawColumns"]["packed_info_data"],
        json!({"storageClass": "blobBase64", "value": "AQI="})
    );
    let friend_request = lines
        .iter()
        .find(|message| message["sourceTableName"] == "FMessageTable")
        .unwrap();
    assert_eq!(friend_request["rawType"], json!(37));
    assert_eq!(friend_request["createdAtUnix"], json!(1700000004_i64));
    assert_eq!(friend_request["contentBase64"], json!("aGk="));
    assert_eq!(
        friend_request["rawColumns"]["fmessage_detail_buf_"],
        json!({"storageClass": "blobBase64", "value": "AwQ="})
    );
    assert_eq!(friend_request["semanticDecodeState"], "complete");
    assert_eq!(
        friend_request["typedPayload"]["value"]["FriendContactEvent"]["eventCode"],
        json!(37)
    );
    assert!(lines.iter().any(|message| {
        message["sourceTableName"] == "FMessageTable"
            && message["rawType"] == 65
            && message["semanticDecodeState"] == "complete"
    }));
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
    assert_eq!(coverage["formatVersion"], json!(4));
    assert_eq!(
        coverage["schemaProfileFingerprint"].as_str().unwrap().len(),
        64
    );
    let all_tables = coverage["allTables"].as_array().unwrap();
    assert_eq!(all_tables.len(), 5);
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
    assert!(all_tables.iter().any(|table| {
        table["sourceTableName"] == "FMessageTable" && table["role"] == "message"
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
    assert_eq!(audit.message_count, 6);
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

#[test]
fn legacy_account_root_binds_self_and_sender_identity_controls_direction() {
    let fixture = tempfile::tempdir().unwrap();
    let account_root = fixture.path().join("wxid_self_ab12");
    fs::create_dir_all(account_root.join("db_storage")).unwrap();
    fs::create_dir_all(account_root.join("msg")).unwrap();
    let snapshot = fixture.path().join("snapshot-bound-direction");
    fs::create_dir_all(snapshot.join("sets/0000")).unwrap();
    let database = snapshot.join("sets/0000/database.db");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE Name2Id(user_name TEXT);
             INSERT INTO Name2Id(rowid, user_name) VALUES
               (1, 'wxid_self'),
               (2, 'wxid_other'),
               (3, '');
             CREATE TABLE Msg_direction(
               local_id INTEGER,
               server_id INTEGER,
               sort_seq INTEGER,
               local_type INTEGER,
               real_sender_id INTEGER,
               talker TEXT,
               sender TEXT,
               create_time INTEGER,
               status INTEGER,
               message_content BLOB,
               is_sender_ INTEGER,
               is_sender INTEGER,
               WCDB_CT_message_content INTEGER
             );
             INSERT INTO Msg_direction VALUES
               (1, 11, 21, 1, NULL, 'wxid_peer', 'wxid_self', 1700000101, 2, x'73656c66', 1, 0, 0),
               (2, 12, 22, 1, NULL, 'group@chatroom', 'wxid_other', 1700000102, 2, x'6f74686572', 0, 1, 0),
               (3, 13, 23, 1, NULL, 'wxid_peer', 'wxid_self', 1700000103, 2, x'636f6e666c696374', 0, 1, 0),
               (4, 14, 24, 1, 3, 'wxid_peer', '', 1700000104, 2, x'656d707479', NULL, NULL, 0);
             CREATE TABLE FMessageTable(
               user_name_ TEXT,
               type_ INTEGER,
               timestamp_ INTEGER,
               content_ BLOB,
               is_sender_ INTEGER
             );
             INSERT INTO FMessageTable VALUES
               ('wxid_friend', 37, 1700000104, x'66616c6c6261636b', 1);",
        )
        .unwrap();
    drop(connection);
    write_legacy_snapshot_manifest(&snapshot, &database, &account_root);

    let catalog = prepare_catalog(&snapshot, None).unwrap();
    let output = fixture.path().join("restored-bound-direction");
    let report = restore_catalog(
        &catalog,
        &RestorationOptions {
            output_directory: output.clone(),
            account_root: Some(account_root.clone()),
            defer_media: false,
        },
    )
    .unwrap();

    assert_eq!(report.format_version, 6);
    assert!(report.self_participant_id.is_some());
    assert_eq!(report.integrity.direction_conflict_count, 1);
    assert!(!report.completion.directions_complete);
    let messages = fs::read_to_string(output.join("messages.ndjson")).unwrap();
    let messages = messages
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let row = |table: &str, source_row_id: i64| {
        messages
            .iter()
            .find(|message| {
                message["sourceTableName"] == table && message["sourceRowId"] == source_row_id
            })
            .unwrap()
    };
    assert_eq!(row("Msg_direction", 1)["direction"], "outgoing");
    assert_eq!(
        row("Msg_direction", 1)["directionEvidence"],
        "senderMatchesAccount"
    );
    assert_eq!(row("Msg_direction", 2)["direction"], "incoming");
    assert_eq!(
        row("Msg_direction", 2)["directionEvidence"],
        "senderDiffersFromAccount"
    );
    assert_eq!(row("FMessageTable", 1)["direction"], "outgoing");
    assert_eq!(
        row("FMessageTable", 1)["directionEvidence"],
        "explicitSourceColumn"
    );
    assert_eq!(row("Msg_direction", 3)["direction"], "outgoing");
    assert_eq!(
        row("Msg_direction", 3)["directionEvidence"],
        "senderAccountConflictWithExplicitSourceColumn"
    );
    // `is_sender_` precedes the deliberately contradictory `is_sender` in the
    // source schema. Both restoration and independent audit must honor that
    // original column order rather than a hard-coded alias preference.
    assert_eq!(
        row("Msg_direction", 1)["rawColumns"]["is_sender_"],
        json!({"storageClass": "integer", "value": 1})
    );
    assert_eq!(
        row("Msg_direction", 1)["rawColumns"]["is_sender"],
        json!({"storageClass": "integer", "value": 0})
    );
    assert!(row("Msg_direction", 4)["senderId"].is_null());
    assert!(row("Msg_direction", 4)["senderSourceIdentifierBase64"].is_null());
    assert_eq!(row("Msg_direction", 4)["direction"], "unknown");
    assert_eq!(row("Msg_direction", 4)["directionEvidence"], "unresolved");

    let audit = audit_archive(&output).unwrap();
    assert!(audit.account_holder_bound);
    assert_eq!(audit.direction_conflict_count, 1);
    assert!(!audit.full_restoration_verified);

    drop(catalog);
    write_bound_snapshot_manifest(&snapshot, &database, &account_root);
    let catalog = prepare_catalog(&snapshot, None).unwrap();
    let manifest_bound_output = fixture.path().join("restored-manifest-bound-direction");
    let manifest_bound_report = restore_catalog(
        &catalog,
        &RestorationOptions {
            output_directory: manifest_bound_output,
            account_root: None,
            defer_media: false,
        },
    )
    .unwrap();
    assert_eq!(manifest_bound_report.format_version, 6);
    assert_eq!(
        manifest_bound_report.account_binding_evidence,
        Some(AccountHolderBindingEvidence::SnapshotManifest)
    );
    assert_eq!(
        manifest_bound_report.self_participant_id,
        report.self_participant_id
    );
    assert_eq!(manifest_bound_report.integrity.direction_conflict_count, 1);
}

#[test]
fn pat_xml_recovers_sender_but_senderless_system_rows_remain_unknown() {
    let fixture = tempfile::tempdir().unwrap();
    let account_root = fixture.path().join("wxid_self_ab12");
    fs::create_dir_all(account_root.join("db_storage")).unwrap();
    fs::create_dir_all(account_root.join("msg")).unwrap();
    let snapshot = fixture.path().join("snapshot-pat-direction");
    fs::create_dir_all(snapshot.join("sets/0000")).unwrap();
    let database = snapshot.join("sets/0000/database.db");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE Msg_pat(
               local_id INTEGER,
               server_id INTEGER,
               sort_seq INTEGER,
               local_type INTEGER,
               talker TEXT,
               sender TEXT,
               create_time INTEGER,
               status INTEGER,
               message_content BLOB,
               WCDB_CT_message_content INTEGER
             );",
        )
        .unwrap();
    let pat_type = (62_i64 << 32) | 49_i64;
    let self_pat = "<sysmsg type=\"pat\"><pat><fromusername></fromusername><fromusername>wxid_self</fromusername><pattedusername>wxid_other</pattedusername></pat></sysmsg>";
    let other_pat = "<sysmsg type=\"pat\"><pat><fromusername>wxid_other</fromusername><fromusername>wxid_other</fromusername><pattedusername>wxid_self</pattedusername></pat></sysmsg>";
    let ambiguous_pat = "<sysmsg type=\"pat\"><pat><fromusername>wxid_other</fromusername><fromusername>wxid_third</fromusername><pattedusername>wxid_self</pattedusername></pat></sysmsg>";
    for (local_id, local_type, talker, payload) in [
        (1_i64, pat_type, "group@chatroom", self_pat),
        (2, pat_type, "group@chatroom", other_pat),
        (3, 10_000, "wxid_peer", "senderless system notice"),
        (4, pat_type, "group@chatroom", ambiguous_pat),
    ] {
        connection
            .execute(
                "INSERT INTO Msg_pat VALUES (?1, ?2, ?3, ?4, ?5, '', ?6, 2, ?7, 0)",
                rusqlite::params![
                    local_id,
                    local_id + 10,
                    local_id + 20,
                    local_type,
                    talker,
                    1_700_001_000_i64 + local_id,
                    payload.as_bytes(),
                ],
            )
            .unwrap();
    }
    drop(connection);
    write_bound_snapshot_manifest(&snapshot, &database, &account_root);

    let catalog = prepare_catalog(&snapshot, None).unwrap();
    let output = fixture.path().join("restored-pat-direction");
    let report = restore_catalog(
        &catalog,
        &RestorationOptions {
            output_directory: output.clone(),
            account_root: None,
            defer_media: false,
        },
    )
    .unwrap();
    assert_eq!(report.format_version, 6);
    assert_eq!(report.integrity.direction_conflict_count, 0);
    assert_eq!(report.integrity.direction_counts["outgoing"], 1);
    assert_eq!(report.integrity.direction_counts["incoming"], 1);
    assert_eq!(report.integrity.direction_counts["unknown"], 2);

    let messages = fs::read_to_string(output.join("messages.ndjson")).unwrap();
    let messages = messages
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let row = |source_row_id: i64| {
        messages
            .iter()
            .find(|message| message["sourceRowId"] == source_row_id)
            .unwrap()
    };
    assert_eq!(row(1)["direction"], "outgoing");
    assert_eq!(
        row(1)["senderId"].as_str(),
        report.self_participant_id.as_deref()
    );
    assert_eq!(row(1)["directionEvidence"], "senderMatchesAccount");
    assert_eq!(row(2)["direction"], "incoming");
    assert_ne!(
        row(2)["senderId"].as_str(),
        report.self_participant_id.as_deref()
    );
    assert_eq!(row(2)["directionEvidence"], "senderDiffersFromAccount");
    for senderless in [row(3), row(4)] {
        assert!(senderless["senderId"].is_null());
        assert!(senderless["senderSourceIdentifierBase64"].is_null());
        assert_eq!(senderless["direction"], "unknown");
        assert_eq!(senderless["directionEvidence"], "unresolved");
    }
    assert!(audit_archive(&output).unwrap().report_matches_archive);

    // A nonempty row sender remains primary evidence. The independent audit
    // must still compare it with Pat's source XML and reject disagreement.
    drop(catalog);
    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "INSERT INTO Msg_pat VALUES (5, 15, 25, ?1, 'group@chatroom', 'wxid_other', 1700001005, 2, ?2, 0)",
            rusqlite::params![pat_type, self_pat.as_bytes()],
        )
        .unwrap();
    drop(connection);
    write_bound_snapshot_manifest(&snapshot, &database, &account_root);
    let catalog = prepare_catalog(&snapshot, None).unwrap();
    let mismatch_output = fixture.path().join("restored-pat-mismatch");
    restore_catalog(
        &catalog,
        &RestorationOptions {
            output_directory: mismatch_output.clone(),
            account_root: None,
            defer_media: false,
        },
    )
    .unwrap();
    assert!(audit_archive(&mismatch_output)
        .unwrap_err()
        .to_string()
        .contains("Pat message sender disagrees with its source XML"));
}

fn write_legacy_snapshot_manifest(snapshot: &Path, database: &Path, account_root: &Path) {
    let bytes = fs::read(database).unwrap();
    let metadata = fs::metadata(database).unwrap();
    let manifest = SnapshotManifest {
        manifest_format_version: 1,
        snapshot_id: "00000000-0000-4000-8000-000000000002".to_string(),
        created_at: "2026-08-27T00:00:00Z".to_string(),
        source_fingerprint: "bound-direction-fixture".to_string(),
        account_binding: None,
        client_build: None,
        acquisition: None,
        entries: vec![SnapshotEntry {
            source: greenbubbles::manifest::PathReference {
                opaque_id: hex::encode(Sha256::digest(
                    fs::canonicalize(account_root)
                        .unwrap()
                        .join("db_storage/message/message_0.db")
                        .to_string_lossy()
                        .as_bytes(),
                ))[..24]
                    .to_string(),
                path: None,
            },
            source_set_id: "set1".to_string(),
            logical_path: "message/message_0.db".to_string(),
            relative_path: "sets/0000/database.db".to_string(),
            role: SnapshotFileRole::Database,
            fingerprint: greenbubbles::manifest::SourceFileFingerprint {
                device_id: 1,
                file_id: 1,
                byte_count: metadata.len() as i64,
                modified_seconds: 0,
                modified_nanoseconds: 0,
            },
            sha256: hex::encode(Sha256::digest(bytes)),
        }],
    };
    fs::write(
        snapshot.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn write_bound_snapshot_manifest(snapshot: &Path, database: &Path, account_root: &Path) {
    let bytes = fs::read(database).unwrap();
    let metadata = fs::metadata(database).unwrap();
    let content_sha256 = hex::encode(Sha256::digest(&bytes));
    let fingerprint = greenbubbles::manifest::SourceFileFingerprint {
        device_id: 1,
        file_id: 1,
        byte_count: metadata.len() as i64,
        modified_seconds: 0,
        modified_nanoseconds: 0,
    };
    let binding = SnapshotAccountBinding {
        format_version: 1,
        account_id: hex::encode(Sha256::digest(
            fs::canonicalize(account_root)
                .unwrap()
                .to_string_lossy()
                .as_bytes(),
        )),
        self_source_identifier_base64: "d3hpZF9zZWxm".to_string(),
        evidence: SnapshotAccountBindingEvidence::SelectedAccountDirectory,
    };
    let source_sets = vec![SnapshotSourceSetInventory {
        source_set_id: "set1".to_string(),
        logical_path: "message/message_0.db".to_string(),
        files: vec![SnapshotSourceFileInventory {
            role: SnapshotFileRole::Database,
            fingerprint: fingerprint.clone(),
            content_sha256: Some(content_sha256.clone()),
        }],
    }];
    let manifest = SnapshotManifest {
        manifest_format_version: 4,
        snapshot_id: "00000000-0000-4000-8000-000000000003".to_string(),
        created_at: "2026-08-27T00:00:00Z".to_string(),
        source_fingerprint: bound_source_fingerprint(&source_sets, &binding),
        account_binding: Some(binding),
        client_build: None,
        acquisition: Some(SnapshotAcquisitionEvidence {
            format_version: 2,
            mode: SnapshotAcquisitionMode::Bootstrap,
            previous_source_fingerprint: None,
            reconciliation_window_seconds: 900,
            changed_source_set_ids: vec!["set1".to_string()],
            reconciliation_source_set_ids: Vec::new(),
            deleted_source_set_ids: Vec::new(),
            source_sets,
            last_integrity_scan_at: Some("2026-08-27T00:00:00Z".to_string()),
        }),
        entries: vec![SnapshotEntry {
            source: greenbubbles::manifest::PathReference {
                opaque_id: "bound-source".to_string(),
                path: None,
            },
            source_set_id: "set1".to_string(),
            logical_path: "message/message_0.db".to_string(),
            relative_path: "sets/0000/database.db".to_string(),
            role: SnapshotFileRole::Database,
            fingerprint,
            sha256: content_sha256,
        }],
    };
    fs::write(
        snapshot.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn bound_source_fingerprint(
    source_sets: &[SnapshotSourceSetInventory],
    binding: &SnapshotAccountBinding,
) -> String {
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
            for field in [
                role.to_string(),
                file.fingerprint.device_id.to_string(),
                file.fingerprint.file_id.to_string(),
                file.fingerprint.byte_count.to_string(),
                file.fingerprint.modified_seconds.to_string(),
                file.fingerprint.modified_nanoseconds.to_string(),
                file.content_sha256
                    .as_deref()
                    .unwrap_or("missing")
                    .to_string(),
            ] {
                hasher.update([0x1f]);
                hasher.update(field.as_bytes());
            }
        }
        hasher.update([0x1e]);
    }
    hasher.update([0x1d]);
    for field in [
        "accountBinding".to_string(),
        binding.format_version.to_string(),
        binding.account_id.clone(),
        binding.self_source_identifier_base64.clone(),
        "selectedAccountDirectory".to_string(),
    ] {
        hasher.update([0x1f]);
        hasher.update(field.as_bytes());
    }
    hex::encode(hasher.finalize())
}
