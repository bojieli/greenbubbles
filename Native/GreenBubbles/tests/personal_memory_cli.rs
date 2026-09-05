use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use greenbubbles::live_query::{LiveQuerySource, QueryDatabaseAccess};
use greenbubbles::personal_memory::{
    acknowledge_personal_memory_page, commit_personal_memory_batch,
    commit_personal_memory_batch_reviewed_no_durable_memory, next_personal_memory_batch,
    next_personal_memory_batch_with_scope, next_personal_memory_page, personal_memory_status,
    prepare_personal_memory_corpus, PersonalMemoryConversationKindSelector,
    PersonalMemoryScopeOptions, PersonalMemorySummarySubjectSelector,
    PERSONAL_MEMORY_CURRENT_SELECTOR,
};
use rusqlite::{params, Connection};
use serde_json::Value;

/// A published corpus is immutable evidence: its root is traversal-only and
/// every file below it is read-only. The root is sealed after the publishing
/// rename rather than before, because Darwin will not rename a directory its
/// owner cannot write — so this pins the end state that resequencing must
/// preserve, not the order it is reached in.
#[test]
fn published_corpus_is_finalized_read_only() {
    let fixture = Fixture::new();
    let policy_path = write_policy(&fixture);
    let source = LiveQuerySource::open(&fixture.root, QueryDatabaseAccess::Decrypted).unwrap();
    let corpus = fixture.directory.path().join("corpus");
    prepare_personal_memory_corpus(&source, &policy_path, &corpus).unwrap();

    let root_mode = fs::metadata(&corpus).unwrap().permissions().mode() & 0o777;
    assert_eq!(root_mode, 0o500, "corpus root must be traversal-only");

    let mut checked_files = 0usize;
    let mut checked_directories = 0usize;
    for entry in walkdir::WalkDir::new(&corpus).min_depth(1) {
        let entry = entry.unwrap();
        let mode = entry.metadata().unwrap().permissions().mode() & 0o777;
        if entry.file_type().is_dir() {
            assert_eq!(
                mode,
                0o500,
                "{} must be traversal-only",
                entry.path().display()
            );
            checked_directories += 1;
        } else {
            assert_eq!(mode, 0o400, "{} must be read-only", entry.path().display());
            checked_files += 1;
        }
    }
    assert!(checked_files > 0, "corpus published no files");
    assert!(
        checked_directories > 0,
        "corpus published no subdirectories"
    );
}

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
            serialized.contains(raw_source_id),
            "personal-memory page lost source identity {raw_source_id}"
        );
    }
    assert_eq!(page["accountHolder"]["sourceId"], "wxid_self");
    assert_eq!(page["accountHolder"]["displayName"], "Self");
    assert!(page["people"]
        .as_object()
        .unwrap()
        .values()
        .all(|identity| identity["sourceId"].is_string() && identity["displayName"].is_string()));
    assert!(page["people"]
        .as_object()
        .unwrap()
        .values()
        .any(|identity| identity["sourceId"] == "wxid_friend"
            && identity["displayName"] == "Friend Remark"
            && identity["remark"] == "Friend Remark"
            && identity["nickname"] == "Friend Nickname"
            && identity["wechatAlias"] == "friend_alias"));
    assert!(page["conversations"]
        .as_object()
        .unwrap()
        .values()
        .any(|identity| identity["sourceId"] == "room@chatroom"
            && identity["title"] == "Project Room"
            && identity["kind"] == "group"));
    assert!(page_messages(&page).iter().all(|message| {
        message["t"]
            .as_str()
            .is_some_and(|timestamp| timestamp.ends_with("+00:00"))
    }));
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
    assert!(manifest["generatedAt"]
        .as_str()
        .is_some_and(|value| value.ends_with("+00:00")));
    assert!(manifest["referenceTime"]
        .as_str()
        .is_some_and(|value| value.ends_with("+00:00")));
    assert!(manifest.get("generatedAtUnixMilliseconds").is_none());
    assert!(manifest.get("referenceUnix").is_none());

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

#[test]
fn canonical_corpus_scopes_compose_conversations_time_senders_and_subjects() {
    let fixture = Fixture::new();
    let policy = write_canonical_policy(&fixture);
    let source = LiveQuerySource::open(&fixture.root, QueryDatabaseAccess::Decrypted).unwrap();
    let corpus = fixture.directory.path().join("canonical-corpus");
    let manifest = prepare_personal_memory_corpus(&source, &policy, &corpus).unwrap();
    assert_eq!(manifest.scanned_message_count, 9);
    assert_eq!(manifest.selected_message_count, 9);
    assert_eq!(manifest.evidence_count, 9);
    assert_eq!(
        serde_json::to_value(manifest.corpus_mode).unwrap(),
        "allMessages"
    );
    let unit_index: Value =
        serde_json::from_slice(&fs::read(corpus.join("batches/index.json")).unwrap()).unwrap();
    assert_eq!(
        unit_index["schema"],
        "greenbubbles.personal-memory-unit-index.v2"
    );
    assert_eq!(unit_index["formatVersion"], 2);
    let first_indexed_unit = &unit_index["units"][0];
    assert!(first_indexed_unit.get("firstEvidenceOrdinal").is_some());
    assert!(first_indexed_unit.get("evidenceAliases").is_none());
    assert!(first_indexed_unit.get("targetPages").is_none());
    assert!(first_indexed_unit.get("conversationId").is_none());

    let wiki = fixture.directory.path().join("canonical-wiki");
    fs::create_dir(&wiki).unwrap();
    fs::set_permissions(&wiki, fs::Permissions::from_mode(0o700)).unwrap();

    let all_state = fixture.directory.path().join("all-state.json");
    let all =
        next_personal_memory_batch_with_scope(&corpus, &all_state, Some(&wiki), 128 * 1024, None)
            .unwrap();
    assert_eq!(all["position"]["messageCount"], 9);
    assert_eq!(all["scope"]["allMessages"], true);
    assert_eq!(all["scope"]["summarySubject"]["kind"], "accountHolder");
    let all_page = next_personal_memory_page(&corpus, &all_state, "current").unwrap();
    assert_eq!(page_message_count(&all_page), 9);
    assert!(all_page["targetPages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "me.md"));
    let all_state_json: Value = serde_json::from_slice(&fs::read(&all_state).unwrap()).unwrap();
    assert_eq!(all_state_json["scopedUnits"], serde_json::json!([]));
    assert_eq!(all_state_json["scopedMessageCount"], 9);

    let direct_scope = write_scope(
        &fixture,
        "direct-scope.json",
        serde_json::json!({
            "conversationSelectors": ["wxid_friend"]
        }),
    );
    let direct_state = fixture.directory.path().join("direct-state.json");
    let direct = next_personal_memory_batch_with_scope(
        &corpus,
        &direct_state,
        Some(&wiki),
        128 * 1024,
        Some(&direct_scope),
    )
    .unwrap();
    assert_eq!(direct["position"]["messageCount"], 4);
    assert_eq!(direct["scope"]["conversationFilterCount"], 1);

    let multiple_scope = write_scope(
        &fixture,
        "multiple-scope.json",
        serde_json::json!({
            "conversationSelectors": ["wxid_friend", "room@chatroom"]
        }),
    );
    let multiple_state = fixture.directory.path().join("multiple-state.json");
    let multiple = next_personal_memory_batch_with_scope(
        &corpus,
        &multiple_state,
        Some(&wiki),
        128 * 1024,
        Some(&multiple_scope),
    )
    .unwrap();
    assert_eq!(multiple["position"]["messageCount"], 9);
    assert_eq!(multiple["scope"]["conversationFilterCount"], 2);

    let time_scope = write_scope(
        &fixture,
        "time-scope.json",
        serde_json::json!({
            "from": "2024-01-01T00:00:00Z",
            "through": "2024-01-01T00:02:00Z"
        }),
    );
    let time_state = fixture.directory.path().join("time-state.json");
    let time = next_personal_memory_batch_with_scope(
        &corpus,
        &time_state,
        Some(&wiki),
        128 * 1024,
        Some(&time_scope),
    )
    .unwrap();
    assert_eq!(time["position"]["messageCount"], 3);
    assert_eq!(time["scope"]["from"], "2024-01-01T00:00:00+00:00");
    assert_eq!(time["scope"]["through"], "2024-01-01T00:02:00+00:00");

    let fractional_scope = write_scope(
        &fixture,
        "fractional-time-scope",
        serde_json::json!({
            "from": "2024-01-01T08:00:00.500+08:00",
            "through": "2024-01-01T08:02:00.500+08:00"
        }),
    );
    let fractional_state = fixture.directory.path().join("fractional-time-state.json");
    let fractional = next_personal_memory_batch_with_scope(
        &corpus,
        &fractional_state,
        Some(&wiki),
        128 * 1024,
        Some(&fractional_scope),
    )
    .unwrap();
    assert_eq!(fractional["position"]["messageCount"], 2);
    assert_eq!(fractional["scope"]["from"], "2024-01-01T00:00:01+00:00");
    assert_eq!(fractional["scope"]["through"], "2024-01-01T00:02:00+00:00");

    let groups_scope = write_scope(
        &fixture,
        "groups-scope",
        serde_json::json!({"conversationKinds": ["group"]}),
    );
    let groups_state = fixture.directory.path().join("groups-state.json");
    let groups = next_personal_memory_batch_with_scope(
        &corpus,
        &groups_state,
        Some(&wiki),
        128 * 1024,
        Some(&groups_scope),
    )
    .unwrap();
    assert_eq!(groups["position"]["messageCount"], 5);
    assert_eq!(
        groups["scope"]["conversationKinds"],
        serde_json::json!(["group"])
    );

    let self_scope = write_scope(
        &fixture,
        "self-scope.json",
        serde_json::json!({
            "senderSelectors": ["self"]
        }),
    );
    let self_state = fixture.directory.path().join("self-state.json");
    let self_batch = next_personal_memory_batch_with_scope(
        &corpus,
        &self_state,
        Some(&wiki),
        128 * 1024,
        Some(&self_scope),
    )
    .unwrap();
    assert_eq!(self_batch["position"]["messageCount"], 2);
    let self_page = next_personal_memory_page(&corpus, &self_state, "current").unwrap();
    assert!(page_messages(&self_page)
        .iter()
        .all(|message| message["a"] == "self"));

    let multiple_sender_scope = write_scope(
        &fixture,
        "multiple-sender-scope.json",
        serde_json::json!({
            "conversationSelectors": ["wxid_friend"],
            "senderSelectors": ["self", "wxid_friend"]
        }),
    );
    let multiple_sender_state = fixture.directory.path().join("multiple-sender-state.json");
    let multiple_sender = next_personal_memory_batch_with_scope(
        &corpus,
        &multiple_sender_state,
        Some(&wiki),
        128 * 1024,
        Some(&multiple_sender_scope),
    )
    .unwrap();
    assert_eq!(multiple_sender["position"]["messageCount"], 4);
    assert_eq!(multiple_sender["scope"]["senderFilterCount"], 2);

    let combined_scope = write_scope(
        &fixture,
        "combined-scope.json",
        serde_json::json!({
            "conversationSelectors": ["room@chatroom"],
            "through": "2023-04-01T12:00:00Z",
            "senderSelectors": ["wxid_group_friend"]
        }),
    );
    let combined_state = fixture.directory.path().join("combined-state.json");
    let combined = next_personal_memory_batch_with_scope(
        &corpus,
        &combined_state,
        Some(&wiki),
        128 * 1024,
        Some(&combined_scope),
    )
    .unwrap();
    assert_eq!(combined["position"]["messageCount"], 3);
    let combined_page = next_personal_memory_page(&corpus, &combined_state, "current").unwrap();
    assert!(page_messages(&combined_page)
        .iter()
        .all(|message| message["a"] == "other"));

    let person_scope = write_scope(
        &fixture,
        "person-subject-scope.json",
        serde_json::json!({
            "conversationSelectors": ["wxid_friend"],
            "summarySubject": {"kind": "person", "selector": "wxid_friend"}
        }),
    );
    let person_state = fixture.directory.path().join("person-state.json");
    next_personal_memory_batch_with_scope(
        &corpus,
        &person_state,
        Some(&wiki),
        128 * 1024,
        Some(&person_scope),
    )
    .unwrap();
    let person_page = next_personal_memory_page(&corpus, &person_state, "current").unwrap();
    let subject_alias = person_page["scope"]["summarySubject"]["alias"]
        .as_str()
        .unwrap();
    assert!(person_page["targetPages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == &format!("people/{subject_alias}.md")));
    assert!(!person_page["targetPages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "me.md"));

    let conversation_scope = write_scope(
        &fixture,
        "conversation-subject-scope.json",
        serde_json::json!({
            "conversationSelectors": ["room@chatroom"],
            "summarySubject": {"kind": "none"}
        }),
    );
    let conversation_state = fixture.directory.path().join("conversation-state.json");
    next_personal_memory_batch_with_scope(
        &corpus,
        &conversation_state,
        Some(&wiki),
        128 * 1024,
        Some(&conversation_scope),
    )
    .unwrap();
    let conversation_page =
        next_personal_memory_page(&corpus, &conversation_state, "current").unwrap();
    assert!(!conversation_page["targetPages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "me.md"));
    assert!(conversation_page["targetPages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value
            .as_str()
            .is_some_and(|path| path.starts_with("conversations/"))));

    let direct_page = next_personal_memory_page(&corpus, &direct_state, "current").unwrap();
    let all_direct_anchor = find_message_evidence(&all_page, "direct self anchor");
    let scoped_direct_anchor = find_message_evidence(&direct_page, "direct self anchor");
    assert_eq!(all_direct_anchor, scoped_direct_anchor);

    let direct_conversation_alias = direct_page["episodes"][0]["c"].as_str().unwrap();
    let direct_friend_alias = page_messages(&direct_page)
        .into_iter()
        .find(|message| message["x"] == "direct before")
        .unwrap()["p"]
        .as_str()
        .unwrap();
    let alias_scope = write_scope(
        &fixture,
        "alias-scope.json",
        serde_json::json!({
            "conversationSelectors": [direct_conversation_alias],
            "senderSelectors": [direct_friend_alias],
            "summarySubject": {"kind": "person", "selector": direct_friend_alias}
        }),
    );
    let alias_state = fixture.directory.path().join("alias-state.json");
    let alias_batch = next_personal_memory_batch_with_scope(
        &corpus,
        &alias_state,
        Some(&wiki),
        128 * 1024,
        Some(&alias_scope),
    )
    .unwrap();
    assert_eq!(alias_batch["position"]["messageCount"], 3);
    assert_eq!(
        alias_batch["scope"]["summarySubject"]["alias"],
        direct_friend_alias
    );

    let status = personal_memory_status(&corpus, Some(&combined_state)).unwrap();
    assert_eq!(status.scanned_message_count, 9);
    assert_eq!(status.eligible_message_count, 9);
    assert_eq!(status.corpus_message_count, 9);
    assert_eq!(status.selected_message_count, 3);
    assert_eq!(status.scope.conversation_filter_count, 1);
    assert_eq!(status.scope.sender_filter_count, 1);
    assert!(!status.scope.all_messages);

    assert!(next_personal_memory_batch_with_scope(
        &corpus,
        &direct_state,
        Some(&wiki),
        128 * 1024,
        Some(&multiple_scope),
    )
    .is_err());
}

#[test]
fn memory_cli_prepares_canonical_history_and_binds_a_composable_scope() {
    let fixture = Fixture::new();
    let policy = write_canonical_policy(&fixture);
    let corpus = fixture.directory.path().join("cli-canonical-corpus");
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
        "canonical prepare failed; stderr: {}; stdout: {}",
        String::from_utf8_lossy(&prepared.stderr),
        String::from_utf8_lossy(&prepared.stdout)
    );
    let manifest: Value = serde_json::from_slice(&prepared.stdout).unwrap();
    assert_eq!(manifest["corpusMode"], "allMessages");
    assert_eq!(manifest["selectedMessageCount"], 9);

    let wiki = fixture.directory.path().join("cli-canonical-wiki");
    fs::create_dir(&wiki).unwrap();
    fs::set_permissions(&wiki, fs::Permissions::from_mode(0o700)).unwrap();
    let state = fixture.directory.path().join("cli-canonical-state.json");
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
        "--conversation",
        "room@chatroom",
        "--conversation-kind",
        "group",
        "--through",
        "2023-04-01T12:00:00Z",
        "--sender",
        "wxid_group_friend",
        "--subject",
        "none",
    ]);
    assert_success(&next);
    let next: Value = serde_json::from_slice(&next.stdout).unwrap();
    assert_eq!(next["position"]["messageCount"], 3);
    assert_eq!(next["scope"]["summarySubject"]["kind"], "none");

    let page = run(&[
        "memory",
        "page",
        corpus.to_str().unwrap(),
        "--state",
        state.to_str().unwrap(),
    ]);
    assert_success(&page);
    let page: Value = serde_json::from_slice(&page.stdout).unwrap();
    assert_eq!(page_message_count(&page), 3);
    assert!(page["targetPages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value
            .as_str()
            .is_some_and(|path| path.starts_with("conversations/"))));

    let status = run(&[
        "memory",
        "status",
        corpus.to_str().unwrap(),
        "--state",
        state.to_str().unwrap(),
    ]);
    assert_success(&status);
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["corpusMessageCount"], 9);
    assert_eq!(status["selectedMessageCount"], 3);
    assert_eq!(status["scope"]["conversationFilterCount"], 1);
    assert_eq!(status["scope"]["senderFilterCount"], 1);
    assert_eq!(status["scope"]["summarySubject"], "none");
    assert_eq!(status["scope"]["from"], Value::Null);
    assert_eq!(status["scope"]["through"], "2023-04-01T12:00:00+00:00");

    let repeated_state = fixture.directory.path().join("cli-repeated-state.json");
    let repeated = run(&[
        "memory",
        "next",
        corpus.to_str().unwrap(),
        "--state",
        repeated_state.to_str().unwrap(),
        "--wiki",
        wiki.to_str().unwrap(),
        "--max-text-bytes",
        "65536",
        "--conversation",
        "wxid_friend",
        "--conversation",
        "room@chatroom",
        "--conversation-kind",
        "direct",
        "--conversation-kind",
        "group",
        "--sender",
        "self",
        "--sender",
        "wxid_group_friend",
    ]);
    assert_success(&repeated);
    let repeated: Value = serde_json::from_slice(&repeated.stdout).unwrap();
    assert_eq!(repeated["position"]["messageCount"], 6);
    assert_eq!(repeated["scope"]["conversationFilterCount"], 2);
    assert_eq!(repeated["scope"]["senderFilterCount"], 2);
    assert_eq!(
        repeated["scope"]["conversationKinds"],
        serde_json::json!(["direct", "group"])
    );

    let invalid_state = fixture.directory.path().join("cli-invalid-time-state.json");
    let invalid_time = run(&[
        "memory",
        "next",
        corpus.to_str().unwrap(),
        "--state",
        invalid_state.to_str().unwrap(),
        "--wiki",
        wiki.to_str().unwrap(),
        "--max-text-bytes",
        "65536",
        "--from",
        "2024-01-01T00:00:00",
    ]);
    assert!(!invalid_time.status.success());
    assert!(String::from_utf8_lossy(&invalid_time.stderr).contains("RFC 3339"));
    assert!(!invalid_state.exists());

    let obsolete_scope = run(&[
        "memory",
        "next",
        corpus.to_str().unwrap(),
        "--state",
        invalid_state.to_str().unwrap(),
        "--wiki",
        wiki.to_str().unwrap(),
        "--max-text-bytes",
        "65536",
        "--scope",
        "scope.json",
    ]);
    assert!(!obsolete_scope.status.success());
    assert!(String::from_utf8_lossy(&obsolete_scope.stderr).contains("unsupported option: --scope"));
}

#[test]
fn canonical_scope_validation_fails_closed_and_empty_matches_complete_cleanly() {
    let fixture = Fixture::new();
    let policy = write_canonical_policy(&fixture);
    let source = LiveQuerySource::open(&fixture.root, QueryDatabaseAccess::Decrypted).unwrap();
    let corpus = fixture.directory.path().join("scope-validation-corpus");
    prepare_personal_memory_corpus(&source, &policy, &corpus).unwrap();
    let wiki = fixture.directory.path().join("scope-validation-wiki");
    fs::create_dir(&wiki).unwrap();
    fs::set_permissions(&wiki, fs::Permissions::from_mode(0o700)).unwrap();

    for (name, fields) in [
        (
            "unknown-conversation.json",
            serde_json::json!({"conversationSelectors": ["does-not-exist"]}),
        ),
        (
            "unknown-sender.json",
            serde_json::json!({"senderSelectors": ["does-not-exist"]}),
        ),
        (
            "unknown-subject.json",
            serde_json::json!({
                "summarySubject": {"kind": "person", "selector": "does-not-exist"}
            }),
        ),
        (
            "duplicate-selector.json",
            serde_json::json!({"conversationSelectors": ["wxid_friend", "wxid_friend"]}),
        ),
        (
            "duplicate-self-selector.json",
            serde_json::json!({"senderSelectors": ["self", "accountHolder"]}),
        ),
        (
            "inverted-time.json",
            serde_json::json!({
                "from": "2024-01-02T00:00:00Z",
                "through": "2024-01-01T00:00:00Z"
            }),
        ),
        (
            "missing-offset.json",
            serde_json::json!({"from": "2024-01-01T00:00:00"}),
        ),
        (
            "date-only.json",
            serde_json::json!({"through": "2024-01-01"}),
        ),
        (
            "duplicate-kind.json",
            serde_json::json!({"conversationKinds": ["group", "group"]}),
        ),
    ] {
        let scope = write_scope(&fixture, name, fields);
        let state = fixture.directory.path().join(format!("{name}.state"));
        assert!(next_personal_memory_batch_with_scope(
            &corpus,
            &state,
            Some(&wiki),
            64 * 1024,
            Some(&scope),
        )
        .is_err());
        assert!(!state.exists());
    }

    let direct_alias = read_conversation_alias(&corpus, "wxid_friend");
    let duplicate_resolved_scope = write_scope(
        &fixture,
        "duplicate-resolved-conversation.json",
        serde_json::json!({"conversationSelectors": ["wxid_friend", direct_alias]}),
    );
    let duplicate_resolved_state = fixture
        .directory
        .path()
        .join("duplicate-resolved-conversation.state");
    assert!(next_personal_memory_batch_with_scope(
        &corpus,
        &duplicate_resolved_state,
        Some(&wiki),
        64 * 1024,
        Some(&duplicate_resolved_scope),
    )
    .is_err());
    assert!(!duplicate_resolved_state.exists());

    let empty_scope = write_scope(
        &fixture,
        "empty-match.json",
        serde_json::json!({
            "from": "2033-05-18T03:33:20Z",
            "through": "2033-05-18T03:35:00Z",
            "summarySubject": {"kind": "none"}
        }),
    );
    let empty_state = fixture.directory.path().join("empty-match-state.json");
    let empty = next_personal_memory_batch_with_scope(
        &corpus,
        &empty_state,
        Some(&wiki),
        64 * 1024,
        Some(&empty_scope),
    )
    .unwrap();
    assert_eq!(empty["complete"], true);
    assert_eq!(empty["position"]["messageCount"], 0);
    assert_eq!(empty["position"]["totalUnits"], 0);
    let status = personal_memory_status(&corpus, Some(&empty_state)).unwrap();
    assert!(status.complete);
    assert_eq!(status.selected_message_count, 0);
    assert_eq!(status.progress_percent, 100.0);

    let direct_scope = write_scope(
        &fixture,
        "rebound-direct.json",
        serde_json::json!({"conversationSelectors": ["wxid_friend"]}),
    );
    let rebound = next_personal_memory_batch_with_scope(
        &corpus,
        &empty_state,
        Some(&wiki),
        64 * 1024,
        Some(&direct_scope),
    )
    .unwrap();
    assert_eq!(rebound["complete"], false);
    assert_eq!(rebound["position"]["messageCount"], 4);
    let rebound_status = personal_memory_status(&corpus, Some(&empty_state)).unwrap();
    assert_eq!(rebound_status.completed_scope_count, 1);
    assert_eq!(rebound_status.selected_message_count, 4);
}

#[test]
fn canonical_corpus_reviews_rows_from_unresolved_hashed_message_tables() {
    let fixture = Fixture::new();
    fixture.add_unmatched_message();
    let policy = write_canonical_policy(&fixture);
    let source = LiveQuerySource::open(&fixture.root, QueryDatabaseAccess::Decrypted).unwrap();
    let corpus = fixture.directory.path().join("unresolved-canonical-corpus");
    let manifest = prepare_personal_memory_corpus(&source, &policy, &corpus).unwrap();
    assert_eq!(manifest.scanned_message_count, 10);
    assert_eq!(manifest.selected_message_count, 10);
    assert_eq!(manifest.evidence_count, 10);
    assert_eq!(manifest.unmatched_message_table_count, 1);
    assert!(!manifest.source_coverage_complete);
    let evidence = fs::read_to_string(corpus.join("evidence.jsonl")).unwrap();
    assert!(evidence.contains("unresolved table message"));

    let wiki = fixture.directory.path().join("unresolved-wiki");
    fs::create_dir(&wiki).unwrap();
    fs::set_permissions(&wiki, fs::Permissions::from_mode(0o700)).unwrap();
    let state = fixture.directory.path().join("unresolved-state.json");
    let batch =
        next_personal_memory_batch_with_scope(&corpus, &state, Some(&wiki), 128 * 1024, None)
            .unwrap();
    assert_eq!(batch["position"]["messageCount"], 10);
    let page = next_personal_memory_page(&corpus, &state, "current").unwrap();
    assert!(page_messages(&page)
        .iter()
        .any(|message| message["x"] == "unresolved table message"));
    let status = personal_memory_status(&corpus, Some(&state)).unwrap();
    assert!(status.row_coverage_complete);
    assert!(!status.source_coverage_complete);
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
                    ('wxid_friend', 'friend_alias', 'Friend Remark', 'Friend Nickname'),
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

    fn add_unmatched_message(&self) {
        let connection = Connection::open(self.root.join("message/message_0.db")).unwrap();
        create_message_table(&connection, "not-in-contact-or-session");
        insert_message(
            &connection,
            "not-in-contact-or-session",
            1,
            2,
            1_704_067_400,
            "unresolved table message",
        );
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

fn write_canonical_policy(fixture: &Fixture) -> PathBuf {
    let policy_path = fixture.directory.path().join("canonical-policy.json");
    write_private(
        &policy_path,
        &serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "greenbubbles.personal-memory-selection-policy.v2",
            "formatVersion": 2,
            "corpusMode": "allMessages",
            "timezone": "UTC",
            "maximumMessageTextBytes": 4096,
            "maximumUnitMessages": 160,
            "maximumUnitTextBytes": 16384,
            "deliveryOrder": "accountHolderRelevance",
            "includeDirectConversations": true,
            "includeGroupConversations": true,
            "includeOfficialAccounts": true,
            "includeServiceAccounts": true
        }))
        .unwrap(),
    );
    policy_path
}

fn write_scope(_fixture: &Fixture, _name: &str, fields: Value) -> PersonalMemoryScopeOptions {
    let strings = |key: &str| {
        fields[key]
            .as_array()
            .into_iter()
            .flatten()
            .map(|value| value.as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let conversation_kinds = strings("conversationKinds")
        .iter()
        .map(|value| PersonalMemoryConversationKindSelector::parse_cli(value).unwrap())
        .collect();
    let summary_subject = match fields["summarySubject"]["kind"].as_str() {
        Some("person") => PersonalMemorySummarySubjectSelector::Person {
            selector: fields["summarySubject"]["selector"]
                .as_str()
                .unwrap()
                .to_string(),
        },
        Some("none") => PersonalMemorySummarySubjectSelector::None,
        Some("accountHolder") | None => PersonalMemorySummarySubjectSelector::AccountHolder,
        Some(kind) => panic!("unsupported test summary subject: {kind}"),
    };
    PersonalMemoryScopeOptions {
        conversation_selectors: strings("conversationSelectors"),
        conversation_kinds,
        from: fields["from"].as_str().map(str::to_string),
        through: fields["through"].as_str().map(str::to_string),
        sender_selectors: strings("senderSelectors"),
        summary_subject,
    }
}

fn page_messages(page: &Value) -> Vec<&Value> {
    page["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|episode| episode["m"].as_array().unwrap())
        .collect()
}

fn page_message_count(page: &Value) -> usize {
    page_messages(page).len()
}

fn find_message_evidence<'a>(page: &'a Value, text: &str) -> &'a str {
    page_messages(page)
        .into_iter()
        .find(|message| message["x"] == text)
        .unwrap()["e"]
        .as_str()
        .unwrap()
}

fn read_conversation_alias(corpus: &Path, source_id: &str) -> String {
    fs::read_to_string(corpus.join("conversations.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|record| record["sourceId"] == source_id)
        .unwrap()["alias"]
        .as_str()
        .unwrap()
        .to_string()
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
