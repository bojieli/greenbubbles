use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use base64::Engine;
use greenbubbles_restore::archive::{create_conversation_policy, read_conversation_page};
use greenbubbles_restore::audit::audit_archive;
use greenbubbles_restore::manifest::{
    PathReference, SnapshotEntry, SnapshotFileRole, SnapshotManifest, SourceFileFingerprint,
};
use greenbubbles_restore::reconcile::reconcile_archives;
use greenbubbles_restore::{
    prepare_catalog, restore_catalog, RestorationMediaPhase, RestorationOptions,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

const CHAT: &str = "wxid_alice";
const GROUP: &str = "12345@chatroom";
const IMAGE_MD5: &str = "11111111111111111111111111111111";
const VIDEO_MD5: &str = "22222222222222222222222222222222";
const FILE_MD5: &str = "33333333333333333333333333333333";
const MISSING_MD5: &str = "44444444444444444444444444444444";
const UNSAFE_MD5: &str = "55555555555555555555555555555555";

#[test]
fn restores_ordered_multimodal_history_with_verified_local_paths() {
    let fixture = tempfile::tempdir().unwrap();
    let account = fixture.path().join("wxid_fixture_ab12");
    let attach = account
        .join("msg/attach")
        .join(format!("{:x}", md5::compute(CHAT.as_bytes())))
        .join("2026-08/Img");
    let video = account.join("msg/video/2026-08");
    let files = account.join("msg/file/2026-08");
    fs::create_dir_all(account.join("db_storage")).unwrap();
    fs::create_dir_all(&attach).unwrap();
    fs::create_dir_all(&video).unwrap();
    fs::create_dir_all(&files).unwrap();

    let jpeg = [
        0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00, 0xff, 0xd9,
    ];
    let xor_key = 0xa5;
    let encrypted_image = jpeg.iter().map(|value| value ^ xor_key).collect::<Vec<_>>();
    let image_path = attach.join(format!("{IMAGE_MD5}_h.dat"));
    fs::write(&image_path, encrypted_image).unwrap();
    let outside = fixture.path().join("outside.dat");
    fs::write(&outside, b"must-not-be-read").unwrap();
    std::os::unix::fs::symlink(&outside, attach.join(format!("{UNSAFE_MD5}.dat"))).unwrap();
    let video_path = video.join(format!("{VIDEO_MD5}.mp4"));
    fs::write(&video_path, b"\0\0\0\x18ftypmp42fixture-video").unwrap();
    let poster_path = video.join(format!("{VIDEO_MD5}.jpg"));
    fs::write(&poster_path, jpeg).unwrap();
    let file_path = files.join("report.pdf");
    fs::write(&file_path, b"%PDF-1.7\nfixture\n").unwrap();

    let snapshot = fixture.path().join("snapshot-test");
    let message_db = snapshot.join("sets/0000/database.db");
    let resource_db = snapshot.join("sets/0001/database.db");
    let media_db = snapshot.join("sets/0002/database.db");
    let contact_db = snapshot.join("sets/0003/database.db");
    let session_db = snapshot.join("sets/0004/database.db");
    fs::create_dir_all(message_db.parent().unwrap()).unwrap();
    fs::create_dir_all(resource_db.parent().unwrap()).unwrap();
    fs::create_dir_all(media_db.parent().unwrap()).unwrap();
    fs::create_dir_all(contact_db.parent().unwrap()).unwrap();
    fs::create_dir_all(session_db.parent().unwrap()).unwrap();

    let image_packed = wx_db::encode_packed_info_for_test(Some(IMAGE_MD5), None);
    let video_packed = wx_db::encode_packed_info_for_test(None, Some(VIDEO_MD5));
    let missing_packed = wx_db::encode_packed_info_for_test(Some(MISSING_MD5), None);
    let table = format!("Msg_{:x}", md5::compute(CHAT.as_bytes()));
    let connection = Connection::open(&message_db).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TABLE Name2Id(user_name TEXT);
             INSERT INTO Name2Id(rowid, user_name) VALUES (1, '{CHAT}');
             INSERT INTO Name2Id(rowid, user_name) VALUES (2, 'wxid_bob');
             INSERT INTO Name2Id(rowid, user_name) VALUES (3, '{GROUP}');
             CREATE TABLE {table}(
               local_id INTEGER, server_id INTEGER, sort_seq INTEGER,
               local_type INTEGER, real_sender_id INTEGER, create_time INTEGER,
               status INTEGER, message_content BLOB, packed_info_data BLOB
             );"
        ))
        .unwrap();
    let insert = format!(
        "INSERT INTO {table}(local_id, server_id, sort_seq, local_type,
          real_sender_id, create_time, status, message_content, packed_info_data)
          VALUES (?1, ?2, ?3, ?4, 1, ?5, 2, ?6, ?7)"
    );
    connection
        .execute(
            &insert,
            rusqlite::params![
                10_i64,
                100_i64,
                300_i64,
                3_i64,
                1_700_000_003_i64,
                b"",
                Option::<Vec<u8>>::None
            ],
        )
        .unwrap();
    connection
        .execute(
            &insert,
            rusqlite::params![
                11_i64,
                101_i64,
                100_i64,
                34_i64,
                1_700_000_001_i64,
                b"",
                Option::<Vec<u8>>::None
            ],
        )
        .unwrap();
    connection
        .execute(
            &insert,
            rusqlite::params![
                12_i64,
                102_i64,
                200_i64,
                43_i64,
                1_700_000_002_i64,
                b"",
                video_packed
            ],
        )
        .unwrap();
    let file_type = (6_i64 << 32) | 49_i64;
    let file_xml = format!(
        "<msg><appmsg><title>report.pdf</title><fileext>pdf</fileext><totallen>17</totallen><md5>{FILE_MD5}</md5></appmsg></msg>"
    );
    connection
        .execute(
            &insert,
            rusqlite::params![
                13_i64,
                103_i64,
                400_i64,
                file_type,
                1_700_000_004_i64,
                file_xml.as_bytes(),
                Option::<Vec<u8>>::None
            ],
        )
        .unwrap();
    connection
        .execute(
            &insert,
            rusqlite::params![
                14_i64,
                104_i64,
                500_i64,
                3_i64,
                1_700_000_005_i64,
                b"",
                missing_packed
            ],
        )
        .unwrap();
    let quote_type = (57_i64 << 32) | 49_i64;
    let quote_xml =
        "<msg><appmsg><title>reply</title><refermsg><svrid>101</svrid><type>34</type><content>voice</content></refermsg></appmsg></msg>";
    connection
        .execute(
            &insert,
            rusqlite::params![
                15_i64,
                105_i64,
                600_i64,
                quote_type,
                1_700_000_006_i64,
                quote_xml.as_bytes(),
                Option::<Vec<u8>>::None
            ],
        )
        .unwrap();
    let unsafe_packed = wx_db::encode_packed_info_for_test(Some(UNSAFE_MD5), None);
    connection
        .execute(
            &insert,
            rusqlite::params![
                16_i64,
                106_i64,
                700_i64,
                3_i64,
                1_700_000_007_i64,
                b"",
                unsafe_packed
            ],
        )
        .unwrap();
    let group_table = format!("Msg_{:x}", md5::compute(GROUP.as_bytes()));
    connection
        .execute_batch(&format!(
            "CREATE TABLE {group_table}(
               local_id INTEGER, server_id INTEGER, sort_seq INTEGER,
               local_type INTEGER, real_sender_id INTEGER, create_time INTEGER,
               status INTEGER, message_content BLOB, packed_info_data BLOB
             );"
        ))
        .unwrap();
    connection
        .execute(
            &format!("INSERT INTO {group_table} VALUES (?1, ?2, ?3, ?4, ?5, ?6, 2, ?7, NULL)"),
            rusqlite::params![
                20_i64,
                200_i64,
                50_i64,
                1_i64,
                2_i64,
                1_700_000_000_i64,
                b"wxid_bob:\nhello group"
            ],
        )
        .unwrap();
    connection
        .execute_batch(
            "CREATE TABLE BizMessage(
               msg_local_id INTEGER, msg_svr_id INTEGER, msg_type INTEGER,
               msg_create_time INTEGER, msg_content BLOB,
               username TEXT, from_user TEXT
             );
             INSERT INTO BizMessage VALUES (
               30, 300, 1, 1700000008, x'7075626c696320757064617465',
               'gh_public', 'gh_public'
             );",
        )
        .unwrap();
    drop(connection);

    let connection = Connection::open(&resource_db).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE MessageResourceInfo(
               message_local_id INTEGER, message_svr_id INTEGER, packed_info BLOB
             );",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO MessageResourceInfo VALUES (?1, ?2, ?3)",
            rusqlite::params![10_i64, 100_i64, image_packed],
        )
        .unwrap();
    drop(connection);

    let connection = Connection::open(&media_db).unwrap();
    connection
        .execute_batch("CREATE TABLE VoiceInfo(local_id INTEGER, svr_id INTEGER, voice_data BLOB);")
        .unwrap();
    connection
        .execute(
            "INSERT INTO VoiceInfo VALUES (?1, ?2, ?3)",
            rusqlite::params![11_i64, 101_i64, b"\x02#!SILK_V3fixture-voice"],
        )
        .unwrap();
    drop(connection);

    let connection = Connection::open(&contact_db).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE contact(username TEXT, alias TEXT, remark TEXT, nick_name TEXT);
             INSERT INTO contact VALUES ('wxid_alice', 'alice-id', 'Alice Remark', 'Alice');
             INSERT INTO contact VALUES ('wxid_bob', 'bob-id', '', 'Bob');
             INSERT INTO contact VALUES ('wxid_carol', 'carol-id', '', 'Carol');
             CREATE TABLE chat_room(username TEXT, owner TEXT, ext_buffer BLOB);",
        )
        .unwrap();
    let room_data = wx_db::encode_room_data_for_test(&[
        ("wxid_bob", Some("Bob in group")),
        ("wxid_carol", None),
    ]);
    connection
        .execute(
            "INSERT INTO chat_room VALUES (?1, ?2, ?3)",
            rusqlite::params![GROUP, "wxid_bob", room_data],
        )
        .unwrap();
    drop(connection);

    let connection = Connection::open(&session_db).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE SessionTable(username TEXT, sort_timestamp INTEGER, summary BLOB);
             INSERT INTO SessionTable VALUES ('wxid_alice', 1700000007, x'6869');
             INSERT INTO SessionTable VALUES ('12345@chatroom', 1700000000, x'67726f7570');",
        )
        .unwrap();
    drop(connection);

    let manifest = SnapshotManifest {
        manifest_format_version: 1,
        snapshot_id: "00000000-0000-4000-8000-000000000003".to_string(),
        created_at: "2026-08-27T00:00:00Z".to_string(),
        source_fingerprint: "multimodal-fixture-fingerprint".to_string(),
        account_binding: None,
        client_build: None,
        acquisition: None,
        entries: vec![
            entry(
                &message_db,
                &account,
                "set-message",
                "message/message_0.db",
                0,
            ),
            entry(
                &resource_db,
                &account,
                "set-resource",
                "message/message_resource.db",
                1,
            ),
            entry(&media_db, &account, "set-media", "media/media_0.db", 2),
            entry(
                &contact_db,
                &account,
                "set-contact",
                "contact/contact.db",
                3,
            ),
            entry(
                &session_db,
                &account,
                "set-session",
                "session/session.db",
                4,
            ),
        ],
    };
    fs::write(
        snapshot.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let catalog = prepare_catalog(&snapshot, None).unwrap();
    let text_first_output = fixture.path().join("text-first");
    let text_first_report = restore_catalog(
        &catalog,
        &RestorationOptions {
            output_directory: text_first_output.clone(),
            account_root: Some(account.clone()),
            defer_media: true,
        },
    )
    .unwrap();
    assert_eq!(
        text_first_report.media_phase,
        RestorationMediaPhase::Deferred
    );
    assert!(!text_first_report.completion.full_restoration_achieved);
    assert_eq!(text_first_report.integrity.restored_row_count, 9);
    assert!(ndjson(text_first_output.join("artifacts.ndjson"))
        .iter()
        .all(|artifact| artifact["verificationDetail"]
            .as_str()
            .is_some_and(|detail| detail.contains("explicitly deferred"))));
    assert_eq!(
        fs::read_dir(text_first_output.join("derived"))
            .unwrap()
            .count(),
        0
    );

    let output = fixture.path().join("restored");
    let report = restore_catalog(
        &catalog,
        &RestorationOptions {
            output_directory: output.clone(),
            account_root: Some(account.clone()),
            defer_media: false,
        },
    )
    .unwrap();

    assert_eq!(report.media_phase, RestorationMediaPhase::Resolved);
    assert_eq!(
        text_first_report.source_fingerprint,
        report.source_fingerprint
    );

    assert!(report.integrity.row_equation_holds());
    assert_eq!(report.integrity.source_row_count, 9);
    assert_eq!(report.integrity.restored_row_count, 9);
    assert_eq!(report.integrity.rejected_row_count, 0);
    assert_eq!(report.integrity.artifact_reference_count, 7);
    assert_eq!(report.integrity.unique_artifact_count, 7);
    assert_eq!(report.integrity.missing_artifact_count, 1);
    assert_eq!(report.integrity.decoded_artifact_count, 1);
    assert_eq!(report.integrity.artifact_decode_gap_count, 1);
    assert_eq!(report.integrity.unsafe_artifact_count, 1);
    assert_eq!(report.integrity.relationship_reference_count, 1);
    assert_eq!(report.integrity.resolved_relationship_count, 1);
    assert_eq!(report.integrity.conversation_count, 3);
    assert_eq!(report.integrity.participant_count, 4);
    assert_eq!(report.integrity.group_member_count, 2);
    assert_eq!(report.integrity.entity_source_row_count, 6);
    assert_eq!(report.integrity.entity_decode_gap_count, 0);
    assert!(!report.completion.artifact_verification_complete);
    assert!(!report.completion.artifact_decoding_complete);
    assert!(!report.completion.full_restoration_achieved);

    let messages = ndjson(output.join("messages.ndjson"));
    let direct_identifier = base64::engine::general_purpose::STANDARD.encode(CHAT.as_bytes());
    let direct_messages = messages
        .iter()
        .filter(|message| message["conversationSourceIdentifierBase64"] == direct_identifier)
        .collect::<Vec<_>>();
    let order = direct_messages
        .iter()
        .map(|message| message["sortSequence"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(order, vec![100, 200, 300, 400, 500, 600, 700]);
    assert!(direct_messages.iter().take(5).all(|message| {
        message["artifactReferences"]
            .as_array()
            .is_some_and(|value| !value.is_empty())
    }));
    assert!(direct_messages
        .iter()
        .all(|message| message["orderingBasis"] == "sortSequence"));
    assert_eq!(
        direct_messages
            .iter()
            .map(|message| message["conversationOrdinal"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4, 5, 6]
    );
    let voice_id = messages
        .iter()
        .find(|message| message["serverId"] == 101)
        .unwrap()["canonicalId"]
        .clone();
    let quote = messages
        .iter()
        .find(|message| message["serverId"] == 105)
        .unwrap();
    assert_eq!(quote["relationships"][0]["targetCanonicalId"], voice_id);
    assert_eq!(quote["relationships"][0]["resolved"], true);

    let conversations = ndjson(output.join("conversations.ndjson"));
    let group = conversations
        .iter()
        .find(|conversation| conversation["kind"] == "group")
        .unwrap();
    assert_eq!(group["participantIds"].as_array().unwrap().len(), 2);
    assert!(conversations
        .iter()
        .any(|conversation| conversation["kind"] == "business"));
    let display_name = base64::engine::general_purpose::STANDARD.encode(b"Bob in group");
    assert!(group["memberships"]
        .as_array()
        .unwrap()
        .iter()
        .any(|membership| {
            membership["role"] == "member" && membership["displayNameBase64"] == display_name
        }));
    let participants = ndjson(output.join("participants.ndjson"));
    assert_eq!(participants.len(), 4);
    assert!(participants
        .iter()
        .any(|participant| participant["remarkBase64"] == "QWxpY2UgUmVtYXJr"));

    let artifacts = ndjson(output.join("artifacts.ndjson"));
    assert!(contains_exact_path(&artifacts, &image_path));
    assert!(contains_exact_path(&artifacts, &video_path));
    assert!(contains_exact_path(&artifacts, &poster_path));
    assert!(contains_exact_path(&artifacts, &file_path));
    let decoded_image = artifacts.iter().find(|artifact| {
        artifact["sourceLocalPath"] == fs::canonicalize(&image_path).unwrap().display().to_string()
    });
    assert!(decoded_image
        .and_then(|artifact| artifact["decodedLocalPath"].as_str())
        .is_some_and(|path| Path::new(path).is_file()));
    assert!(artifacts.iter().any(|artifact| {
        artifact["availability"] == "materializedFromDatabase"
            && artifact["detectedFormat"] == "tencent-silk-v3"
            && artifact["materializedLocalPath"]
                .as_str()
                .is_some_and(|path| Path::new(path).is_file())
    }));
    assert!(artifacts
        .iter()
        .any(|artifact| artifact["availability"] == "notDownloaded"));
    assert!(artifacts.iter().any(|artifact| {
        artifact["availability"] == "unsafePath" && artifact["sourceLocalPath"].is_null()
    }));

    let audit = audit_archive(&output).unwrap();
    assert!(audit.report_matches_archive);
    assert!(audit.all_recorded_artifact_files_match);
    assert_eq!(audit.message_count, 9);
    assert_eq!(audit.artifact_reference_count, 7);
    assert_eq!(audit.verified_external_source_file_count, 4);
    assert!(audit.verified_connector_owned_file_count >= 2);
    assert!(!audit.full_restoration_verified);
    assert!(audit.completion_evidence.media_reference_corpus_observed);
    assert!(audit.completion_evidence.verified_local_media_observed);
    assert!(!audit.completion_evidence.artifact_verification_complete);
    assert!(!audit.completion_evidence.technical_restoration_complete);

    let messages_path = output.join("messages.ndjson");
    let artifacts_path = output.join("artifacts.ndjson");
    let report_path = output.join("report.json");
    let original_message_bytes = fs::read(&messages_path).unwrap();
    let original_artifact_bytes = fs::read(&artifacts_path).unwrap();
    let original_report_bytes = fs::read(&report_path).unwrap();

    let mut preferred_tampered_messages = messages.clone();
    let variants = preferred_tampered_messages
        .iter_mut()
        .find_map(|message| {
            let references = message["artifactReferences"].as_array_mut()?;
            (references.len() > 1).then_some(references)
        })
        .unwrap();
    for reference in variants {
        reference["preferred"] = serde_json::json!(true);
    }
    write_private(
        messages_path.clone(),
        (preferred_tampered_messages
            .iter()
            .map(|message| serde_json::to_string(message).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n")
            .as_bytes(),
    );
    assert!(audit_archive(&output)
        .unwrap_err()
        .to_string()
        .contains("exactly one preferred artifact"));
    write_private(messages_path.clone(), &original_message_bytes);

    let mut unavailable_tampered_artifacts = artifacts.clone();
    unavailable_tampered_artifacts
        .iter_mut()
        .find(|artifact| artifact["availability"] == "notDownloaded")
        .unwrap()["sourceByteCount"] = serde_json::json!(1);
    write_private(
        artifacts_path.clone(),
        (unavailable_tampered_artifacts
            .iter()
            .map(|artifact| serde_json::to_string(artifact).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n")
            .as_bytes(),
    );
    assert!(audit_archive(&output)
        .unwrap_err()
        .to_string()
        .contains("unexpectedly retains verified local-file evidence"));
    write_private(artifacts_path.clone(), &original_artifact_bytes);

    let mut provenance_tampered_artifacts = artifacts.clone();
    provenance_tampered_artifacts
        .iter_mut()
        .find(|artifact| artifact["availability"] == "materializedFromDatabase")
        .unwrap()["sourceResourceTableName"] = serde_json::json!("MessageResourceInfo");
    write_private(
        artifacts_path.clone(),
        (provenance_tampered_artifacts
            .iter()
            .map(|artifact| serde_json::to_string(artifact).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n")
            .as_bytes(),
    );
    assert!(audit_archive(&output)
        .unwrap_err()
        .to_string()
        .contains("does not identify a covered resource table"));
    write_private(artifacts_path.clone(), &original_artifact_bytes);

    let mut decode_tampered_artifacts = artifacts.clone();
    decode_tampered_artifacts
        .iter_mut()
        .find(|artifact| artifact["decodeState"] == "decoded")
        .unwrap()["decodedFormat"] = serde_json::Value::Null;
    write_private(
        artifacts_path.clone(),
        (decode_tampered_artifacts
            .iter()
            .map(|artifact| serde_json::to_string(artifact).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n")
            .as_bytes(),
    );
    assert!(audit_archive(&output)
        .unwrap_err()
        .to_string()
        .contains("incomplete or incompatible derivative evidence"));
    write_private(artifacts_path, &original_artifact_bytes);
    assert!(audit_archive(&output).is_ok());

    let mut state_tampered_messages = messages.clone();
    state_tampered_messages[0]["relationships"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "kind": "unknown",
            "targetCanonicalId": null,
            "targetServerId": null,
            "targetLocalId": null,
            "resolved": false,
            "resolutionState": "referenceIdentifierMissing",
            "rawReferenceBase64": null
        }));
    let state_tampered_bytes = state_tampered_messages
        .iter()
        .map(|message| serde_json::to_string(message).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    write_private(messages_path.clone(), state_tampered_bytes.as_bytes());
    let mut state_tampered_report: serde_json::Value =
        serde_json::from_slice(&original_report_bytes).unwrap();
    state_tampered_report["integrity"]["relationshipReferenceCount"] = serde_json::json!(2);
    state_tampered_report["integrity"]["unresolvedRelationshipCount"] = serde_json::json!(1);
    state_tampered_report["integrity"]["absentRelationshipTargetCount"] = serde_json::json!(1);
    write_private(
        report_path.clone(),
        &serde_json::to_vec_pretty(&state_tampered_report).unwrap(),
    );
    assert!(audit_archive(&output)
        .unwrap_err()
        .to_string()
        .contains("relationship resolution-state counts"));
    write_private(messages_path, &original_message_bytes);
    write_private(report_path, &original_report_bytes);
    assert!(audit_archive(&output).is_ok());

    let direct_conversation_id = direct_messages[0]["conversationId"]
        .as_str()
        .unwrap()
        .to_string();
    let group_conversation_id = messages
        .iter()
        .find(|message| message["conversationSourceIdentifierBase64"] != direct_identifier)
        .unwrap()["conversationId"]
        .as_str()
        .unwrap()
        .to_string();
    let policy_path = output.join("read-policy.json");
    create_conversation_policy(
        &output,
        &policy_path,
        BTreeSet::from([direct_conversation_id.clone()]),
        2,
    )
    .unwrap();
    let first_page =
        read_conversation_page(&output, &policy_path, &direct_conversation_id, None, 100).unwrap();
    assert_eq!(first_page.items.len(), 2);
    let second_page = read_conversation_page(
        &output,
        &policy_path,
        &direct_conversation_id,
        first_page.next_cursor.as_deref(),
        100,
    )
    .unwrap();
    assert_eq!(second_page.items[0].conversation_ordinal, 2);
    assert!(
        read_conversation_page(&output, &policy_path, &group_conversation_id, None, 1)
            .unwrap_err()
            .to_string()
            .contains("not enabled")
    );

    let current_archive = fixture.path().join("current-archive");
    fs::create_dir(&current_archive).unwrap();
    fs::set_permissions(&current_archive, fs::Permissions::from_mode(0o700)).unwrap();
    let mut current_report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("report.json")).unwrap()).unwrap();
    current_report["sourceFingerprint"] = serde_json::json!("next-fixture-fingerprint");
    write_private(
        current_archive.join("report.json"),
        &serde_json::to_vec_pretty(&current_report).unwrap(),
    );
    let mut current_messages = messages.clone();
    current_messages
        .iter_mut()
        .find(|message| message["conversationId"] == direct_conversation_id)
        .unwrap()["status"] = serde_json::json!(99);
    let removed_id = direct_messages[1]["canonicalId"].as_str().unwrap();
    current_messages.retain(|message| message["canonicalId"] != removed_id);
    let mut added = direct_messages[2].clone();
    added["canonicalId"] = serde_json::json!("synthetic-added-message");
    added["conversationOrdinal"] = serde_json::json!(999);
    added["serverId"] = serde_json::json!(999);
    current_messages.push(added);
    let message_bytes = current_messages
        .iter()
        .map(|message| serde_json::to_string(message).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    write_private(
        current_archive.join("messages.ndjson"),
        message_bytes.as_bytes(),
    );
    let event_directory = fixture.path().join("event-output");
    fs::create_dir(&event_directory).unwrap();
    fs::set_permissions(&event_directory, fs::Permissions::from_mode(0o700)).unwrap();
    let events_path = event_directory.join("events.ndjson");
    let delta = reconcile_archives(&output, &current_archive, &policy_path, &events_path).unwrap();
    assert_eq!(delta.previous_message_count, 7);
    assert_eq!(delta.current_message_count, 7);
    assert_eq!(delta.added_count, 1);
    assert_eq!(delta.changed_count, 1);
    assert_eq!(delta.removed_count, 1);
    assert_eq!(ndjson(events_path).len(), 3);

    let wrong_account = fixture.path().join("wxid_wrong_ab12");
    fs::create_dir_all(wrong_account.join("db_storage")).unwrap();
    fs::create_dir_all(wrong_account.join("msg")).unwrap();
    let error = restore_catalog(
        &catalog,
        &RestorationOptions {
            output_directory: fixture.path().join("wrong-account-output"),
            account_root: Some(wrong_account),
            defer_media: false,
        },
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("does not match the snapshot source scope"));

    // Optional media metadata is a degradable data surface. Losing either
    // table after catalog planning must not reject otherwise healthy messages
    // or the rest of the artifact inventory.
    let prepared_resource_db = &catalog
        .databases
        .iter()
        .find(|database| database.source_set_id == "set-resource")
        .unwrap()
        .path;
    let prepared_voice_db = &catalog
        .databases
        .iter()
        .find(|database| database.source_set_id == "set-media")
        .unwrap()
        .path;
    let prepared_contact_db = &catalog
        .databases
        .iter()
        .find(|database| database.source_set_id == "set-contact")
        .unwrap()
        .path;
    let prepared_session_db = &catalog
        .databases
        .iter()
        .find(|database| database.source_set_id == "set-session")
        .unwrap()
        .path;
    Connection::open(prepared_resource_db)
        .unwrap()
        .execute_batch("DROP TABLE MessageResourceInfo")
        .unwrap();
    Connection::open(prepared_voice_db)
        .unwrap()
        .execute_batch("DROP TABLE VoiceInfo")
        .unwrap();
    Connection::open(prepared_contact_db)
        .unwrap()
        .execute_batch("DROP TABLE contact; DROP TABLE chat_room")
        .unwrap();
    Connection::open(prepared_session_db)
        .unwrap()
        .execute_batch("DROP TABLE SessionTable")
        .unwrap();
    let degraded_output = fixture.path().join("degraded-media-metadata");
    let degraded_report = restore_catalog(
        &catalog,
        &RestorationOptions {
            output_directory: degraded_output.clone(),
            account_root: Some(account),
            defer_media: false,
        },
    )
    .unwrap();
    assert_eq!(degraded_report.integrity.restored_row_count, 9);
    assert_eq!(ndjson(degraded_output.join("messages.ndjson")).len(), 9);
    assert!(degraded_report.integrity.missing_local_profile_count > 0);
    assert!(
        degraded_report.integrity.missing_artifact_count >= 3,
        "unexpected missing artifact count: {}",
        degraded_report.integrity.missing_artifact_count
    );
    assert!(ndjson(degraded_output.join("artifacts.ndjson"))
        .iter()
        .filter_map(|artifact| artifact["verificationDetail"].as_str())
        .any(|detail| detail.contains("optional VoiceInfo tables were unavailable")));
    assert!(audit_archive(&degraded_output).is_ok());

    fs::write(&video_path, b"substituted-after-restoration").unwrap();
    assert!(audit_archive(&output)
        .unwrap_err()
        .to_string()
        .contains("no longer matches recorded evidence"));
}

fn entry(
    path: &Path,
    account: &Path,
    set_id: &str,
    logical_path: &str,
    index: usize,
) -> SnapshotEntry {
    let bytes = fs::read(path).unwrap();
    let source_path = fs::canonicalize(account)
        .unwrap()
        .join("db_storage")
        .join(logical_path);
    let source_id = hex::encode(Sha256::digest(source_path.to_string_lossy().as_bytes()));
    SnapshotEntry {
        source: PathReference {
            opaque_id: source_id[..24].to_string(),
            path: None,
        },
        source_set_id: set_id.to_string(),
        logical_path: logical_path.to_string(),
        relative_path: format!("sets/{index:04}/database.db"),
        role: SnapshotFileRole::Database,
        fingerprint: SourceFileFingerprint {
            device_id: 1,
            file_id: index as u64 + 1,
            byte_count: bytes.len() as i64,
            modified_seconds: 0,
            modified_nanoseconds: 0,
        },
        sha256: hex::encode(Sha256::digest(&bytes)),
    }
}

fn ndjson(path: PathBuf) -> Vec<serde_json::Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn write_private(path: PathBuf, bytes: &[u8]) {
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn contains_exact_path(artifacts: &[serde_json::Value], expected: &Path) -> bool {
    let canonical = fs::canonicalize(expected).unwrap().display().to_string();
    artifacts
        .iter()
        .any(|artifact| artifact["sourceLocalPath"] == canonical)
}
