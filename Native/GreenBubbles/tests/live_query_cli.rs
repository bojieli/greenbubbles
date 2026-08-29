use std::fs;
use std::io::Write;
use std::os::raw::c_void;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Instant;

use rusqlite::{params, Connection};
use serde_json::Value;

use greenbubbles::live_query::QueryDatabaseAccess;
use greenbubbles::recoverable_snapshot::create_recoverable_snapshot_with_recovery_words_and_optional_protectors;
use greenbubbles::snapshot_protector::{
    SnapshotLocalCredential, SnapshotPassphrase, SnapshotRecoveryWords,
};

const RAW_KEY: [u8; 32] = [0xAB; 32];

#[test]
fn resource_commands_expose_help_without_opening_a_database() {
    for command in ["source", "conversations", "messages", "message"] {
        for arguments in [vec![command, "--help"], vec!["help", command]] {
            let output = Command::new(env!("CARGO_BIN_EXE_greenbubbles"))
                .args(arguments)
                .output()
                .unwrap();
            assert!(output.status.success());
            assert!(output.stderr.is_empty());
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert!(stdout.starts_with("Usage:\n"));
            assert!(stdout.contains("bounded"));
            assert!(stdout.contains("--passphrase-stdin"));
            assert!(stdout.contains("--decrypted"));
        }
    }
}

#[test]
fn decrypted_cli_returns_versioned_cursor_pages_without_creating_an_archive() {
    let fixture = Fixture::new(false);
    let status = run(
        &[
            "source",
            "status",
            fixture.root.to_str().unwrap(),
            "--decrypted",
        ],
        None,
    );
    assert_success(&status);
    let status_stdout = String::from_utf8(status.stdout).unwrap();
    assert!(!status_stdout.contains(fixture.root.to_str().unwrap()));
    let status: Value = serde_json::from_str(&status_stdout).unwrap();
    assert_eq!(status["schema"], "greenbubbles.query.v1");
    assert_eq!(status["operation"], "source.status");
    assert_eq!(status["databaseCount"], 5);
    assert_eq!(status["entries"].as_array().unwrap().len(), 5);
    assert_eq!(status["writeAheadLogBytes"], 0);
    assert_eq!(status["sharedMemoryBytes"], 0);
    assert_eq!(status["rollbackJournalBytes"], 0);
    assert_eq!(status["totalSqliteStorageBytes"], status["databaseBytes"]);

    let first = run(
        &[
            "conversations",
            "list",
            fixture.root.to_str().unwrap(),
            "--decrypted",
            "--limit",
            "2",
        ],
        None,
    );
    assert_success(&first);
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["schema"], "greenbubbles.query.v1");
    assert_eq!(first["operation"], "conversations.list");
    assert_eq!(first["source"]["mode"], "decrypted");
    assert_eq!(first["items"][0]["displayName"], "Remark A");
    assert_eq!(first["page"]["returned"], 2);
    assert_eq!(first["page"]["hasMore"], true);
    let cursor = first["page"]["nextCursor"].as_str().unwrap();

    let second = run(
        &[
            "conversations",
            "list",
            fixture.root.to_str().unwrap(),
            "--decrypted",
            "--limit",
            "2",
            "--cursor",
            cursor,
        ],
        None,
    );
    assert_success(&second);
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second["items"][0]["id"], "wxid_c");
    assert_eq!(second["items"][1]["id"], "wxid_d");

    let messages = run(
        &[
            "messages",
            "list",
            fixture.root.to_str().unwrap(),
            "--decrypted",
            "--conversation",
            "wxid_talker",
            "--limit",
            "3",
        ],
        None,
    );
    assert_success(&messages);
    let messages: Value = serde_json::from_slice(&messages.stdout).unwrap();
    assert_eq!(messages["operation"], "messages.list");
    assert_eq!(
        messages["consistency"]["guarantee"],
        "perDatabaseReadStatement"
    );
    assert_eq!(messages["consistency"]["databaseCount"], 3);
    assert_eq!(messages["consistency"]["crossDatabaseAtomic"], false);
    assert_eq!(messages["page"]["returned"], 3);
    assert_eq!(messages["items"][0]["content"]["Text"], "s1-new");
    assert_eq!(messages["items"][0]["senderDisplayName"], "Sender Remark");
    assert_eq!(messages["items"][1]["content"]["Text"], "s0-new");

    let message_id = messages["items"][0]["id"].as_str().unwrap().to_string();
    let exact = run(
        &[
            "message",
            "get",
            fixture.root.to_str().unwrap(),
            "--decrypted",
            "--conversation",
            "wxid_talker",
            "--message",
            &message_id,
        ],
        None,
    );
    assert_success(&exact);
    let exact: Value = serde_json::from_slice(&exact.stdout).unwrap();
    assert_eq!(exact["operation"], "message.get");
    assert_eq!(exact["consistency"]["databaseCount"], 2);
    assert_eq!(exact["consistency"]["crossDatabaseAtomic"], false);
    assert_eq!(exact["item"]["id"], message_id);
    assert_eq!(exact["item"]["content"]["Text"], "s1-new");

    assert_eq!(fixture.relative_files(), fixture.original_files);
    assert!(!fixture.root.join("messages.ndjson").exists());
    assert!(!fixture.root.join("staging.sqlite").exists());
}

#[test]
fn exact_message_identity_is_bound_to_source_and_conversation() {
    let fixture = Fixture::new(false);
    let listed = run(
        &[
            "messages",
            "list",
            fixture.root.to_str().unwrap(),
            "--decrypted",
            "--conversation",
            "wxid_talker",
            "--limit",
            "1",
        ],
        None,
    );
    assert_success(&listed);
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    let message_id = listed["items"][0]["id"].as_str().unwrap().to_string();

    for (conversation, identity) in [
        ("wxid_other", message_id.as_str()),
        ("wxid_talker", "not-an-opaque-message-identity"),
    ] {
        let failure = run(
            &[
                "message",
                "get",
                fixture.root.to_str().unwrap(),
                "--decrypted",
                "--conversation",
                conversation,
                "--message",
                identity,
            ],
            None,
        );
        assert!(!failure.status.success());
        let error: Value = serde_json::from_slice(&failure.stdout).unwrap();
        assert_eq!(error["schema"], "greenbubbles.query.v1");
        assert_eq!(error["operation"], "message.get");
        assert_eq!(error["error"]["code"], "invalidCursor");
    }
    assert_eq!(fixture.relative_files(), fixture.original_files);
}

#[test]
fn native_search_is_bounded_keyset_paginated_and_query_bound() {
    let fixture = Fixture::new(false);
    let direct_source = greenbubbles::live_query::LiveQuerySource::open(
        &fixture.root,
        greenbubbles::live_query::QueryDatabaseAccess::Decrypted,
    )
    .unwrap();
    let direct = greenbubbles::live_query::search_messages(&direct_source, "hello", None, 1, None);
    assert!(direct.is_ok(), "direct search failed: {direct:?}");
    let first = run(
        &[
            "messages",
            "search",
            fixture.root.to_str().unwrap(),
            "--decrypted",
            "--query-stdin",
            "--limit",
            "1",
        ],
        Some(b"hello\n"),
    );
    assert_success(&first);
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["operation"], "messages.search");
    assert_eq!(first["consistency"]["databaseCount"], 2);
    assert_eq!(first["consistency"]["crossDatabaseAtomic"], false);
    assert_eq!(first["items"][0]["senderDisplayName"], "Sender Remark");
    assert_eq!(first["page"]["returned"], 1);
    assert_eq!(first["page"]["hasMore"], true);
    assert_eq!(
        first["warnings"][0]["code"],
        "nativeSearchIndexFreshnessUnverified"
    );
    let cursor = first["page"]["nextCursor"].as_str().unwrap();

    let second = run(
        &[
            "messages",
            "search",
            fixture.root.to_str().unwrap(),
            "--decrypted",
            "--query-stdin",
            "--limit",
            "1",
            "--cursor",
            cursor,
        ],
        Some(b"hello\n"),
    );
    assert_success(&second);
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_ne!(first["items"][0]["id"], second["items"][0]["id"]);

    let wrong_query = run(
        &[
            "messages",
            "search",
            fixture.root.to_str().unwrap(),
            "--decrypted",
            "--query-stdin",
            "--limit",
            "1",
            "--cursor",
            cursor,
        ],
        Some(b"different\n"),
    );
    assert!(!wrong_query.status.success());
    let error: Value = serde_json::from_slice(&wrong_query.stdout).unwrap();
    assert_eq!(error["error"]["code"], "invalidCursor");

    let filtered = run(
        &[
            "messages",
            "search",
            fixture.root.to_str().unwrap(),
            "--decrypted",
            "--query-stdin",
            "--conversation",
            "wxid_other",
        ],
        Some(b"hello\n"),
    );
    assert_success(&filtered);
    let filtered: Value = serde_json::from_slice(&filtered.stdout).unwrap();
    assert_eq!(filtered["page"]["returned"], 1);
    assert_eq!(filtered["items"][0]["conversationId"], "wxid_other");
}

#[test]
fn missing_native_fts_uses_bounded_source_fallback_without_writes() {
    let fixture = Fixture::new(false);
    fs::remove_file(fixture.root.join("message/message_fts.db")).unwrap();
    let files_before = fixture.relative_files();
    let search = run(
        &[
            "messages",
            "search",
            fixture.root.to_str().unwrap(),
            "--decrypted",
            "--query-stdin",
            "--conversation",
            "wxid_talker",
            "--limit",
            "1",
        ],
        Some(b"s0\n"),
    );
    assert_success(&search);
    let search: Value = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(
        search["consistency"]["guarantee"],
        "boundedDecodedSourceWindow"
    );
    assert_eq!(search["items"][0]["snippet"], "s0-new");
    assert_eq!(search["items"][0]["senderDisplayName"], "Sender Remark");
    assert_eq!(
        search["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|warning| warning["code"] == "fallbackSearchSourceWindowBounded")
            .unwrap()["count"],
        2
    );

    let exact = run(
        &[
            "message",
            "get",
            fixture.root.to_str().unwrap(),
            "--decrypted",
            "--conversation",
            "wxid_talker",
            "--message",
            search["items"][0]["id"].as_str().unwrap(),
        ],
        None,
    );
    assert_success(&exact);
    let exact: Value = serde_json::from_slice(&exact.stdout).unwrap();
    assert_eq!(exact["item"]["content"]["Text"], "s0-new");
    assert_eq!(fixture.relative_files(), files_before);
}

#[test]
#[ignore = "manual release-mode latency evidence; run with --ignored --nocapture --test-threads=1"]
fn fallback_search_latency_evidence_for_the_fixed_500_message_window() {
    let reports = [
        benchmark_fallback_search(false, 1, 512, 256, true, 20),
        benchmark_fallback_search(true, 1, 512, 256, true, 20),
        benchmark_fallback_search(true, 1, 512, 8 * 1024, true, 20),
        benchmark_fallback_search(true, 16, 40, 1024, false, 20),
    ];
    println!(
        "GREENBUBBLES_FALLBACK_SEARCH_BENCHMARK_V1\n{}",
        serde_json::to_string_pretty(&reports).unwrap()
    );
}

#[test]
fn encrypted_cli_reads_directly_and_wrong_key_fails_without_disclosure() {
    let fixture = Fixture::new(true);
    let key = format!("{}\n", hex::encode(RAW_KEY));
    let conversations = run(
        &[
            "conversations",
            "list",
            fixture.root.to_str().unwrap(),
            "--passphrase-stdin",
            "--limit",
            "1",
        ],
        Some(key.as_bytes()),
    );
    assert_success(&conversations);
    let conversations: Value = serde_json::from_slice(&conversations.stdout).unwrap();
    assert_eq!(conversations["source"]["mode"], "liveEncrypted");
    assert_eq!(conversations["page"]["returned"], 1);

    let output = run(
        &[
            "messages",
            "list",
            fixture.root.to_str().unwrap(),
            "--passphrase-stdin",
            "--conversation",
            "wxid_talker",
            "--limit",
            "1",
        ],
        Some(key.as_bytes()),
    );
    assert_success(&output);
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["source"]["mode"], "liveEncrypted");
    assert_eq!(response["page"]["returned"], 1);
    assert_eq!(response["items"][0]["content"]["Text"], "s1-new");
    let status = run(
        &[
            "source",
            "status",
            fixture.root.to_str().unwrap(),
            "--passphrase-stdin",
        ],
        Some(key.as_bytes()),
    );
    assert_success(&status);
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["source"]["mode"], "liveEncrypted");
    assert_eq!(status["databaseCount"], 5);
    let message_id = response["items"][0]["id"].as_str().unwrap();
    let exact = run(
        &[
            "message",
            "get",
            fixture.root.to_str().unwrap(),
            "--passphrase-stdin",
            "--conversation",
            "wxid_talker",
            "--message",
            message_id,
        ],
        Some(key.as_bytes()),
    );
    assert_success(&exact);
    let exact: Value = serde_json::from_slice(&exact.stdout).unwrap();
    assert_eq!(exact["operation"], "message.get");
    assert_eq!(exact["item"]["content"]["Text"], "s1-new");
    assert_eq!(fixture.relative_files(), fixture.original_files);

    let search_input = format!("{}\nhello\n", hex::encode(RAW_KEY));
    let search = run(
        &[
            "messages",
            "search",
            fixture.root.to_str().unwrap(),
            "--passphrase-stdin",
            "--query-stdin",
            "--limit",
            "1",
        ],
        Some(search_input.as_bytes()),
    );
    assert_success(&search);
    let search: Value = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(search["operation"], "messages.search");
    assert_eq!(search["page"]["returned"], 1);

    let wrong_key = format!("{}\n", hex::encode([0xCD; 32]));
    let failure = run(
        &[
            "conversations",
            "list",
            fixture.root.to_str().unwrap(),
            "--passphrase-stdin",
        ],
        Some(wrong_key.as_bytes()),
    );
    assert!(!failure.status.success());
    let stderr = String::from_utf8(failure.stderr).unwrap();
    let error: Value = serde_json::from_slice(&failure.stdout).unwrap();
    assert_eq!(error["schema"], "greenbubbles.query.v1");
    assert_eq!(error["operation"], "conversations.list");
    assert_eq!(error["ok"], false);
    assert_eq!(error["error"]["code"], "databaseUnavailable");
    assert!(stderr.contains("see the JSON error"));
    assert!(!stderr.contains(fixture.root.to_str().unwrap()));
    assert!(!stderr.contains(&hex::encode(RAW_KEY)));
    assert!(!stderr.contains(&hex::encode([0xCD; 32])));
}

#[test]
fn default_and_named_profiles_remove_repeated_source_arguments() {
    let fixture = Fixture::new(false);
    let profile_home = ProfileHome::new(serde_json::json!({
        "schema": "greenbubbles.query-profiles.v1",
        "formatVersion": 1,
        "defaultProfile": "plain",
        "profiles": {
            "plain": {
                "sourceRoot": fixture.root,
                "access": {"mode": "decrypted"}
            },
            "alternate": {
                "sourceRoot": fixture.root,
                "access": {"mode": "decrypted"}
            }
        }
    }));

    let conversations = run_with_home(
        profile_home.path(),
        &["conversations", "list", "--limit", "1"],
        None,
    );
    assert_success(&conversations);
    let conversations: Value = serde_json::from_slice(&conversations.stdout).unwrap();
    assert_eq!(conversations["source"]["mode"], "decrypted");
    assert_eq!(conversations["page"]["returned"], 1);

    let messages = run_with_home(
        profile_home.path(),
        &[
            "messages",
            "list",
            "--profile",
            "alternate",
            "--conversation",
            "wxid_talker",
            "--limit",
            "1",
        ],
        None,
    );
    assert_success(&messages);
    let messages: Value = serde_json::from_slice(&messages.stdout).unwrap();
    assert_eq!(messages["items"][0]["content"]["Text"], "s1-new");

    for (arguments, expected_operation) in [
        (vec!["profile", "list"], None),
        (vec!["profile", "show", "plain"], None),
        (vec!["profile", "validate"], None),
        (vec!["source", "status"], Some("source.status")),
    ] {
        let output = run_with_home(profile_home.path(), &arguments, None);
        assert_success(&output);
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        if let Some(expected_operation) = expected_operation {
            assert_eq!(response["operation"], expected_operation);
        }
        assert!(!String::from_utf8_lossy(&output.stdout).contains("message_content"));
    }

    let set_default = run_with_home(
        profile_home.path(),
        &["profile", "set-default", "alternate"],
        None,
    );
    assert_success(&set_default);
    let stored: Value =
        serde_json::from_slice(&fs::read(profile_home.config_path()).unwrap()).unwrap();
    assert_eq!(stored["defaultProfile"], "alternate");
    assert_eq!(
        fs::metadata(profile_home.config_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    for arguments in [
        vec![
            "conversations",
            "list",
            fixture.root.to_str().unwrap(),
            "--decrypted",
            "--profile",
            "plain",
        ],
        vec!["conversations", "list", "--decrypted"],
    ] {
        let failure = run_with_home(profile_home.path(), &arguments, None);
        assert!(!failure.status.success());
        let response: Value = serde_json::from_slice(&failure.stdout).unwrap();
        assert_eq!(response["error"]["code"], "invalidRequest");
    }
}

#[test]
fn live_key_profile_keeps_key_out_of_arguments_and_search_stdin() {
    let fixture = Fixture::new(true);
    let profile_home = ProfileHome::empty();
    let credential = profile_home.credential_path("wechat-key");
    write_private_file(
        &credential,
        format!("{}\n", hex::encode(RAW_KEY)).as_bytes(),
    );
    profile_home.write_configuration(serde_json::json!({
        "schema": "greenbubbles.query-profiles.v1",
        "formatVersion": 1,
        "defaultProfile": "live",
        "profiles": {
            "live": {
                "sourceRoot": fixture.root,
                "access": {
                    "mode": "liveWeChatKeyFile",
                    "credentialFile": credential
                }
            }
        }
    }));

    let conversations = run_with_home(
        profile_home.path(),
        &["conversations", "list", "--limit", "1"],
        None,
    );
    assert_success(&conversations);
    let conversations: Value = serde_json::from_slice(&conversations.stdout).unwrap();
    assert_eq!(conversations["source"]["mode"], "liveEncrypted");

    let search = run_with_home(
        profile_home.path(),
        &[
            "messages",
            "search",
            "--query-stdin",
            "--conversation",
            "wxid_talker",
            "--limit",
            "1",
        ],
        Some(b"hello\n"),
    );
    assert_success(&search);
    let search: Value = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(search["operation"], "messages.search");
    assert_eq!(search["page"]["returned"], 1);

    fs::set_permissions(&credential, fs::Permissions::from_mode(0o640)).unwrap();
    let failure = run_with_home(profile_home.path(), &["conversations", "list"], None);
    assert!(!failure.status.success());
    let response: Value = serde_json::from_slice(&failure.stdout).unwrap();
    assert_eq!(response["error"]["code"], "invalidProfile");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&failure.stdout),
        String::from_utf8_lossy(&failure.stderr)
    );
    assert!(!combined.contains(&hex::encode(RAW_KEY)));
    assert!(!combined.contains(fixture.root.to_str().unwrap()));
}

#[test]
fn snapshot_profiles_support_local_recovery_and_passphrase_credentials() {
    let fixture = Fixture::new(true);
    fs::set_permissions(fixture._directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let recovery_words = SnapshotRecoveryWords::generate().unwrap();
    let local_credential = SnapshotLocalCredential::generate().unwrap();
    let passphrase = SnapshotPassphrase::from_utf8(
        b"correct horse battery staple for snapshot profile".to_vec(),
    )
    .unwrap();
    let snapshot = fixture._directory.path().join("recoverable-snapshot");
    create_recoverable_snapshot_with_recovery_words_and_optional_protectors(
        &fixture.root,
        QueryDatabaseAccess::LiveEncrypted(&RAW_KEY),
        &snapshot,
        &recovery_words,
        Some(&local_credential),
        Some(&passphrase),
    )
    .unwrap();

    let profile_home = ProfileHome::empty();
    let recovery_file = profile_home.credential_path("recovery-kit");
    let local_file = profile_home.credential_path("local-credential");
    let passphrase_file = profile_home.credential_path("snapshot-passphrase");
    recovery_words.write_private_file(&recovery_file).unwrap();
    local_credential.write_private_file(&local_file).unwrap();
    write_private_file(
        &passphrase_file,
        b"correct horse battery staple for snapshot profile\n",
    );
    profile_home.write_configuration(serde_json::json!({
        "schema": "greenbubbles.query-profiles.v1",
        "formatVersion": 1,
        "defaultProfile": "archive-local",
        "profiles": {
            "archive-local": {
                "sourceRoot": snapshot,
                "access": {
                    "mode": "snapshotLocalCredential",
                    "credentialFile": local_file
                }
            },
            "archive-recovery": {
                "sourceRoot": snapshot,
                "access": {
                    "mode": "snapshotRecoveryKit",
                    "credentialFile": recovery_file
                }
            },
            "archive-passphrase": {
                "sourceRoot": snapshot,
                "access": {
                    "mode": "snapshotPassphraseFile",
                    "credentialFile": passphrase_file
                }
            }
        }
    }));

    for profile in [None, Some("archive-recovery"), Some("archive-passphrase")] {
        let mut arguments = vec!["conversations", "list"];
        if let Some(profile) = profile {
            arguments.extend(["--profile", profile]);
        }
        arguments.extend(["--limit", "1"]);
        let output = run_with_home(profile_home.path(), &arguments, None);
        assert_success(&output);
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["source"]["mode"], "snapshotEncrypted");
        assert_eq!(response["page"]["returned"], 1);
    }
}

#[test]
fn unbounded_and_ambiguous_access_options_fail_closed() {
    let fixture = Fixture::new(false);
    for arguments in [
        vec![
            "conversations",
            "list",
            fixture.root.to_str().unwrap(),
            "--decrypted",
            "--all",
        ],
        vec![
            "conversations",
            "list",
            fixture.root.to_str().unwrap(),
            "--decrypted",
            "--limit",
            "501",
        ],
        vec!["conversations", "list", fixture.root.to_str().unwrap()],
        vec![
            "conversations",
            "list",
            fixture.root.to_str().unwrap(),
            "--decrypted",
            "--passphrase-stdin",
        ],
    ] {
        let output = run(&arguments, None);
        assert!(!output.status.success());
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    original_files: Vec<String>,
}

impl Fixture {
    fn new(encrypted: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("db_storage");
        fs::create_dir_all(root.join("contact")).unwrap();
        fs::create_dir_all(root.join("session")).unwrap();
        fs::create_dir_all(root.join("message")).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();

        create_database(
            &root.join("contact/contact.db"),
            encrypted,
            "CREATE TABLE contact(
                username TEXT PRIMARY KEY,
                alias BLOB,
                remark BLOB,
                nick_name BLOB
             );
             INSERT INTO contact VALUES
                ('wxid_a', 'Alias A', 'Remark A', 'Nickname A'),
                ('wxid_b', 'Alias B', '', 'Nickname B'),
                ('wxid_c', 'Alias C', '', ''),
                ('wxid_d', '', '', ''),
                ('wxid_talker', '', 'Talker Remark', ''),
                ('wxid_sender', '', 'Sender Remark', ''),
                ('wxid_other', '', 'Other Conversation', '');",
        );
        create_database(
            &root.join("session/session.db"),
            encrypted,
            "CREATE TABLE SessionTable(
                username TEXT NOT NULL,
                sort_timestamp INTEGER NOT NULL,
                summary BLOB,
                last_msg_type INTEGER,
                last_msg_sender TEXT,
                last_sender_display_name TEXT
             );
             INSERT INTO SessionTable VALUES
                ('wxid_a', 30, 'a', 1, 'wxid_a', 'A'),
                ('wxid_b', 20, 'b', 1, 'wxid_b', 'B'),
                ('wxid_c', 20, 'c', 1, 'wxid_c', 'C'),
                ('wxid_d', 10, 'd', 1, 'wxid_d', 'D');",
        );

        let table = format!("Msg_{:x}", md5::compute(b"wxid_talker"));
        for (shard, values) in [
            (0, vec![(100, 1000, 0, "s0-new"), (90, 900, 7, "s0-old")]),
            (1, vec![(100, 1000, 0, "s1-new"), (80, 800, 8, "s1-old")]),
        ] {
            let path = root.join(format!("message/message_{shard}.db"));
            let connection = open_database_for_creation(&path, encrypted);
            connection
                .execute_batch(&format!(
                    "CREATE TABLE Name2Id(user_name TEXT);
                     INSERT INTO Name2Id(rowid, user_name) VALUES (1, 'wxid_sender');
                     CREATE TABLE [{table}](
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
            for (sort_sequence, create_time, server_id, body) in values {
                connection
                    .execute(
                        &format!(
                            "INSERT INTO [{table}](server_id, sort_seq, local_type, real_sender_id, \
                             create_time, status, message_content, WCDB_CT_message_content) \
                             VALUES (?1, ?2, 1, 1, ?3, 0, ?4, 0)"
                        ),
                        params![server_id, sort_sequence, create_time, body.as_bytes()],
                    )
                    .unwrap();
            }
        }

        let fts = open_database_for_creation(&root.join("message/message_fts.db"), encrypted);
        fts.execute_batch(
            "CREATE TABLE name2id(rowid INTEGER PRIMARY KEY, username TEXT NOT NULL);
             INSERT INTO name2id VALUES (1, 'wxid_talker');
             INSERT INTO name2id VALUES (2, 'wxid_sender');
             INSERT INTO name2id VALUES (3, 'wxid_other');
             CREATE VIRTUAL TABLE message_fts_v4_0 USING fts5(
                acontent, message_local_id UNINDEXED, sort_seq UNINDEXED,
                local_type UNINDEXED, session_id UNINDEXED, sender_id UNINDEXED,
                create_time UNINDEXED, tokenize='unicode61'
             );
             CREATE VIRTUAL TABLE message_fts_v4_1 USING fts5(
                acontent, message_local_id UNINDEXED, sort_seq UNINDEXED,
                local_type UNINDEXED, session_id UNINDEXED, sender_id UNINDEXED,
                create_time UNINDEXED, tokenize='unicode61'
             );
             INSERT INTO message_fts_v4_0 VALUES
                ('hello oldest', 10, 100, 1, 1, 2, 1000);
             INSERT INTO message_fts_v4_1 VALUES
                ('hello newest', 20, 200, 1, 1, 2, 2000);
             INSERT INTO message_fts_v4_1 VALUES
                ('hello elsewhere', 30, 150, 1, 3, 2, 1500);",
        )
        .unwrap();

        let mut fixture = Self {
            _directory: directory,
            root,
            original_files: Vec::new(),
        };
        fixture.original_files = fixture.relative_files();
        fixture
    }

    fn relative_files(&self) -> Vec<String> {
        let mut files = walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| {
                entry
                    .path()
                    .strip_prefix(&self.root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        files.sort();
        files
    }

    fn replace_with_benchmark_messages(
        &self,
        encrypted: bool,
        conversation_count: usize,
        messages_per_conversation: usize,
        payload_bytes: usize,
    ) {
        assert!((1..=16).contains(&conversation_count));
        assert!(messages_per_conversation > 0);
        assert!((64..=16 * 1024).contains(&payload_bytes));

        let session = open_database_for_creation(&self.root.join("session/session.db"), encrypted);
        session.execute("DELETE FROM SessionTable", []).unwrap();
        let contact = open_database_for_creation(&self.root.join("contact/contact.db"), encrypted);
        let mut messages =
            open_database_for_creation(&self.root.join("message/message_0.db"), encrypted);
        let transaction = messages.transaction().unwrap();
        for conversation_index in 0..conversation_count {
            let conversation = format!("benchmark_conversation_{conversation_index:02}");
            session
                .execute(
                    "INSERT INTO SessionTable(
                        username, sort_timestamp, summary, last_msg_type,
                        last_msg_sender, last_sender_display_name
                     ) VALUES (?1, ?2, 'benchmark', 1, 'wxid_sender', 'Sender')",
                    params![&conversation, conversation_index as i64],
                )
                .unwrap();
            contact
                .execute(
                    "INSERT OR REPLACE INTO contact(username, alias, remark, nick_name)
                     VALUES (?1, '', ?2, '')",
                    params![
                        &conversation,
                        format!("Conversation {conversation_index:02}")
                    ],
                )
                .unwrap();
            let table = format!("Msg_{:x}", md5::compute(conversation.as_bytes()));
            transaction
                .execute_batch(&format!(
                    "CREATE TABLE [{table}](
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
            for message_index in 0..messages_per_conversation {
                let prefix = format!(
                    "ordinary benchmark text conversation {conversation_index:02} message {message_index:04} "
                );
                let mut body = prefix.into_bytes();
                body.resize(payload_bytes, b'x');
                transaction
                    .execute(
                        &format!(
                            "INSERT INTO [{table}](
                                server_id, sort_seq, local_type, real_sender_id,
                                create_time, status, message_content,
                                WCDB_CT_message_content
                             ) VALUES (?1, ?2, 1, 1, ?3, 0, ?4, 0)"
                        ),
                        params![
                            (conversation_index * messages_per_conversation + message_index + 1)
                                as i64,
                            (messages_per_conversation - message_index) as i64,
                            1_700_000_000_i64 - message_index as i64,
                            body,
                        ],
                    )
                    .unwrap();
            }
        }
        transaction.commit().unwrap();
    }
}

fn benchmark_fallback_search(
    encrypted: bool,
    conversation_count: usize,
    messages_per_conversation: usize,
    payload_bytes: usize,
    scoped: bool,
    sample_count: usize,
) -> Value {
    let fixture = Fixture::new(encrypted);
    fixture.replace_with_benchmark_messages(
        encrypted,
        conversation_count,
        messages_per_conversation,
        payload_bytes,
    );
    fs::remove_file(fixture.root.join("message/message_fts.db")).unwrap();
    let files_before = fixture.relative_files();
    let root = fixture.root.to_str().unwrap();
    let mut arguments = vec!["messages", "search", root];
    if encrypted {
        arguments.push("--passphrase-stdin");
    } else {
        arguments.push("--decrypted");
    }
    arguments.extend(["--query-stdin", "--limit", "50"]);
    if scoped {
        arguments.extend(["--conversation", "benchmark_conversation_00"]);
    }
    let query = "needle-that-is-intentionally-absent-from-every-row";
    let input = if encrypted {
        format!("{}\n{query}\n", hex::encode(RAW_KEY))
    } else {
        format!("{query}\n")
    };

    for _ in 0..3 {
        assert_fallback_benchmark_response(&run(&arguments, Some(input.as_bytes())));
    }
    let mut microseconds = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        let started = Instant::now();
        let output = run(&arguments, Some(input.as_bytes()));
        let elapsed = started.elapsed();
        assert_fallback_benchmark_response(&output);
        microseconds.push(elapsed.as_micros() as u64);
    }
    assert_eq!(fixture.relative_files(), files_before);
    microseconds.sort_unstable();
    let p50 = percentile_microseconds(&microseconds, 50);
    let p95 = percentile_microseconds(&microseconds, 95);
    let maximum = *microseconds.last().unwrap();
    serde_json::json!({
        "sourceMode": if encrypted { "liveEncrypted" } else { "decrypted" },
        "scope": if scoped { "singleConversation" } else { "sixteenConversations" },
        "conversationCount": conversation_count,
        "messagesPerConversation": messages_per_conversation,
        "payloadBytesPerMessage": payload_bytes,
        "fixedScannedMessageBound": 500,
        "sampleCount": sample_count,
        "warmupCount": 3,
        "p50Milliseconds": p50 as f64 / 1_000.0,
        "p95Milliseconds": p95 as f64 / 1_000.0,
        "maximumMilliseconds": maximum as f64 / 1_000.0,
        "persistentWritesObserved": false,
    })
}

fn assert_fallback_benchmark_response(output: &Output) {
    assert_success(output);
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        response["consistency"]["guarantee"],
        "boundedDecodedSourceWindow"
    );
    assert_eq!(response["page"]["returned"], 0);
    assert_eq!(response["page"]["hasMore"], true);
    assert_eq!(
        response["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|warning| warning["code"] == "fallbackSearchSourceWindowBounded")
            .unwrap()["count"],
        500
    );
}

fn percentile_microseconds(sorted: &[u64], percentile: usize) -> u64 {
    let rank = (sorted.len() * percentile).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn create_database(path: &Path, encrypted: bool, sql: &str) {
    let connection = open_database_for_creation(path, encrypted);
    connection.execute_batch(sql).unwrap();
}

fn open_database_for_creation(path: &Path, encrypted: bool) -> Connection {
    let connection = Connection::open(path).unwrap();
    if encrypted {
        unsafe {
            let result = rusqlite::ffi::sqlite3_key(
                connection.handle(),
                RAW_KEY.as_ptr() as *const c_void,
                RAW_KEY.len() as i32,
            );
            assert_eq!(result, 0);
        }
    }
    connection
}

struct ProfileHome {
    directory: tempfile::TempDir,
    configuration_directory: PathBuf,
    credential_directory: PathBuf,
}

impl ProfileHome {
    fn empty() -> Self {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let configuration_directory = directory.path().join(".greenbubbles");
        let credential_directory = configuration_directory.join("credentials");
        fs::create_dir_all(&credential_directory).unwrap();
        fs::set_permissions(&configuration_directory, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&credential_directory, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            directory,
            configuration_directory,
            credential_directory,
        }
    }

    fn new(configuration: Value) -> Self {
        let home = Self::empty();
        home.write_configuration(configuration);
        home
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn config_path(&self) -> PathBuf {
        self.configuration_directory.join("query-profiles.json")
    }

    fn credential_path(&self, name: &str) -> PathBuf {
        self.credential_directory.join(name)
    }

    fn write_configuration(&self, configuration: Value) {
        write_private_file(
            &self.config_path(),
            &serde_json::to_vec_pretty(&configuration).unwrap(),
        );
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn run(arguments: &[&str], input: Option<&[u8]>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_greenbubbles"));
    command
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if input.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn run_with_home(home: &Path, arguments: &[&str], input: Option<&[u8]>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_greenbubbles"));
    command
        .args(arguments)
        .env("HOME", home)
        .env_remove("GREENBUBBLES_QUERY_PROFILES_FILE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if input.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    child.wait_with_output().unwrap()
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
