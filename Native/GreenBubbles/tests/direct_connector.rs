use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use greenbubbles::connector::{
    audit_connector_log, ConnectorDestination, ConnectorErrorCode, ConnectorOperation,
    ConnectorRequest, ConnectorResult, CONNECTOR_API_VERSION,
};
use greenbubbles::direct_connector::DirectConnectorService;
use greenbubbles::live_query::{LiveQuerySource, QueryDatabaseAccess};
use greenbubbles::model::EntityDecodeState;
use greenbubbles::tools::{
    create_direct_tool_policy, ConversationToolScope, ToolCapability, ToolMessageField,
};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[test]
fn policy_scoped_connector_reads_sqlite_directly_with_paging_search_and_audit() {
    let fixture = Fixture::new();
    let source = LiveQuerySource::open(&fixture.source, QueryDatabaseAccess::Decrypted).unwrap();
    let before = database_hashes(&fixture.source);

    let mut scopes = BTreeMap::new();
    scopes.insert(
        "wxid_allowed".to_string(),
        ConversationToolScope {
            capabilities: BTreeSet::from([
                ToolCapability::ListConversations,
                ToolCapability::ReadRecentMessages,
                ToolCapability::SearchMessages,
            ]),
            message_fields: BTreeSet::from([
                ToolMessageField::Sender,
                ToolMessageField::CreatedAt,
                ToolMessageField::Direction,
                ToolMessageField::MessageType,
                ToolMessageField::Content,
            ]),
            not_before_unix: Some(850),
            not_after_unix: Some(1_100),
            allow_remote_model: false,
        },
    );
    scopes.insert(
        "wxid_second".to_string(),
        ConversationToolScope {
            capabilities: BTreeSet::from([ToolCapability::ListConversations]),
            message_fields: BTreeSet::new(),
            not_before_unix: None,
            not_after_unix: None,
            allow_remote_model: false,
        },
    );
    create_direct_tool_policy(&fixture.policy, source.identity(), scopes, 2, 7).unwrap();
    let service = DirectConnectorService::open(source, &fixture.policy, &fixture.audit).unwrap();

    let status = service.handle(request(
        "status",
        ConnectorDestination::Local,
        ConnectorOperation::Status,
    ));
    let ConnectorResult::DirectStatus(status) = status.result.unwrap() else {
        panic!("unexpected direct connector result")
    };
    assert_eq!(status.enabled_conversation_count, 2);

    let first = service.handle(request(
        "list-1",
        ConnectorDestination::Local,
        ConnectorOperation::ListConversations {
            cursor: None,
            limit: Some(1),
        },
    ));
    let ConnectorResult::Conversations(first) = first.result.unwrap() else {
        panic!("unexpected direct connector result")
    };
    assert_eq!(first.conversations.len(), 1);
    assert_eq!(first.conversations[0].conversation_id, "wxid_allowed");
    assert_eq!(first.conversations[0].human_label, "Allowed Conversation");
    assert_eq!(
        first.conversations[0].entity_decode_state,
        EntityDecodeState::Complete
    );
    let cursor = first.next_cursor.unwrap();
    assert!(!cursor.contains("wxid_allowed"));

    let second = service.handle(request(
        "list-2",
        ConnectorDestination::Local,
        ConnectorOperation::ListConversations {
            cursor: Some(cursor),
            limit: Some(1),
        },
    ));
    let ConnectorResult::Conversations(second) = second.result.unwrap() else {
        panic!("unexpected direct connector result")
    };
    assert_eq!(second.conversations.len(), 1);
    assert_eq!(second.conversations[0].conversation_id, "wxid_second");
    assert!(second.next_cursor.is_none());

    let messages = service.handle(request(
        "messages",
        ConnectorDestination::Local,
        ConnectorOperation::GetMessages {
            conversation_id: "wxid_allowed".to_string(),
            cursor: None,
            limit: Some(50),
        },
    ));
    let ConnectorResult::Messages(messages) = messages.result.unwrap() else {
        panic!("unexpected direct connector result")
    };
    assert_eq!(messages.messages.len(), 2);
    assert_eq!(messages.messages[0].created_at_unix, Some(1_000));
    assert_eq!(
        messages.messages[0].sender_display_name.as_deref(),
        Some("You")
    );
    assert_eq!(messages.messages[0].is_account_holder, Some(true));
    assert_eq!(
        messages.messages[0].direction,
        Some(greenbubbles::MessageDirection::Outgoing)
    );
    assert_eq!(messages.messages[1].created_at_unix, Some(950));
    assert_eq!(messages.messages[1].is_account_holder, None);
    assert_eq!(messages.messages[1].direction, None);
    assert_eq!(
        messages.messages[0].payload_summary.as_deref(),
        Some("new mes")
    );
    assert_eq!(messages.messages[0].payload_summary_truncated, Some(true));
    assert!(!messages
        .limitation_codes
        .contains(&"directDirectionUnavailable".to_string()));

    let search = service.handle(request(
        "search",
        ConnectorDestination::Local,
        ConnectorOperation::SearchMessages {
            query: "needle".to_string(),
            conversation_id: None,
            cursor: None,
            limit: Some(10),
        },
    ));
    let ConnectorResult::Messages(search) = search.result.unwrap() else {
        panic!("unexpected direct connector result")
    };
    assert_eq!(search.messages.len(), 1);
    assert_eq!(search.messages[0].conversation_id, "wxid_allowed");
    assert_eq!(
        search.messages[0].sender_display_name.as_deref(),
        Some("Sender Remark")
    );
    assert_eq!(search.messages[0].is_account_holder, Some(false));
    assert_eq!(
        search.messages[0].direction,
        Some(greenbubbles::MessageDirection::Incoming)
    );
    assert_eq!(
        search.messages[0].payload_summary.as_deref(),
        Some("needle ")
    );
    let search_identity = search.messages[0].canonical_id.clone();

    let exact = service.handle(request(
        "exact-search-hit",
        ConnectorDestination::Local,
        ConnectorOperation::GetMessage {
            canonical_id: search_identity.clone(),
        },
    ));
    assert!(exact.ok, "{:?}", exact.error);
    let ConnectorResult::Message(Some(exact)) = exact.result.unwrap() else {
        panic!("unexpected direct connector result")
    };
    assert_eq!(exact.canonical_id, search_identity);
    assert_eq!(exact.payload_summary.as_deref(), Some("needle "));
    assert_eq!(exact.is_account_holder, Some(false));

    let denied = service.handle(request(
        "remote-denied",
        ConnectorDestination::RemoteModel,
        ConnectorOperation::GetMessages {
            conversation_id: "wxid_allowed".to_string(),
            cursor: None,
            limit: Some(1),
        },
    ));
    assert!(!denied.ok);
    assert_eq!(denied.error.unwrap().code, ConnectorErrorCode::Unauthorized);

    let replica_only = service.handle(request(
        "replica-only",
        ConnectorDestination::Local,
        ConnectorOperation::GetChanges {
            cursor: None,
            limit: Some(1),
        },
    ));
    assert!(!replica_only.ok);
    assert_eq!(
        replica_only.error.unwrap().code,
        ConnectorErrorCode::Unavailable
    );

    let audit = audit_connector_log(&fixture.audit).unwrap();
    assert!(audit.chain_verified);
    assert!(audit.fully_chained);
    assert_eq!(audit.denied_event_count, 2);
    assert_eq!(database_hashes(&fixture.source), before);
}

#[test]
fn direct_connector_omits_account_marker_when_sender_is_withheld_by_policy() {
    let fixture = Fixture::new();
    let source = LiveQuerySource::open(&fixture.source, QueryDatabaseAccess::Decrypted).unwrap();
    create_direct_tool_policy(
        &fixture.policy,
        source.identity(),
        BTreeMap::from([(
            "wxid_allowed".to_string(),
            ConversationToolScope {
                capabilities: BTreeSet::from([ToolCapability::ReadRecentMessages]),
                message_fields: BTreeSet::from([
                    ToolMessageField::CreatedAt,
                    ToolMessageField::Direction,
                    ToolMessageField::Content,
                ]),
                not_before_unix: Some(990),
                not_after_unix: Some(1_010),
                allow_remote_model: false,
            },
        )]),
        10,
        100,
    )
    .unwrap();
    let service = DirectConnectorService::open(source, &fixture.policy, &fixture.audit).unwrap();
    let response = service.handle(request(
        "sender-withheld",
        ConnectorDestination::Local,
        ConnectorOperation::GetMessages {
            conversation_id: "wxid_allowed".to_string(),
            cursor: None,
            limit: Some(10),
        },
    ));
    assert!(response.ok, "{:?}", response.error);
    let ConnectorResult::Messages(page) = response.result.unwrap() else {
        panic!("unexpected direct connector result")
    };
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].sender_id, None);
    assert_eq!(page.messages[0].sender_display_name, None);
    assert_eq!(page.messages[0].is_account_holder, None);
    assert_eq!(
        page.messages[0].direction,
        Some(greenbubbles::MessageDirection::Outgoing)
    );
}

#[test]
fn direct_policy_is_bound_to_one_source_identity() {
    let first = Fixture::new();
    let second = Fixture::new();
    let first_source =
        LiveQuerySource::open(&first.source, QueryDatabaseAccess::Decrypted).unwrap();
    create_direct_tool_policy(
        &first.policy,
        first_source.identity(),
        BTreeMap::from([(
            "wxid_allowed".to_string(),
            ConversationToolScope {
                capabilities: BTreeSet::from([ToolCapability::ListConversations]),
                message_fields: BTreeSet::new(),
                not_before_unix: None,
                not_after_unix: None,
                allow_remote_model: false,
            },
        )]),
        10,
        100,
    )
    .unwrap();
    let second_source =
        LiveQuerySource::open(&second.source, QueryDatabaseAccess::Decrypted).unwrap();
    assert!(DirectConnectorService::open(second_source, &first.policy, &second.audit).is_err());
}

#[test]
fn group_conversation_label_comes_from_the_group_contact_not_the_last_sender() {
    let fixture = Fixture::new();
    let session = Connection::open(fixture.source.join("session/session.db")).unwrap();
    session
        .execute(
            "INSERT INTO SessionTable VALUES (?1, 40, 'group', 1, 'wxid_sender', 'Wrong Member')",
            ["room@chatroom"],
        )
        .unwrap();
    drop(session);
    let contact = Connection::open(fixture.source.join("contact/contact.db")).unwrap();
    contact
        .execute(
            "INSERT INTO contact(username, remark) VALUES (?1, ?2)",
            ["room@chatroom", "Study Group"],
        )
        .unwrap();
    drop(contact);

    let source = LiveQuerySource::open(&fixture.source, QueryDatabaseAccess::Decrypted).unwrap();
    create_direct_tool_policy(
        &fixture.policy,
        source.identity(),
        BTreeMap::from([(
            "room@chatroom".to_string(),
            ConversationToolScope {
                capabilities: BTreeSet::from([ToolCapability::ListConversations]),
                message_fields: BTreeSet::new(),
                not_before_unix: None,
                not_after_unix: None,
                allow_remote_model: false,
            },
        )]),
        10,
        100,
    )
    .unwrap();
    let service = DirectConnectorService::open(source, &fixture.policy, &fixture.audit).unwrap();
    let response = service.handle(request(
        "group-label",
        ConnectorDestination::Local,
        ConnectorOperation::ListConversations {
            cursor: None,
            limit: Some(10),
        },
    ));
    let ConnectorResult::Conversations(page) = response.result.unwrap() else {
        panic!("unexpected direct connector result")
    };
    assert_eq!(page.conversations.len(), 1);
    assert_eq!(page.conversations[0].human_label, "Study Group");
    assert_eq!(
        page.conversations[0].entity_decode_state,
        EntityDecodeState::Complete
    );
}

#[test]
fn direct_connector_uses_bounded_fallback_and_hydrates_without_native_fts() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.source.join("message/message_fts.db")).unwrap();
    let source = LiveQuerySource::open(&fixture.source, QueryDatabaseAccess::Decrypted).unwrap();
    create_direct_tool_policy(
        &fixture.policy,
        source.identity(),
        BTreeMap::from([(
            "wxid_allowed".to_string(),
            ConversationToolScope {
                capabilities: BTreeSet::from([ToolCapability::SearchMessages]),
                message_fields: BTreeSet::from([
                    ToolMessageField::Sender,
                    ToolMessageField::CreatedAt,
                    ToolMessageField::Content,
                ]),
                not_before_unix: Some(850),
                not_after_unix: Some(1_100),
                allow_remote_model: false,
            },
        )]),
        10,
        100,
    )
    .unwrap();
    let before = database_hashes(&fixture.source);
    let service = DirectConnectorService::open(source, &fixture.policy, &fixture.audit).unwrap();
    let search = service.handle(request(
        "fallback-search",
        ConnectorDestination::Local,
        ConnectorOperation::SearchMessages {
            query: "needle".to_string(),
            conversation_id: Some("wxid_allowed".to_string()),
            cursor: None,
            limit: Some(10),
        },
    ));
    assert!(search.ok, "{:?}", search.error);
    let ConnectorResult::Messages(search) = search.result.unwrap() else {
        panic!("unexpected direct connector result")
    };
    assert_eq!(search.messages.len(), 1);
    assert!(search
        .limitation_codes
        .contains(&"directQuery.fallbackSearchSourceWindowBounded".to_string()));
    let identity = search.messages[0].canonical_id.clone();

    let exact = service.handle(request(
        "fallback-exact",
        ConnectorDestination::Local,
        ConnectorOperation::GetMessage {
            canonical_id: identity.clone(),
        },
    ));
    assert!(exact.ok, "{:?}", exact.error);
    let ConnectorResult::Message(Some(exact)) = exact.result.unwrap() else {
        panic!("unexpected direct connector result")
    };
    assert_eq!(exact.canonical_id, identity);
    assert_eq!(exact.payload_summary.as_deref(), Some("needle source"));
    assert_eq!(database_hashes(&fixture.source), before);
}

#[test]
fn direct_connector_cli_creates_a_source_bound_owner_only_policy() {
    let fixture = Fixture::new();
    let policy = fixture
        .policy
        .parent()
        .unwrap()
        .join("cli-direct-policy.json");
    let output = Command::new(env!("CARGO_BIN_EXE_greenbubbles"))
        .args([
            "connector-policy-direct",
            fixture.source.to_str().unwrap(),
            policy.to_str().unwrap(),
            "wxid_allowed",
            "--capabilities",
            "list,read,search",
            "--fields",
            "sender,created-at,type,content",
            "--decrypted",
            "--max-results",
            "25",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["maximumResultCount"], 25);
    assert_eq!(value["conversationScopes"].as_object().unwrap().len(), 1);
    assert_eq!(
        fs::metadata(&policy).unwrap().permissions().mode() & 0o077,
        0
    );

    let request_path = fixture.policy.parent().unwrap().join("direct-request.json");
    fs::write(
        &request_path,
        serde_json::to_vec(&request(
            "cli-query",
            ConnectorDestination::Local,
            ConnectorOperation::ListConversations {
                cursor: None,
                limit: Some(1),
            },
        ))
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&request_path, fs::Permissions::from_mode(0o600)).unwrap();
    let audit = fixture.policy.parent().unwrap().join("cli-audit.ndjson");
    let query = Command::new(env!("CARGO_BIN_EXE_greenbubbles"))
        .args([
            "connector-query-direct",
            fixture.source.to_str().unwrap(),
            policy.to_str().unwrap(),
            audit.to_str().unwrap(),
            request_path.to_str().unwrap(),
            "--decrypted",
        ])
        .output()
        .unwrap();
    assert!(
        query.status.success(),
        "{}",
        String::from_utf8_lossy(&query.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&query.stdout).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"]["kind"], "conversations");
    assert_eq!(
        response["result"]["value"]["conversations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(audit_connector_log(&audit).unwrap().chain_verified);

    let missing = fixture.policy.parent().unwrap().join("missing-policy.json");
    let failure = Command::new(env!("CARGO_BIN_EXE_greenbubbles"))
        .args([
            "connector-policy-direct",
            fixture.source.to_str().unwrap(),
            missing.to_str().unwrap(),
            "wxid_missing",
            "--capabilities",
            "list",
            "--fields",
            "sender",
            "--decrypted",
        ])
        .output()
        .unwrap();
    assert!(!failure.status.success());
    assert!(!missing.exists());
}

fn request(
    request_id: &str,
    destination: ConnectorDestination,
    operation: ConnectorOperation,
) -> ConnectorRequest {
    ConnectorRequest {
        api_version: CONNECTOR_API_VERSION.to_string(),
        request_id: request_id.to_string(),
        requester_id: "test-agent".to_string(),
        destination,
        operation,
    }
}

struct Fixture {
    _directory: TempDir,
    source: PathBuf,
    policy: PathBuf,
    audit: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let source = directory
            .path()
            .join("xwechat_files/wxid_self_ab12/db_storage");
        let private = directory.path().join("private");
        for path in [
            &source,
            &source.join("contact"),
            &source.join("session"),
            &source.join("message"),
            &private,
        ] {
            fs::create_dir_all(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Connection::open(source.join("contact/contact.db"))
            .unwrap()
            .execute_batch(
                "CREATE TABLE contact(
                    username TEXT PRIMARY KEY,
                    alias BLOB,
                    remark BLOB,
                    nick_name BLOB
                 );
                 INSERT INTO contact VALUES
                    ('wxid_allowed', '', 'Allowed Conversation', ''),
                    ('wxid_second', '', 'Second Conversation', ''),
                    ('wxid_blocked', '', 'Blocked Conversation', ''),
                    ('wxid_self', '', 'Account Holder', ''),
                    ('wxid_sender', '', 'Sender Remark', '');",
            )
            .unwrap();
        Connection::open(source.join("session/session.db"))
            .unwrap()
            .execute_batch(
                "CREATE TABLE SessionTable(
                    username TEXT NOT NULL,
                    sort_timestamp INTEGER NOT NULL,
                    summary BLOB,
                    last_msg_type INTEGER,
                    last_msg_sender TEXT,
                    last_sender_display_name TEXT
                 );
                 INSERT INTO SessionTable VALUES
                    ('wxid_allowed', 30, 'allowed', 1, 'wxid_allowed', 'Allowed'),
                    ('wxid_second', 20, 'second', 1, 'wxid_second', 'Second'),
                    ('wxid_blocked', 10, 'blocked', 1, 'wxid_blocked', 'Blocked');",
            )
            .unwrap();

        let table = format!("Msg_{:x}", md5::compute(b"wxid_allowed"));
        let messages = Connection::open(source.join("message/message_0.db")).unwrap();
        messages
            .execute_batch(&format!(
                "CREATE TABLE Name2Id(user_name TEXT);
                 INSERT INTO Name2Id(rowid, user_name) VALUES (1, 'wxid_sender');
                 INSERT INTO Name2Id(rowid, user_name) VALUES (2, 'wxid_self');
                 CREATE TABLE [{table}](
                    local_id INTEGER,
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
        for (local_id, sort_sequence, created_at, sender, body) in [
            (1, 100, 1_000, Some(2_i64), "new message"),
            (4, 95, 950, None, "senderless event"),
            (2, 90, 900, Some(1_i64), "needle source"),
            (3, 80, 800, Some(1_i64), "outside policy"),
        ] {
            messages
                .execute(
                    &format!(
                        "INSERT INTO [{table}](local_id, server_id, sort_seq, local_type, \
                         real_sender_id, create_time, status, message_content, \
                         WCDB_CT_message_content) VALUES (?1, ?1, ?2, 1, ?3, ?4, 0, ?5, 0)"
                    ),
                    params![local_id, sort_sequence, sender, created_at, body.as_bytes()],
                )
                .unwrap();
        }
        drop(messages);

        let fts = Connection::open(source.join("message/message_fts.db")).unwrap();
        fts.execute_batch(
            "CREATE TABLE name2id(rowid INTEGER PRIMARY KEY, username TEXT NOT NULL);
             INSERT INTO name2id VALUES (1, 'wxid_allowed');
             INSERT INTO name2id VALUES (2, 'wxid_sender');
             INSERT INTO name2id VALUES (3, 'wxid_blocked');
             CREATE VIRTUAL TABLE message_fts_v4_0 USING fts5(
                acontent, message_local_id UNINDEXED, sort_seq UNINDEXED,
                local_type UNINDEXED, session_id UNINDEXED, sender_id UNINDEXED,
                create_time UNINDEXED, tokenize='unicode61'
             );
             INSERT INTO message_fts_v4_0 VALUES
                ('needle blocked', 1, 200, 1, 3, 2, 1050),
                ('needle source', 2, 90, 1, 1, 2, 900);",
        )
        .unwrap();

        Self {
            policy: private.join("direct-policy.json"),
            audit: private.join("audit.ndjson"),
            _directory: directory,
            source,
        }
    }
}

fn database_hashes(root: &Path) -> BTreeMap<String, String> {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            let relative = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let digest = hex::encode(Sha256::digest(fs::read(entry.path()).unwrap()));
            (relative, digest)
        })
        .collect()
}
