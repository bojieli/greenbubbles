use std::fs;
use std::path::{Path, PathBuf};

use greenbubbles_restore::manifest::{
    PathReference, SnapshotEntry, SnapshotFileRole, SnapshotManifest, SourceFileFingerprint,
};
use greenbubbles_restore::{prepare_catalog, restore_catalog, RestorationOptions};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

const CHAT: &str = "wxid_alice";
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
    fs::create_dir_all(message_db.parent().unwrap()).unwrap();
    fs::create_dir_all(resource_db.parent().unwrap()).unwrap();
    fs::create_dir_all(media_db.parent().unwrap()).unwrap();

    let image_packed = wx_db::encode_packed_info_for_test(Some(IMAGE_MD5), None);
    let video_packed = wx_db::encode_packed_info_for_test(None, Some(VIDEO_MD5));
    let missing_packed = wx_db::encode_packed_info_for_test(Some(MISSING_MD5), None);
    let table = format!("Msg_{:x}", md5::compute(CHAT.as_bytes()));
    let connection = Connection::open(&message_db).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TABLE Name2Id(user_name TEXT);
             INSERT INTO Name2Id(rowid, user_name) VALUES (1, '{CHAT}');
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

    let manifest = SnapshotManifest {
        manifest_format_version: 1,
        snapshot_id: "00000000-0000-4000-8000-000000000003".to_string(),
        created_at: "2026-08-27T00:00:00Z".to_string(),
        source_fingerprint: "multimodal-fixture-fingerprint".to_string(),
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
        ],
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
            account_root: Some(account.clone()),
        },
    )
    .unwrap();

    assert!(report.integrity.row_equation_holds());
    assert_eq!(report.integrity.source_row_count, 7);
    assert_eq!(report.integrity.restored_row_count, 7);
    assert_eq!(report.integrity.rejected_row_count, 0);
    assert_eq!(report.integrity.artifact_reference_count, 7);
    assert_eq!(report.integrity.unique_artifact_count, 7);
    assert_eq!(report.integrity.missing_artifact_count, 1);
    assert_eq!(report.integrity.decoded_artifact_count, 1);
    assert_eq!(report.integrity.unsafe_artifact_count, 1);
    assert_eq!(report.integrity.relationship_reference_count, 1);
    assert_eq!(report.integrity.resolved_relationship_count, 1);

    let messages = ndjson(output.join("messages.ndjson"));
    let order = messages
        .iter()
        .map(|message| message["sortSequence"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(order, vec![100, 200, 300, 400, 500, 600, 700]);
    assert!(messages.iter().take(5).all(|message| {
        message["artifactReferences"]
            .as_array()
            .is_some_and(|value| !value.is_empty())
    }));
    assert!(messages
        .iter()
        .all(|message| message["orderingBasis"] == "sortSequence"));
    assert_eq!(
        messages
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
            && artifact["decodedFormat"] == "silk"
            && artifact["decodedLocalPath"]
                .as_str()
                .is_some_and(|path| Path::new(path).is_file())
    }));
    assert!(artifacts
        .iter()
        .any(|artifact| artifact["availability"] == "notDownloaded"));
    assert!(artifacts.iter().any(|artifact| {
        artifact["availability"] == "unsafePath" && artifact["sourceLocalPath"].is_null()
    }));

    let wrong_account = fixture.path().join("wxid_wrong_ab12");
    fs::create_dir_all(wrong_account.join("db_storage")).unwrap();
    fs::create_dir_all(wrong_account.join("msg")).unwrap();
    let error = restore_catalog(
        &catalog,
        &RestorationOptions {
            output_directory: fixture.path().join("wrong-account-output"),
            account_root: Some(wrong_account),
        },
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("does not match the snapshot source scope"));
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

fn contains_exact_path(artifacts: &[serde_json::Value], expected: &Path) -> bool {
    let canonical = fs::canonicalize(expected).unwrap().display().to_string();
    artifacts
        .iter()
        .any(|artifact| artifact["sourceLocalPath"] == canonical)
}
