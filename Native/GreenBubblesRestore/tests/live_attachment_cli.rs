use std::fs;
use std::io::Write;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rusqlite::{params, Connection};
use serde_json::Value;
use sha2::{Digest, Sha256};

const CONVERSATION: &str = "wxid_attachment_friend";
const SOURCE_MD5: &str = "0123456789abcdef0123456789abcdef";
const MESSAGE_CONVERSATION: &str = "wxid_media_friend";
const IMAGE_MD5: &str = "00000000000000000000000000000000";
const VIDEO_MD5: &str = "11111111111111111111111111111111";
const DOCUMENT_MD5: &str = "22222222222222222222222222222222";

#[test]
fn attachment_commands_expose_help_without_accessing_an_account() {
    for arguments in [
        vec!["attachment", "--help"],
        vec!["help", "attachment"],
        vec!["attachment", "inspect", "--help"],
        vec!["attachment", "materialize", "--help"],
    ] {
        let output = run(&arguments);
        assert_success(&output);
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.starts_with("Usage:\n"));
        assert!(stdout.contains("attachment"));
    }
}

#[test]
fn inspect_writes_nothing_and_materialize_creates_exactly_one_private_image() {
    let fixture = Fixture::new();
    let before_inspection = relative_files(fixture.directory.path());
    let inspection = run(&[
        "attachment",
        "inspect",
        fixture.account.to_str().unwrap(),
        "--conversation",
        CONVERSATION,
        "--md5",
        SOURCE_MD5,
    ]);
    assert_success(&inspection);
    let inspection_stdout = String::from_utf8(inspection.stdout).unwrap();
    assert!(!inspection_stdout.contains(fixture.account.to_str().unwrap()));
    let inspection: Value = serde_json::from_str(&inspection_stdout).unwrap();
    assert_eq!(inspection["schema"], "greenbubbles.attachment.v1");
    assert_eq!(inspection["formatVersion"], 1);
    assert_eq!(inspection["operation"], "attachment.inspect");
    assert_eq!(inspection["ok"], true);
    assert_eq!(inspection["availability"], "downloaded");
    assert_eq!(inspection["candidateCount"], 1);
    assert_eq!(inspection["sourcePathReleased"], false);
    assert_eq!(
        relative_files(fixture.directory.path()),
        before_inspection,
        "inspection must not create an archive, derivative, cache, or temporary file"
    );

    let attachment_id = inspection["preferredAttachmentId"].as_str().unwrap();
    let output_path = fixture.output_directory.join("decoded.jpg");
    let before_materialization = relative_files(fixture.directory.path());
    let materialization = run(&[
        "attachment",
        "materialize",
        fixture.account.to_str().unwrap(),
        "--conversation",
        CONVERSATION,
        "--md5",
        SOURCE_MD5,
        "--attachment",
        attachment_id,
        "--output",
        output_path.to_str().unwrap(),
    ]);
    assert_success(&materialization);
    let materialization_stdout = String::from_utf8(materialization.stdout).unwrap();
    assert!(!materialization_stdout.contains(fixture.account.to_str().unwrap()));
    assert!(!materialization_stdout.contains(output_path.to_str().unwrap()));
    let materialization: Value = serde_json::from_str(&materialization_stdout).unwrap();
    assert_eq!(materialization["schema"], "greenbubbles.attachment.v1");
    assert_eq!(materialization["operation"], "attachment.materialize");
    assert_eq!(materialization["decodedFormat"], "jpg");
    assert_eq!(materialization["decodedByteCount"], fixture.decoded.len());
    assert_eq!(
        materialization["decodedSha256"],
        hex::encode(Sha256::digest(&fixture.decoded))
    );
    assert_eq!(materialization["sourcePathReleased"], false);
    assert_eq!(materialization["outputPathReleased"], false);
    assert_eq!(fs::read(&output_path).unwrap(), fixture.decoded);
    assert_eq!(
        fs::metadata(&output_path).unwrap().permissions().mode() & 0o077,
        0
    );

    let mut expected_after = before_materialization;
    expected_after.push(
        output_path
            .strip_prefix(fixture.directory.path())
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    expected_after.sort();
    assert_eq!(relative_files(fixture.directory.path()), expected_after);

    let overwrite = run(&[
        "attachment",
        "materialize",
        fixture.account.to_str().unwrap(),
        "--conversation",
        CONVERSATION,
        "--md5",
        SOURCE_MD5,
        "--attachment",
        attachment_id,
        "--output",
        output_path.to_str().unwrap(),
    ]);
    assert!(!overwrite.status.success());
    let error: Value = serde_json::from_slice(&overwrite.stdout).unwrap();
    assert_eq!(error["schema"], "greenbubbles.attachment.v1");
    assert_eq!(error["error"]["code"], "outputRejected");
    assert_eq!(fs::read(&output_path).unwrap(), fixture.decoded);
    assert_eq!(relative_files(fixture.directory.path()), expected_after);
}

#[test]
fn attachment_cli_rejects_unbounded_or_mismatched_requests_without_path_disclosure() {
    let fixture = Fixture::new();
    let original_files = relative_files(fixture.directory.path());
    for arguments in [
        vec![
            "attachment",
            "inspect",
            fixture.account.to_str().unwrap(),
            "--conversation",
            CONVERSATION,
            "--md5",
            SOURCE_MD5,
            "--all",
        ],
        vec![
            "attachment",
            "materialize",
            fixture.account.to_str().unwrap(),
            "--conversation",
            CONVERSATION,
            "--md5",
            SOURCE_MD5,
            "--attachment",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--output",
            fixture
                .output_directory
                .join("absent.jpg")
                .to_str()
                .unwrap(),
        ],
    ] {
        let output = run(&arguments);
        assert!(!output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        let error: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(error["schema"], "greenbubbles.attachment.v1");
        assert_eq!(error["ok"], false);
        assert!(!stdout.contains(fixture.account.to_str().unwrap()));
        assert!(!stderr.contains(fixture.account.to_str().unwrap()));
        assert!(stderr.contains("see the JSON error"));
    }
    assert_eq!(relative_files(fixture.directory.path()), original_files);
}

#[test]
fn exact_message_cli_lazily_materializes_image_voice_video_and_document() {
    let fixture = MessageFixture::new();
    let ids = fixture.message_ids();
    let cases = [
        ("image", ids.image.as_str(), fixture.image.as_slice(), "jpg"),
        (
            "voice",
            ids.voice.as_str(),
            fixture.voice.as_slice(),
            "silk",
        ),
        ("video", ids.video.as_str(), fixture.video.as_slice(), "mp4"),
        (
            "document",
            ids.document.as_str(),
            fixture.document.as_slice(),
            "pdf",
        ),
    ];

    for (kind, message_id, expected, expected_format) in cases {
        let before_inspection = relative_files(fixture.directory.path());
        let inspection = run(&[
            "attachment",
            "inspect",
            fixture.account.to_str().unwrap(),
            "--conversation",
            MESSAGE_CONVERSATION,
            "--message",
            message_id,
            "--kind",
            kind,
            "--decrypted",
        ]);
        assert!(
            inspection.status.success(),
            "{kind} inspection failed; stderr: {}; stdout: {}",
            String::from_utf8_lossy(&inspection.stderr),
            String::from_utf8_lossy(&inspection.stdout)
        );
        let stdout = String::from_utf8(inspection.stdout).unwrap();
        assert!(!stdout.contains(fixture.account.to_str().unwrap()));
        assert!(!stdout.contains(fixture.database_root.to_str().unwrap()));
        let inspection: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(inspection["schema"], "greenbubbles.attachment.v1");
        assert_eq!(inspection["operation"], "attachment.inspect");
        assert_eq!(inspection["kind"], kind);
        assert_eq!(inspection["candidateCount"], 1);
        assert_eq!(inspection["sourcePathReleased"], false);
        assert_eq!(
            relative_files(fixture.directory.path()),
            before_inspection,
            "message-bound inspection must be side-effect free"
        );

        let attachment_id = inspection["preferredAttachmentId"].as_str().unwrap();
        let output_path = fixture.output.join(format!("{kind}.materialized"));
        let materialization = run(&[
            "attachment",
            "materialize",
            fixture.account.to_str().unwrap(),
            "--conversation",
            MESSAGE_CONVERSATION,
            "--message",
            message_id,
            "--kind",
            kind,
            "--attachment",
            attachment_id,
            "--output",
            output_path.to_str().unwrap(),
            "--decrypted",
        ]);
        assert_success(&materialization);
        let stdout = String::from_utf8(materialization.stdout).unwrap();
        assert!(!stdout.contains(fixture.account.to_str().unwrap()));
        assert!(!stdout.contains(output_path.to_str().unwrap()));
        let materialization: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(materialization["kind"], kind);
        assert_eq!(materialization["decodedFormat"], expected_format);
        assert_eq!(materialization["decodedByteCount"], expected.len());
        assert_eq!(
            materialization["decodedSha256"],
            hex::encode(Sha256::digest(expected))
        );
        assert_eq!(materialization["sourcePathReleased"], false);
        assert_eq!(materialization["outputPathReleased"], false);
        assert_eq!(fs::read(&output_path).unwrap(), expected);
        assert_eq!(
            fs::metadata(&output_path).unwrap().permissions().mode() & 0o077,
            0
        );

        if kind == "video" {
            let overwrite = run(&[
                "attachment",
                "materialize",
                fixture.account.to_str().unwrap(),
                "--conversation",
                MESSAGE_CONVERSATION,
                "--message",
                message_id,
                "--kind",
                kind,
                "--attachment",
                attachment_id,
                "--output",
                output_path.to_str().unwrap(),
                "--decrypted",
            ]);
            assert_attachment_failure(&overwrite, "outputRejected", &fixture);
            assert_eq!(fs::read(&output_path).unwrap(), expected);
        }
    }
}

#[test]
fn exact_message_requests_require_one_access_mode_and_bind_every_identity_dimension() {
    let fixture = MessageFixture::new();
    let other_source = MessageFixture::new();
    let ids = fixture.message_ids();
    let original_files = relative_files(fixture.directory.path());

    let cases = [
        vec![
            "attachment",
            "inspect",
            fixture.account.to_str().unwrap(),
            "--conversation",
            MESSAGE_CONVERSATION,
            "--message",
            ids.video.as_str(),
            "--kind",
            "video",
        ],
        vec![
            "attachment",
            "inspect",
            fixture.account.to_str().unwrap(),
            "--conversation",
            MESSAGE_CONVERSATION,
            "--message",
            ids.video.as_str(),
            "--kind",
            "video",
            "--decrypted",
            "--snapshot-key-stdin",
        ],
        vec![
            "attachment",
            "inspect",
            fixture.account.to_str().unwrap(),
            "--conversation",
            "wxid_wrong_conversation",
            "--message",
            ids.video.as_str(),
            "--kind",
            "video",
            "--decrypted",
        ],
        vec![
            "attachment",
            "inspect",
            fixture.account.to_str().unwrap(),
            "--conversation",
            MESSAGE_CONVERSATION,
            "--message",
            ids.video.as_str(),
            "--kind",
            "document",
            "--decrypted",
        ],
        vec![
            "attachment",
            "inspect",
            other_source.account.to_str().unwrap(),
            "--conversation",
            MESSAGE_CONVERSATION,
            "--message",
            ids.video.as_str(),
            "--kind",
            "video",
            "--decrypted",
        ],
        vec![
            "attachment",
            "inspect",
            fixture.account.to_str().unwrap(),
            "--conversation",
            MESSAGE_CONVERSATION,
            "--message",
            ids.video.as_str(),
            "--kind",
            "video",
            "--md5",
            VIDEO_MD5,
            "--decrypted",
        ],
    ];

    for arguments in cases {
        let failure = run(&arguments);
        assert_attachment_failure(&failure, "invalidAttachmentRequest", &fixture);
    }
    assert_eq!(relative_files(fixture.directory.path()), original_files);
}

#[test]
fn exact_message_cli_rejects_oversized_and_symlinked_sources_without_partial_output() {
    let oversized = MessageFixture::new();
    let oversized_ids = oversized.message_ids();
    fs::OpenOptions::new()
        .write(true)
        .open(&oversized.video_path)
        .unwrap()
        .set_len(2 * 1024 * 1024 * 1024 + 1)
        .unwrap();
    let before = relative_files(oversized.directory.path());
    let failure = inspect_exact(&oversized, &oversized_ids.video, "video");
    assert_attachment_failure(&failure, "unsafeSource", &oversized);
    assert_eq!(relative_files(oversized.directory.path()), before);

    let symlinked = MessageFixture::new();
    let symlinked_ids = symlinked.message_ids();
    let external = symlinked.directory.path().join("outside-video.mp4");
    fs::write(&external, b"outside account").unwrap();
    fs::remove_file(&symlinked.video_path).unwrap();
    symlink(&external, &symlinked.video_path).unwrap();
    let before = relative_files(symlinked.directory.path());
    let failure = inspect_exact(&symlinked, &symlinked_ids.video, "video");
    assert_attachment_failure(&failure, "unsafeSource", &symlinked);
    assert_eq!(relative_files(symlinked.directory.path()), before);

    let unsafe_metadata = MessageFixture::new();
    let unsafe_ids = unsafe_metadata.message_ids();
    Connection::open(unsafe_metadata.database_root.join("hardlink/hardlink.db"))
        .unwrap()
        .execute(
            "UPDATE video_hardlink_info_v3 SET file_name = '../escape.mp4'",
            [],
        )
        .unwrap();
    let before = relative_files(unsafe_metadata.directory.path());
    let failure = inspect_exact(&unsafe_metadata, &unsafe_ids.video, "video");
    assert_attachment_failure(&failure, "unsafeSource", &unsafe_metadata);
    assert_eq!(relative_files(unsafe_metadata.directory.path()), before);
}

#[test]
fn exact_message_cli_enforces_candidate_and_directory_traversal_bounds() {
    let candidates = MessageFixture::new();
    let candidate_ids = candidates.message_ids();
    let directory = candidates
        .account
        .join("msg/video")
        .join(MESSAGE_CONVERSATION)
        .join("2026-09");
    fs::create_dir_all(&directory).unwrap();
    for index in 0..257 {
        fs::write(
            directory.join(format!("{VIDEO_MD5}-{index:03}.mp4")),
            b"bounded-candidate",
        )
        .unwrap();
    }
    let before = relative_files(candidates.directory.path());
    let failure = inspect_exact(&candidates, &candidate_ids.video, "video");
    assert_attachment_failure(&failure, "attachmentUnavailable", &candidates);
    assert_eq!(relative_files(candidates.directory.path()), before);

    let directories = MessageFixture::new();
    let directory_ids = directories.message_ids();
    let root = directories
        .account
        .join("msg/video")
        .join(MESSAGE_CONVERSATION);
    for index in 0..4_096 {
        fs::create_dir_all(root.join(format!("month-{index:04}"))).unwrap();
    }
    let before = relative_files(directories.directory.path());
    let failure = inspect_exact(&directories, &directory_ids.video, "video");
    assert_attachment_failure(&failure, "attachmentUnavailable", &directories);
    assert_eq!(relative_files(directories.directory.path()), before);
}

struct Fixture {
    directory: tempfile::TempDir,
    account: PathBuf,
    output_directory: PathBuf,
    decoded: Vec<u8>,
}

struct MessageIds {
    image: String,
    voice: String,
    video: String,
    document: String,
}

struct MessageFixture {
    directory: tempfile::TempDir,
    account: PathBuf,
    database_root: PathBuf,
    output: PathBuf,
    video_path: PathBuf,
    image: Vec<u8>,
    voice: Vec<u8>,
    video: Vec<u8>,
    document: Vec<u8>,
}

impl MessageFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let account = directory.path().join("account");
        let database_root = account.join("db_storage");
        let output = directory.path().join("output");
        for path in [
            database_root.join("contact"),
            database_root.join("session"),
            database_root.join("message"),
            database_root.join("media"),
            database_root.join("hardlink"),
            output.clone(),
        ] {
            fs::create_dir_all(&path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }

        Connection::open(database_root.join("contact/contact.db"))
            .unwrap()
            .execute_batch(
                "CREATE TABLE contact(username TEXT PRIMARY KEY, remark TEXT, nick_name TEXT, alias TEXT);
                 INSERT INTO contact VALUES ('wxid_sender', 'Sender', '', '');",
            )
            .unwrap();
        Connection::open(database_root.join("session/session.db"))
            .unwrap()
            .execute_batch(
                "CREATE TABLE SessionTable(username TEXT, sort_timestamp INTEGER, summary BLOB);
                 INSERT INTO SessionTable VALUES ('wxid_media_friend', 1, 'media');",
            )
            .unwrap();

        let message_table = format!("Msg_{:x}", md5::compute(MESSAGE_CONVERSATION.as_bytes()));
        let message = Connection::open(database_root.join("message/message_0.db")).unwrap();
        message
            .execute_batch(&format!(
                "CREATE TABLE Name2Id(user_name TEXT);
                 INSERT INTO Name2Id(rowid, user_name) VALUES (1, 'wxid_sender');
                 CREATE TABLE [{message_table}](
                    server_id INTEGER,
                    sort_seq INTEGER,
                    local_type INTEGER,
                    real_sender_id INTEGER,
                    create_time INTEGER,
                    status INTEGER,
                    message_content BLOB,
                    packed_info_data BLOB,
                    WCDB_CT_message_content INTEGER,
                    compress_content BLOB
                 );"
            ))
            .unwrap();
        let image_packed = wx_db::encode_packed_info_for_test(Some(IMAGE_MD5), None);
        let video_packed = wx_db::encode_packed_info_for_test(None, Some(VIDEO_MD5));
        let document_xml = format!(
            "<msg><appmsg><title>report.pdf</title><fileext>pdf</fileext><totallen>24</totallen><md5>{DOCUMENT_MD5}</md5></appmsg></msg>"
        );
        for (server_id, sort_sequence, local_type, content, packed) in [
            (3001, 400, 3_i64, Vec::new(), image_packed),
            (3002, 300, 34_i64, Vec::new(), Vec::new()),
            (3003, 200, 43_i64, Vec::new(), video_packed),
            (
                3004,
                100,
                ((6_i64) << 32) | 49,
                document_xml.into_bytes(),
                Vec::new(),
            ),
        ] {
            message
                .execute(
                    &format!(
                        "INSERT INTO [{message_table}](server_id, sort_seq, local_type, real_sender_id, create_time, status, message_content, packed_info_data, WCDB_CT_message_content) VALUES (?1, ?2, ?3, 1, ?2, 0, ?4, ?5, 0)"
                    ),
                    params![server_id, sort_sequence, local_type, content, packed],
                )
                .unwrap();
        }
        drop(message);

        let voice = b"\x02#!SILK_V3synthetic-lossless-voice".to_vec();
        let media = Connection::open(database_root.join("media/media_0.db")).unwrap();
        media
            .execute_batch("CREATE TABLE VoiceInfo(svr_id INTEGER, voice_data BLOB);")
            .unwrap();
        media
            .execute(
                "INSERT INTO VoiceInfo VALUES (?1, ?2)",
                params![3002_i64, &voice],
            )
            .unwrap();
        drop(media);

        let hardlink = Connection::open(database_root.join("hardlink/hardlink.db")).unwrap();
        hardlink
            .execute_batch(
                "CREATE TABLE dir2id(rowid INTEGER PRIMARY KEY, username TEXT);
                 INSERT INTO dir2id VALUES (1, 'wxid_media_friend');
                 INSERT INTO dir2id VALUES (2, '2026-08');
                 CREATE TABLE video_hardlink_info_v3(md5 TEXT, file_name TEXT, file_size INTEGER, modify_time INTEGER, dir1 INTEGER, dir2 INTEGER);",
            )
            .unwrap();
        hardlink
            .execute(
                "INSERT INTO video_hardlink_info_v3 VALUES (?1, 'custom-video.mp4', 24, 1, 1, 2)",
                [VIDEO_MD5],
            )
            .unwrap();
        drop(hardlink);

        let image = b"\xff\xd8\xffsynthetic-message-jpeg".to_vec();
        let image_directory = account
            .join("msg/attach")
            .join(format!(
                "{:x}",
                md5::compute(MESSAGE_CONVERSATION.as_bytes())
            ))
            .join("2026-08")
            .join("Img");
        fs::create_dir_all(&image_directory).unwrap();
        let image_key = 0x5Au8;
        fs::write(
            image_directory.join(format!("{IMAGE_MD5}_h.dat")),
            image
                .iter()
                .map(|byte| byte ^ image_key)
                .collect::<Vec<_>>(),
        )
        .unwrap();

        let video = b"\x00\x00\x00\x18ftypmp42synthetic-video".to_vec();
        let document = b"%PDF-1.7 synthetic report".to_vec();
        let video_directory = account
            .join("msg/attach")
            .join(MESSAGE_CONVERSATION)
            .join("2026-08")
            .join("Video");
        let document_directory = account
            .join("msg/file")
            .join(MESSAGE_CONVERSATION)
            .join("2026-08");
        fs::create_dir_all(&video_directory).unwrap();
        fs::create_dir_all(&document_directory).unwrap();
        let video_path = video_directory.join("custom-video.mp4");
        fs::write(&video_path, &video).unwrap();
        fs::write(document_directory.join("report.pdf"), &document).unwrap();

        Self {
            directory,
            account,
            database_root,
            output,
            video_path,
            image,
            voice,
            video,
            document,
        }
    }

    fn message_ids(&self) -> MessageIds {
        let output = run(&[
            "messages",
            "list",
            self.database_root.to_str().unwrap(),
            "--conversation",
            MESSAGE_CONVERSATION,
            "--limit",
            "10",
            "--decrypted",
        ]);
        assert_success(&output);
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        let find = |message_type: u64, message_subtype: u64| {
            response["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| {
                    item["messageType"].as_u64() == Some(message_type)
                        && item["messageSubtype"].as_u64() == Some(message_subtype)
                })
                .unwrap()
                .get("id")
                .and_then(Value::as_str)
                .unwrap()
                .to_string()
        };
        MessageIds {
            image: find(3, 0),
            voice: find(34, 0),
            video: find(43, 0),
            document: find(49, 6),
        }
    }
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let account = directory.path().join("account");
        let output_directory = directory.path().join("output");
        fs::create_dir_all(&output_directory).unwrap();
        fs::set_permissions(&output_directory, fs::Permissions::from_mode(0o700)).unwrap();

        let conversation_hash = format!("{:x}", md5::compute(CONVERSATION.as_bytes()));
        let image_directory = account
            .join("msg/attach")
            .join(conversation_hash)
            .join("2026-08")
            .join("Img");
        fs::create_dir_all(&image_directory).unwrap();
        let decoded = b"\xff\xd8\xffsynthetic-cli-jpeg".to_vec();
        let xor_key = 0x5Au8;
        let encrypted = decoded
            .iter()
            .map(|byte| byte ^ xor_key)
            .collect::<Vec<_>>();
        fs::write(
            image_directory.join(format!("{SOURCE_MD5}_h.dat")),
            encrypted,
        )
        .unwrap();

        Self {
            directory,
            account,
            output_directory,
            decoded,
        }
    }
}

fn relative_files(root: &Path) -> Vec<String> {
    let mut files = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn run(arguments: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_greenbubbles"))
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().map(|mut stdin| stdin.write_all(&[]));
    child.wait_with_output().unwrap()
}

fn inspect_exact(fixture: &MessageFixture, message_id: &str, kind: &str) -> Output {
    run(&[
        "attachment",
        "inspect",
        fixture.account.to_str().unwrap(),
        "--conversation",
        MESSAGE_CONVERSATION,
        "--message",
        message_id,
        "--kind",
        kind,
        "--decrypted",
    ])
}

fn assert_attachment_failure(output: &Output, code: &str, fixture: &MessageFixture) {
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["schema"], "greenbubbles.attachment.v1");
    assert_eq!(error["ok"], false);
    assert_eq!(error["error"]["code"], code);
    assert!(!stdout.contains(fixture.account.to_str().unwrap()));
    assert!(!stdout.contains(fixture.database_root.to_str().unwrap()));
    assert!(!stdout.contains(fixture.output.to_str().unwrap()));
    assert!(!stderr.contains(fixture.account.to_str().unwrap()));
    assert!(!stderr.contains(fixture.database_root.to_str().unwrap()));
    assert!(!stderr.contains(fixture.output.to_str().unwrap()));
    assert!(stderr.contains("see the JSON error"));
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed; stderr: {}; stdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
}
