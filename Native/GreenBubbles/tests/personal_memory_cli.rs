use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use greenbubbles::live_query::{LiveQuerySource, QueryDatabaseAccess};
use greenbubbles::personal_memory::{
    acknowledge_personal_memory_page, commit_personal_memory_batch,
    commit_personal_memory_batch_reviewed_no_durable_memory, next_personal_memory_batch,
    next_personal_memory_page, personal_memory_status, prepare_personal_memory_corpus,
    PERSONAL_MEMORY_CURRENT_SELECTOR,
};
use rusqlite::{params, Connection};
use serde_json::Value;

#[test]
fn corpus_selection_is_owner_active_and_batch_state_is_crash_safe() {
    let fixture = Fixture::new();
    let policy_path = write_policy(&fixture);

    let source = LiveQuerySource::open(&fixture.root, QueryDatabaseAccess::Decrypted).unwrap();
    let corpus = fixture.directory.path().join("corpus");
    let manifest = prepare_personal_memory_corpus(&source, &policy_path, &corpus).unwrap();
    assert_eq!(manifest.scanned_message_count, 9);
    assert_eq!(manifest.selected_message_count, 6);
    assert_eq!(manifest.evidence_count, 6);
    assert!(manifest.account_holder_attribution_bound);
    assert_eq!(manifest.unmatched_message_table_count, 0);
    assert_eq!(
        serde_json::to_value(manifest.delivery_order).unwrap(),
        "accountHolderRelevance"
    );

    let evidence = fs::read_to_string(corpus.join("evidence.jsonl")).unwrap();
    assert!(evidence.contains("direct self anchor"));
    assert!(evidence.contains("group self anchor"));
    assert!(!evidence.contains("silent group traffic"));
    assert!(!evidence.contains("inactive direct month"));
    assert!(!evidence.contains("inactive group month"));

    let wiki = fixture.directory.path().join("wiki");
    fs::create_dir(&wiki).unwrap();
    fs::set_permissions(&wiki, fs::Permissions::from_mode(0o700)).unwrap();
    let state = fixture.directory.path().join("run-state.json");
    let first = next_personal_memory_batch(&corpus, &state, Some(&wiki), 64 * 1024).unwrap();
    assert_eq!(first["complete"], false);
    assert_eq!(first["deliveryOrder"], "accountHolderRelevance");
    assert_eq!(first["position"]["messageCount"], 6);
    assert_eq!(first["delivery"]["pageCount"], 1);
    assert!(first.get("episodes").is_none());
    let batch_id = first["batchId"].as_str().unwrap().to_string();
    let page = next_personal_memory_page(&corpus, &state, &batch_id).unwrap();
    let repeated_page =
        next_personal_memory_page(&corpus, &state, PERSONAL_MEMORY_CURRENT_SELECTOR).unwrap();
    assert_eq!(repeated_page, page);
    assert_eq!(page["page"]["messageCount"], 6);
    let serialized = serde_json::to_string(&page).unwrap();
    for verbose_field in ["canonicalId", "conversationId", "senderId", "contentSHA256"] {
        assert!(!serialized.contains(verbose_field));
    }
    for raw_source_id in ["wxid_friend", "wxid_group_friend", "room@chatroom"] {
        assert!(
            !serialized.contains(raw_source_id),
            "model-facing batch leaked raw source ID {raw_source_id}"
        );
    }
    assert!(serialized.contains("Direct conversation C"));
    assert!(serialized.contains("Person P"));
    assert!(serialized.contains("\"a\":\"self\""));

    assert!(commit_personal_memory_batch(&corpus, &state, &batch_id, &wiki).is_err());
    let after_empty_rejection = personal_memory_status(&corpus, Some(&state)).unwrap();
    assert_eq!(
        after_empty_rejection.outstanding_batch_id.as_deref(),
        Some(batch_id.as_str())
    );

    write_private(&wiki.join("people/P000001.md"), b"");
    assert!(commit_personal_memory_batch(&corpus, &state, &batch_id, &wiki).is_err());
    fs::remove_file(wiki.join("people/P000001.md")).unwrap();

    let self_alias = page["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|episode| episode["m"].as_array().unwrap())
        .find(|message| message["a"] == "self")
        .unwrap()["e"]
        .as_str()
        .unwrap()
        .to_string();
    let page_token = page["pageToken"].as_str().unwrap();
    acknowledge_personal_memory_page(
        &corpus,
        &state,
        &batch_id,
        page_token,
        std::slice::from_ref(&self_alias),
        false,
    )
    .unwrap();
    let reviewed = next_personal_memory_batch(&corpus, &state, Some(&wiki), 128 * 1024).unwrap();
    assert_eq!(reviewed["batchId"], batch_id);
    assert_eq!(reviewed["delivery"]["reviewComplete"], true);

    write_private(
        &wiki.join("me.md"),
        b"# Me\n\n- This invalid claim has an unknown source. [E999999999]\n",
    );
    assert!(commit_personal_memory_batch(&corpus, &state, &batch_id, &wiki).is_err());
    let after_rejection = personal_memory_status(&corpus, Some(&state)).unwrap();
    assert_eq!(
        after_rejection.outstanding_batch_id.as_deref(),
        Some(batch_id.as_str())
    );

    write_private(
        &wiki.join("me.md"),
        format!("# Me\n\n- I participated in this conversation. [{self_alias}]\n").as_bytes(),
    );
    let committed = commit_personal_memory_batch(&corpus, &state, &batch_id, &wiki).unwrap();
    assert!(committed.committed);
    assert!(!committed.already_committed);
    assert!(committed.complete);
    assert_eq!(committed.changed_pages, vec!["me.md"]);

    let repeated_commit = commit_personal_memory_batch(&corpus, &state, &batch_id, &wiki).unwrap();
    assert!(repeated_commit.already_committed);

    let committed_me = fs::read(wiki.join("me.md")).unwrap();
    write_private(
        &wiki.join("me.md"),
        b"# Me\n\n- This edit bypassed the batch protocol.\n",
    );
    assert!(next_personal_memory_batch(&corpus, &state, Some(&wiki), 64 * 1024).is_err());
    write_private(&wiki.join("me.md"), &committed_me);

    let complete = next_personal_memory_batch(&corpus, &state, Some(&wiki), 64 * 1024).unwrap();
    assert_eq!(complete["complete"], true);
    let status = personal_memory_status(&corpus, Some(&state)).unwrap();
    assert!(status.complete);
    assert_eq!(status.progress_percent, 100.0);
    assert_eq!(status.scanned_message_count, 9);
    assert_eq!(status.selected_message_count, 6);
    assert_eq!(status.committed_message_count, 6);
    assert!(status.source_coverage_complete);
    assert!(status.content_complete);
    assert_eq!(status.unmatched_message_table_count, 0);
    assert!(status.limitation_codes.is_empty());
    assert_eq!(status.review_complete, None);
    let last_committed = status.last_committed.unwrap();
    assert_eq!(last_committed.batch_id, batch_id);
    assert_eq!(last_committed.reviewed_page_count, 1);
    assert_eq!(last_committed.reviewed_message_count, 6);
    assert_eq!(last_committed.retained_evidence_count, 1);
}

#[test]
fn memory_cli_drives_the_prepare_next_commit_and_status_contract() {
    let fixture = Fixture::new();
    let policy = write_policy(&fixture);
    let corpus = fixture.directory.path().join("cli-corpus");
    let prepared = run(&[
        "memory",
        "prepare",
        fixture.root.to_str().unwrap(),
        corpus.to_str().unwrap(),
        "--selection-policy",
        policy.to_str().unwrap(),
        "--decrypted",
    ]);
    assert!(
        prepared.status.success(),
        "prepare failed; stderr: {}; stdout: {}",
        String::from_utf8_lossy(&prepared.stderr),
        String::from_utf8_lossy(&prepared.stdout)
    );
    let progress = String::from_utf8(prepared.stderr).unwrap();
    assert!(progress.contains("memory prepare: metadataSelection"));
    assert!(progress.contains("memory prepare: complete"));
    assert!(!progress.contains("direct self anchor"));
    let manifest: Value = serde_json::from_slice(&prepared.stdout).unwrap();
    assert_eq!(manifest["schema"], "greenbubbles.personal-memory-corpus.v1");
    assert_eq!(manifest["selectedMessageCount"], 6);
    assert_eq!(manifest["deliveryOrder"], "accountHolderRelevance");

    let wiki = fixture.directory.path().join("cli-wiki");
    fs::create_dir(&wiki).unwrap();
    fs::set_permissions(&wiki, fs::Permissions::from_mode(0o700)).unwrap();
    let state = fixture.directory.path().join("cli-state.json");
    let next = run(&[
        "memory",
        "next",
        corpus.to_str().unwrap(),
        "--state",
        state.to_str().unwrap(),
        "--wiki",
        wiki.to_str().unwrap(),
        "--max-text-bytes",
        "65536",
    ]);
    assert_success(&next);
    let batch: Value = serde_json::from_slice(&next.stdout).unwrap();
    let batch_id = batch["batchId"].as_str().unwrap().to_string();
    assert!(batch.get("episodes").is_none());
    let page = run(&[
        "memory",
        "page",
        corpus.to_str().unwrap(),
        "--state",
        state.to_str().unwrap(),
    ]);
    assert_success(&page);
    assert!(page.stdout.len() <= 48 * 1024);
    let repeated_page = run(&[
        "memory",
        "page",
        corpus.to_str().unwrap(),
        "--state",
        state.to_str().unwrap(),
    ]);
    assert_success(&repeated_page);
    assert_eq!(repeated_page.stdout, page.stdout);
    let page: Value = serde_json::from_slice(&page.stdout).unwrap();
    let page_token = page["pageToken"].as_str().unwrap().to_string();
    let evidence = page["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|episode| episode["m"].as_array().unwrap())
        .find(|message| message["a"] == "self")
        .unwrap()["e"]
        .as_str()
        .unwrap()
        .to_string();

    let empty_commit = run(&[
        "memory",
        "commit",
        corpus.to_str().unwrap(),
        "--state",
        state.to_str().unwrap(),
        "--wiki",
        wiki.to_str().unwrap(),
    ]);
    assert!(!empty_commit.status.success());

    let acknowledge = run(&[
        "memory",
        "acknowledge",
        corpus.to_str().unwrap(),
        "--state",
        state.to_str().unwrap(),
        "--retain-evidence",
        &evidence,
    ]);
    assert_success(&acknowledge);
    let acknowledgement: Value = serde_json::from_slice(&acknowledge.stdout).unwrap();
    assert_eq!(acknowledgement["reviewComplete"], true);
    assert_eq!(acknowledgement["batchId"], batch_id);
    assert_eq!(acknowledgement["pageToken"], page_token);

    write_private(
        &wiki.join("me.md"),
        format!("# Me\n\n- Participated in a chat. [{evidence}]\n").as_bytes(),
    );

    let commit = run(&[
        "memory",
        "commit",
        corpus.to_str().unwrap(),
        "--state",
        state.to_str().unwrap(),
        "--wiki",
        wiki.to_str().unwrap(),
    ]);
    assert_success(&commit);
    let commit: Value = serde_json::from_slice(&commit.stdout).unwrap();
    assert_eq!(commit["committed"], true);

    let status = run(&[
        "memory",
        "status",
        corpus.to_str().unwrap(),
        "--state",
        state.to_str().unwrap(),
    ]);
    assert_success(&status);
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["complete"], true);
    assert_eq!(status["progressPercent"], 100.0);
    assert_eq!(status["scannedMessageCount"], 9);
    assert_eq!(status["selectedMessageCount"], 6);
    assert_eq!(status["committedMessageCount"], 6);
    assert_eq!(status["sourceCoverageComplete"], true);
    assert_eq!(status["contentComplete"], true);
    assert_eq!(status["unmatchedMessageTableCount"], 0);
    assert_eq!(status["limitationCodes"], serde_json::json!([]));
    assert_eq!(status["deliveryOrder"], "accountHolderRelevance");
    assert_eq!(status["reviewComplete"], Value::Null);
    assert_eq!(status["lastCommitted"]["reviewedPageCount"], 1);
    assert_eq!(status["lastCommitted"]["reviewedMessageCount"], 6);
    assert_eq!(status["lastCommitted"]["retainedEvidenceCount"], 1);
}

#[test]
fn reviewed_no_durable_memory_is_explicit_and_requires_an_unchanged_wiki() {
    let fixture = Fixture::new();
    let policy = write_policy(&fixture);
    let source = LiveQuerySource::open(&fixture.root, QueryDatabaseAccess::Decrypted).unwrap();
    let corpus = fixture.directory.path().join("no-memory-corpus");
    prepare_personal_memory_corpus(&source, &policy, &corpus).unwrap();

    let wiki = fixture.directory.path().join("no-memory-wiki");
    fs::create_dir(&wiki).unwrap();
    fs::set_permissions(&wiki, fs::Permissions::from_mode(0o700)).unwrap();
    let state = fixture.directory.path().join("no-memory-state.json");
    let batch = next_personal_memory_batch(&corpus, &state, Some(&wiki), 64 * 1024).unwrap();
    let batch_id = batch["batchId"].as_str().unwrap();
    next_personal_memory_page(&corpus, &state, batch_id).unwrap();
    acknowledge_personal_memory_page(
        &corpus,
        &state,
        PERSONAL_MEMORY_CURRENT_SELECTOR,
        PERSONAL_MEMORY_CURRENT_SELECTOR,
        &[],
        true,
    )
    .unwrap();

    write_private(&wiki.join("me.md"), b"# Me\n");
    assert!(commit_personal_memory_batch_reviewed_no_durable_memory(
        &corpus, &state, batch_id, &wiki
    )
    .is_err());
    fs::remove_file(wiki.join("me.md")).unwrap();

    let committed = run(&[
        "memory",
        "commit",
        corpus.to_str().unwrap(),
        "--state",
        state.to_str().unwrap(),
        "--wiki",
        wiki.to_str().unwrap(),
        "--reviewed-no-durable-memory",
    ]);
    assert_success(&committed);
    let committed: Value = serde_json::from_slice(&committed.stdout).unwrap();
    assert_eq!(committed["committed"], true);
    assert_eq!(committed["disposition"], "reviewedNoDurableMemory");
    assert_eq!(committed["changedPages"], serde_json::json!([]));
    assert_eq!(committed["complete"], true);

    let repeated = commit_personal_memory_batch_reviewed_no_durable_memory(
        &corpus,
        &state,
        PERSONAL_MEMORY_CURRENT_SELECTOR,
        &wiki,
    )
    .unwrap();
    assert!(repeated.already_committed);
    assert_eq!(
        serde_json::to_value(repeated.disposition).unwrap(),
        "reviewedNoDurableMemory"
    );
}

struct Fixture {
    directory: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let account = directory.path().join("wxid_self_abcd");
        let root = account.join("db_storage");
        for relative in ["contact", "session", "message"] {
            fs::create_dir_all(root.join(relative)).unwrap();
        }
        fs::set_permissions(&account, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();

        let contact = Connection::open(root.join("contact/contact.db")).unwrap();
        contact
            .execute_batch(
                "CREATE TABLE contact(
                    username TEXT PRIMARY KEY,
                    alias BLOB,
                    remark BLOB,
                    nick_name BLOB
                 );
                 INSERT INTO contact VALUES
                    ('wxid_self', '', '', 'Self'),
                    ('wxid_friend', '', '', ''),
                    ('room@chatroom', '', 'Project Room', '');
                 CREATE TABLE chat_room(username TEXT PRIMARY KEY, owner TEXT, ext_buffer BLOB);
                 INSERT INTO chat_room VALUES ('room@chatroom', 'wxid_self', NULL);",
            )
            .unwrap();
        let session = Connection::open(root.join("session/session.db")).unwrap();
        session
            .execute_batch(
                "CREATE TABLE SessionTable(
                    username TEXT NOT NULL,
                    sort_timestamp INTEGER NOT NULL,
                    summary BLOB
                 );
                 INSERT INTO SessionTable VALUES
                    ('wxid_friend', 1706745600, 'direct'),
                    ('room@chatroom', 1682899200, 'group');",
            )
            .unwrap();

        let connection = Connection::open(root.join("message/message_0.db")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE Name2Id(user_name TEXT);
                 INSERT INTO Name2Id(rowid, user_name) VALUES
                    (1, 'wxid_self'),
                    (2, 'wxid_friend'),
                    (3, 'wxid_group_friend');",
            )
            .unwrap();
        create_message_table(&connection, "wxid_friend");
        create_message_table(&connection, "room@chatroom");
        insert_message(
            &connection,
            "wxid_friend",
            1,
            2,
            1_704_067_200,
            "direct before",
        );
        insert_message(
            &connection,
            "wxid_friend",
            2,
            1,
            1_704_067_260,
            "direct self anchor",
        );
        insert_message(
            &connection,
            "wxid_friend",
            3,
            2,
            1_704_067_320,
            "direct after",
        );
        insert_message(
            &connection,
            "wxid_friend",
            4,
            2,
            1_706_745_600,
            "inactive direct month",
        );
        insert_message(
            &connection,
            "room@chatroom",
            1,
            3,
            1_680_307_200,
            "group before",
        );
        insert_message(
            &connection,
            "room@chatroom",
            2,
            1,
            1_680_307_260,
            "group self anchor",
        );
        insert_message(
            &connection,
            "room@chatroom",
            3,
            3,
            1_680_307_320,
            "group after",
        );
        insert_message(
            &connection,
            "room@chatroom",
            4,
            3,
            1_680_350_400,
            "silent group traffic",
        );
        insert_message(
            &connection,
            "room@chatroom",
            5,
            3,
            1_682_899_200,
            "inactive group month",
        );
        Self { directory, root }
    }
}

fn create_message_table(connection: &Connection, conversation: &str) {
    let table = format!("Msg_{:x}", md5::compute(conversation.as_bytes()));
    connection
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
}

fn insert_message(
    connection: &Connection,
    conversation: &str,
    sequence: i64,
    sender_row_id: i64,
    timestamp: i64,
    text: &str,
) {
    let table = format!("Msg_{:x}", md5::compute(conversation.as_bytes()));
    connection
        .execute(
            &format!(
                "INSERT INTO [{table}](
                    server_id, sort_seq, local_type, real_sender_id, create_time,
                    status, message_content, WCDB_CT_message_content
                 ) VALUES (?1, ?1, 1, ?2, ?3, 0, ?4, 0)"
            ),
            params![sequence, sender_row_id, timestamp, text.as_bytes()],
        )
        .unwrap();
}

fn write_private(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).unwrap();
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn write_policy(fixture: &Fixture) -> PathBuf {
    let policy_path = fixture.directory.path().join("selection-policy.json");
    fs::write(
        &policy_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "greenbubbles.personal-memory-selection-policy.v1",
            "formatVersion": 1,
            "timezone": "UTC",
            "minimumSelfMessagesPerActiveMonth": 1,
            "recentLookbackMonths": 12,
            "minimumSelfActiveMonthsInLookback": 0,
            "directSessionGapMinutes": 720,
            "groupSessionGapMinutes": 60,
            "directContextBefore": 24,
            "directContextAfter": 24,
            "groupContextBefore": 12,
            "groupContextAfter": 16,
            "maximumMessageTextBytes": 4096,
            "maximumUnitMessages": 160,
            "maximumUnitTextBytes": 16384,
            "includeDirectConversations": true,
            "includeGroupConversations": true,
            "includeOfficialAccounts": false,
            "includeServiceAccounts": false
        }))
        .unwrap(),
    )
    .unwrap();
    policy_path
}

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_greenbubbles"))
        .args(arguments)
        .output()
        .unwrap()
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
