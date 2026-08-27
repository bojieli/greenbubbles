use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use greenbubbles_restore::replica::{
    bootstrap_replica, get_replica_changes, get_replica_message, list_replica_conversations,
    replica_conversation_references_artifact_in_range, replica_coverage, replica_status,
    search_replica_messages, synchronize_replica, ReplicaMessageFilter,
};
use greenbubbles_restore::tools::{
    create_tool_policy, ConversationToolScope, ToolCapability, ToolMessageField,
};
use greenbubbles_restore::{
    connector::{
        ConnectorDestination, ConnectorErrorCode, ConnectorOperation, ConnectorRequest,
        ConnectorResult, ConnectorService, CONNECTOR_API_VERSION,
    },
    transport::{send_unix_request, serve_unix_once},
};
use greenbubbles_restore::{
    ArtifactAvailability, ArtifactDecodeState, ArtifactKind, ArtifactRole, CanonicalArtifact,
    CanonicalConversation, CanonicalMessage, CanonicalParticipant, ClientBuildCompatibilityState,
    ConversationKind, DirectionEvidence, EntityDecodeState, LocalProfileState,
    MessageArtifactReference, MessageDirection, MessageOrderingBasis, ReplicaKey,
    RestorationCompletion, RestorationCoverage, RestorationIntegrity, RestorationReport,
    SemanticDecodeState, SnapshotAcquisitionEvidence, SnapshotAcquisitionMode, TypedPayload,
};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};

const KEY_BYTES: [u8; 32] = [0x31; 32];
const WRONG_KEY_BYTES: [u8; 32] = [0x32; 32];
const PRIVATE_TEXT: &str = "encrypted replica private text";

#[test]
fn serves_scoped_replica_reads_and_complete_non_executing_drafts() {
    let fixture = tempfile::tempdir().unwrap();
    let private = fixture.path().join("private");
    let drafts = private.join("drafts");
    fs::create_dir(&private).unwrap();
    fs::create_dir(&drafts).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&drafts, fs::Permissions::from_mode(0o700)).unwrap();
    let archive = build_archive(&private, "archive", "account-a", "source-a");
    let replica = private.join("replica.db");
    let key = ReplicaKey::from_bytes(KEY_BYTES);
    bootstrap_replica(&archive, &replica, &key).unwrap();
    assert!(replica_conversation_references_artifact_in_range(
        &replica,
        &key,
        "conversation-a",
        "artifact-a",
        Some(1_700_000_000),
        Some(1_700_000_000),
    )
    .unwrap());
    assert!(!replica_conversation_references_artifact_in_range(
        &replica,
        &key,
        "conversation-a",
        "artifact-a",
        Some(1_700_000_001),
        None,
    )
    .unwrap());
    let policy = private.join("policy.json");
    create_tool_policy(
        &archive,
        &policy,
        BTreeMap::from([(
            "conversation-a".to_string(),
            ConversationToolScope {
                capabilities: BTreeSet::from([
                    ToolCapability::ListConversations,
                    ToolCapability::ReadRecentMessages,
                    ToolCapability::SearchMessages,
                    ToolCapability::CreateDraft,
                ]),
                message_fields: BTreeSet::from([
                    ToolMessageField::Sender,
                    ToolMessageField::CreatedAt,
                    ToolMessageField::Direction,
                    ToolMessageField::MessageType,
                    ToolMessageField::Content,
                    ToolMessageField::Attachments,
                    ToolMessageField::Relationships,
                ]),
                not_before_unix: None,
                not_after_unix: None,
                allow_remote_model: false,
            },
        )]),
        100,
        4_096,
        16_384,
    )
    .unwrap();
    let audit = private.join("connector-audit.ndjson");
    let service = ConnectorService::open(&replica, &key, &policy, &audit, &drafts).unwrap();

    let capabilities = service.handle(connector_request(
        "capabilities",
        ConnectorDestination::Local,
        ConnectorOperation::Capabilities,
    ));
    let ConnectorResult::Capabilities(capabilities) = capabilities.result.unwrap() else {
        panic!("unexpected connector result")
    };
    assert!(capabilities.passive_read.enabled);
    assert!(capabilities.draft.enabled);
    assert!(!capabilities.text_send.available);
    assert!(!capabilities.reply_send.available);
    assert!(!capabilities.file_send.available);
    assert!(!capabilities.cached_moments_read.available);
    assert!(!capabilities.cached_moments_read.enabled);
    assert!(capabilities.operations["getArtifact"].enabled);

    let cached_denied = service.handle(connector_request(
        "cached-denied",
        ConnectorDestination::Local,
        ConnectorOperation::GetCachedMoments {
            author_id: None,
            not_before_unix: None,
            not_after_unix: None,
            content_type: None,
            cursor: None,
            limit: Some(10),
        },
    ));
    assert!(!cached_denied.ok);

    let listed = service.handle(connector_request(
        "list",
        ConnectorDestination::Local,
        ConnectorOperation::ListConversations,
    ));
    let ConnectorResult::Conversations(listed) = listed.result.unwrap() else {
        panic!("unexpected connector result")
    };
    assert_eq!(listed.conversations.len(), 1);
    assert!(!listed.conversations[0].human_label.is_empty());

    let remote_denied = service.handle(connector_request(
        "remote-denied",
        ConnectorDestination::RemoteModel,
        ConnectorOperation::GetMessages {
            conversation_id: "conversation-a".to_string(),
            cursor: None,
            limit: Some(10),
        },
    ));
    assert!(!remote_denied.ok);

    let artifact_response = service.handle(connector_request(
        "artifact",
        ConnectorDestination::Local,
        ConnectorOperation::GetArtifact {
            conversation_id: "conversation-a".to_string(),
            artifact_id: "artifact-a".to_string(),
        },
    ));
    let ConnectorResult::Artifact(artifact) = artifact_response.result.unwrap() else {
        panic!("unexpected connector result")
    };
    assert_eq!(
        artifact
            .source
            .as_ref()
            .unwrap()
            .account_relative_path
            .as_deref(),
        Some("msg/image.jpg")
    );
    assert_eq!(
        artifact.decoded.as_ref().unwrap().sha256,
        hex::encode(Sha256::digest(b"decoded-image"))
    );
    let source_path = artifact.source.as_ref().unwrap().absolute_path.clone();
    let decoded_path = artifact.decoded.as_ref().unwrap().absolute_path.clone();
    assert!(Path::new(&source_path).is_file());
    assert!(Path::new(&decoded_path).is_file());

    fs::write(&decoded_path, b"altered-image").unwrap();
    let stale_artifact_denied = service.handle(connector_request(
        "stale-artifact-denied",
        ConnectorDestination::Local,
        ConnectorOperation::GetArtifact {
            conversation_id: "conversation-a".to_string(),
            artifact_id: "artifact-a".to_string(),
        },
    ));
    assert!(!stale_artifact_denied.ok);
    assert_eq!(
        stale_artifact_denied.error.unwrap().code,
        ConnectorErrorCode::IntegrityFailure
    );
    fs::write(&decoded_path, b"decoded-image").unwrap();

    let remote_artifact_denied = service.handle(connector_request(
        "remote-artifact-denied",
        ConnectorDestination::RemoteModel,
        ConnectorOperation::GetArtifact {
            conversation_id: "conversation-a".to_string(),
            artifact_id: "artifact-a".to_string(),
        },
    ));
    assert!(!remote_artifact_denied.ok);
    assert_eq!(
        remote_artifact_denied.error.unwrap().code,
        ConnectorErrorCode::Unauthorized
    );

    let draft_response = service.handle(connector_request(
        "draft",
        ConnectorDestination::Local,
        ConnectorOperation::CreateReplyDraft {
            conversation_id: "conversation-a".to_string(),
            reply_target_canonical_id: "message-a".to_string(),
            rendered_text: "immutable synthetic draft body".to_string(),
            attachment_ids: vec!["artifact-a".to_string()],
            expires_in_seconds: Some(60),
        },
    ));
    assert!(draft_response.ok);
    let ConnectorResult::Draft(receipt) = draft_response.result.unwrap() else {
        panic!("unexpected connector result")
    };
    assert_eq!(
        receipt.reply_target_canonical_id.as_deref(),
        Some("message-a")
    );
    assert_eq!(receipt.attachment_count, 1);
    let draft_path = drafts.join(format!("{}.json", receipt.draft_id));
    assert_eq!(file_mode(&draft_path), 0o600);

    let preview = service.handle(connector_request(
        "preview",
        ConnectorDestination::Local,
        ConnectorOperation::PreviewAction {
            draft_id: receipt.draft_id.clone(),
        },
    ));
    let ConnectorResult::Preview(preview) = preview.result.unwrap() else {
        panic!("unexpected connector result")
    };
    assert_eq!(
        preview.draft.rendered_text,
        "immutable synthetic draft body"
    );
    assert!(!preview.executable);
    assert!(!preview.expired);
    assert_eq!(
        preview.draft.attachments[0].sha256,
        hex::encode(Sha256::digest(b"decoded-image"))
    );

    let audit_bytes = fs::read(&audit).unwrap();
    assert!(!contains_bytes(
        &audit_bytes,
        b"immutable synthetic draft body"
    ));
    assert!(contains_bytes(&audit_bytes, receipt.draft_id.as_bytes()));

    let mut tampered: serde_json::Value =
        serde_json::from_slice(&fs::read(&draft_path).unwrap()).unwrap();
    tampered["renderedText"] = json!("tampered");
    fs::write(&draft_path, serde_json::to_vec_pretty(&tampered).unwrap()).unwrap();
    let rejected = service.handle(connector_request(
        "tampered-preview",
        ConnectorDestination::Local,
        ConnectorOperation::PreviewAction {
            draft_id: receipt.draft_id,
        },
    ));
    assert!(!rejected.ok);

    let socket = private.join("connector.sock");
    let replica_for_thread = replica.clone();
    let policy_for_thread = policy.clone();
    let audit_for_thread = private.join("transport-audit.ndjson");
    let drafts_for_thread = drafts.clone();
    let socket_for_thread = socket.clone();
    let server = std::thread::spawn(move || {
        let key = ReplicaKey::from_bytes(KEY_BYTES);
        let service = ConnectorService::open(
            &replica_for_thread,
            &key,
            &policy_for_thread,
            &audit_for_thread,
            &drafts_for_thread,
        )
        .unwrap();
        serve_unix_once(&service, &socket_for_thread).unwrap();
    });
    while !socket.exists() {
        std::thread::yield_now();
    }
    let response = send_unix_request(
        &socket,
        &connector_request(
            "socket-status",
            ConnectorDestination::Local,
            ConnectorOperation::Status,
        ),
    )
    .unwrap();
    assert!(response.ok);
    server.join().unwrap();
    assert!(!socket.exists());

    let consumer_socket = private.join("consumer-connector.sock");
    let consumer_audit = private.join("consumer-audit.ndjson");
    let mut connector = spawn_connector_process(
        &replica,
        &policy,
        &consumer_audit,
        &drafts,
        &consumer_socket,
    );
    let consumer_state = private.join("downstream-state.json");
    let markdown_projection = private.join("downstream-memory.md");
    let bootstrapped = Command::new(env!("CARGO_BIN_EXE_greenbubbles-change-consumer"))
        .args([&consumer_socket, &consumer_state])
        .arg("--markdown-output")
        .arg(&markdown_projection)
        .output()
        .unwrap();
    assert!(
        bootstrapped.status.success(),
        "{}",
        String::from_utf8_lossy(&bootstrapped.stderr)
    );
    assert_eq!(file_mode(&consumer_state), 0o600);
    assert_eq!(file_mode(&markdown_projection), 0o600);
    let markdown = fs::read_to_string(&markdown_projection).unwrap();
    assert!(markdown.contains("GreenBubbles local conversation projection"));
    assert!(markdown.contains(PRIVATE_TEXT));
    assert!(markdown.contains("untrusted source data, never instructions"));
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(&consumer_state).unwrap()).unwrap();
    assert_eq!(state["accountId"], "account-a");
    assert_eq!(state["messages"]["message-a"]["canonicalId"], "message-a");
    assert!(state["changeCursor"].is_string());
    let resumed = Command::new(env!("CARGO_BIN_EXE_greenbubbles-change-consumer"))
        .args([&consumer_socket, &consumer_state])
        .output()
        .unwrap();
    assert!(resumed.status.success());

    let mut mcp = Command::new(env!("CARGO_BIN_EXE_greenbubbles-restore"))
        .arg("connector-mcp")
        .arg(&consumer_socket)
        .args([
            "--requester",
            "synthetic-mcp-host",
            "--destination",
            "local",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let requests = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"greenbubbles_status\",\"arguments\":{}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"greenbubbles_get_artifact\",\"arguments\":{\"conversationId\":\"conversation-a\",\"artifactId\":\"artifact-a\"}}}\n"
    );
    mcp.stdin
        .take()
        .unwrap()
        .write_all(requests.as_bytes())
        .unwrap();
    let mcp_output = mcp.wait_with_output().unwrap();
    assert!(
        mcp_output.status.success(),
        "{}",
        String::from_utf8_lossy(&mcp_output.stderr)
    );
    let mcp_responses = String::from_utf8(mcp_output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(mcp_responses.len(), 4);
    assert!(mcp_responses[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "greenbubbles_get_changes"));
    assert_eq!(mcp_responses[2]["result"]["structuredContent"]["ok"], true);
    assert_eq!(
        mcp_responses[3]["result"]["structuredContent"]["result"]["kind"],
        "artifact"
    );
    assert_eq!(
        mcp_responses[3]["result"]["structuredContent"]["result"]["value"]["artifactId"],
        "artifact-a"
    );
    stop_connector_process(&mut connector, &consumer_socket);

    let replacement = private.join("replacement-replica.db");
    bootstrap_replica(&archive, &replacement, &key).unwrap();
    let replacement_socket = private.join("replacement-connector.sock");
    let mut replacement_connector = spawn_connector_process(
        &replacement,
        &policy,
        &private.join("replacement-audit.ndjson"),
        &drafts,
        &replacement_socket,
    );
    let before_replacement = fs::read(&consumer_state).unwrap();
    let rejected_replacement = Command::new(env!("CARGO_BIN_EXE_greenbubbles-change-consumer"))
        .args([&replacement_socket, &consumer_state])
        .output()
        .unwrap();
    assert!(!rejected_replacement.status.success());
    assert_eq!(fs::read(&consumer_state).unwrap(), before_replacement);
    let explicit_rebootstrap = Command::new(env!("CARGO_BIN_EXE_greenbubbles-change-consumer"))
        .args([&replacement_socket, &consumer_state])
        .arg("--rebootstrap")
        .output()
        .unwrap();
    assert!(
        explicit_rebootstrap.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit_rebootstrap.stderr)
    );
    stop_connector_process(&mut replacement_connector, &replacement_socket);
}

fn spawn_connector_process(
    replica: &Path,
    policy: &Path,
    audit: &Path,
    drafts: &Path,
    socket: &Path,
) -> Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_greenbubbles-restore"))
        .arg("connector-serve")
        .args([replica, policy, audit, drafts, socket])
        .arg("--replica-key-stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{}\n", "31".repeat(32)).as_bytes())
        .unwrap();
    for _ in 0..500 {
        if socket.exists() {
            return child;
        }
        if let Some(status) = child.try_wait().unwrap() {
            let mut detail = String::new();
            child
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut detail)
                .unwrap();
            panic!("connector exited before binding ({status}): {detail}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("connector did not bind its Unix socket");
}

fn stop_connector_process(child: &mut Child, socket: &Path) {
    child.kill().unwrap();
    child.wait().unwrap();
    if socket.exists() {
        fs::remove_file(socket).unwrap();
    }
}

fn connector_request(
    request_id: &str,
    destination: ConnectorDestination,
    operation: ConnectorOperation,
) -> ConnectorRequest {
    ConnectorRequest {
        api_version: CONNECTOR_API_VERSION.to_string(),
        request_id: request_id.to_string(),
        requester_id: "synthetic-test".to_string(),
        destination,
        operation,
    }
}

#[test]
fn bootstraps_account_isolated_encrypted_replica_and_retains_migration_backup() {
    let fixture = tempfile::tempdir().unwrap();
    let private = fixture.path().join("private");
    fs::create_dir(&private).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    let archive = build_archive(&private, "archive-a", "account-a", "source-a");
    let replica = private.join("replica.db");
    let key = ReplicaKey::from_bytes(KEY_BYTES);

    let first = bootstrap_replica(&archive, &replica, &key).unwrap();
    assert!(first.encrypted_at_rest);
    assert!(!first.idempotent);
    assert_eq!(first.schema_version, 4);
    assert_eq!(first.conversation_count, 1);
    assert_eq!(first.participant_count, 1);
    assert_eq!(first.message_count, 1);
    assert_eq!(first.artifact_count, 1);
    assert_eq!(first.cached_moment_count, 0);
    assert_eq!(first.cached_moment_interaction_count, 0);
    assert_eq!(first.message_artifact_count, 1);
    assert_eq!(file_mode(&replica), 0o600);

    let bytes = fs::read(&replica).unwrap();
    assert_ne!(&bytes[..16], b"SQLite format 3\0");
    assert!(!contains_bytes(&bytes, PRIVATE_TEXT.as_bytes()));
    let private_path = fs::canonicalize(archive.join("source-account/msg/image.jpg"))
        .unwrap()
        .display()
        .to_string();
    assert!(!contains_bytes(&bytes, private_path.as_bytes()));
    assert!(Connection::open(&replica)
        .unwrap()
        .query_row("SELECT count(*) FROM message", [], |_| Ok(()))
        .is_err());

    let status = replica_status(&replica, &key).unwrap();
    assert_eq!(status.account_id.as_deref(), Some("account-a"));
    assert_eq!(
        status.current_source_fingerprint.as_deref(),
        Some("source-a")
    );
    assert_eq!(status.message_count, 1);
    assert!(status.last_checkpoint_unix_nanoseconds.is_some());
    assert!(status.checkpoint_revision.is_some());
    assert_eq!(status.decoder_name.as_deref(), Some("synthetic"));
    assert_eq!(status.decoder_version.as_deref(), Some("1"));
    assert_eq!(
        status.media_phase,
        Some(greenbubbles_restore::RestorationMediaPhase::Resolved)
    );
    assert_eq!(status.last_sync_kind.as_deref(), Some("bootstrap"));
    assert!(status.last_sync_started_unix_nanoseconds.is_some());
    assert!(status.last_sync_duration_milliseconds.is_some());
    assert_eq!(
        status.client_build_compatibility.unwrap().state,
        ClientBuildCompatibilityState::LegacySyntheticFixture
    );
    assert!(replica_status(&replica, &ReplicaKey::from_bytes(WRONG_KEY_BYTES)).is_err());

    let repeated = bootstrap_replica(&archive, &replica, &key).unwrap();
    assert!(repeated.idempotent);
    assert_eq!(repeated.message_count, 1);

    let archive_b = clone_archive(&archive, &private, "archive-sync-b", "source-sync-b");
    let mut messages = read_ndjson::<CanonicalMessage>(&archive_b.join("messages.ndjson"));
    messages[0].typed_payload = TypedPayload::Decoded(json!({"Text": "edited searchable text"}));
    messages[0].content_base64 = Some("ZWRpdGVkIHNlYXJjaGFibGUgdGV4dA==".to_string());
    let mut added = messages[0].clone();
    added.canonical_id = "message-b".to_string();
    added.local_id = Some(2);
    added.server_id = Some(3);
    added.sort_sequence = Some(4);
    added.conversation_ordinal = 1;
    added.created_at_unix = Some(1_700_000_001);
    added.typed_payload = TypedPayload::Decoded(json!({"Text": "second retained message"}));
    added.content_base64 = Some("c2Vjb25kIHJldGFpbmVkIG1lc3NhZ2U=".to_string());
    messages.push(added);
    overwrite_ndjson(&archive_b.join("messages.ndjson"), &messages);
    let synchronized = synchronize_replica(&archive_b, &replica, &key).unwrap();
    assert_eq!(synchronized.previous_source_fingerprint, "source-a");
    assert_eq!(synchronized.current_source_fingerprint, "source-sync-b");
    assert_eq!(synchronized.added_count, 1);
    assert_eq!(synchronized.changed_count, 1);
    assert_eq!(synchronized.removed_count, 0);
    assert_eq!(synchronized.message_count, 2);

    let exact_filter = ReplicaMessageFilter {
        conversation_id: Some("conversation-a".to_string()),
        sender_id: Some("participant-a".to_string()),
        direction: Some(MessageDirection::Incoming),
        logical_type: Some(1),
        sub_type: Some(0),
        not_before_unix: Some(1_700_000_001),
        not_after_unix: Some(1_700_000_001),
        reply_target_canonical_id: None,
        has_attachment: Some(true),
        full_text_query: Some("second".to_string()),
    };
    let exact = search_replica_messages(&replica, &key, &exact_filter, None, 10).unwrap();
    assert_eq!(exact.items.len(), 1);
    assert_eq!(exact.items[0].canonical_id, "message-b");
    assert_eq!(
        get_replica_message(&replica, &key, "message-b")
            .unwrap()
            .unwrap()
            .canonical_id,
        "message-b"
    );
    assert!(get_replica_message(&replica, &key, "missing")
        .unwrap()
        .is_none());
    assert_eq!(
        list_replica_conversations(&replica, &key, 10)
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        replica_coverage(&replica, &key).unwrap().source_fingerprint,
        "source-sync-b"
    );
    let message_first =
        search_replica_messages(&replica, &key, &ReplicaMessageFilter::default(), None, 1).unwrap();
    assert_eq!(message_first.items.len(), 1);
    assert!(message_first.next_cursor.is_some());
    let message_second = search_replica_messages(
        &replica,
        &key,
        &ReplicaMessageFilter::default(),
        message_first.next_cursor.as_deref(),
        1,
    )
    .unwrap();
    assert_eq!(message_second.items.len(), 1);
    assert_ne!(
        message_first.items[0].canonical_id,
        message_second.items[0].canonical_id
    );
    assert!(search_replica_messages(
        &replica,
        &key,
        &exact_filter,
        message_first.next_cursor.as_deref(),
        10,
    )
    .is_err());
    let pre_sync_message_cursor = message_first.next_cursor.clone();

    let pre_upgrade_revision = replica_status(&replica, &key)
        .unwrap()
        .checkpoint_revision
        .unwrap();
    let archive_b_upgrade = clone_archive(
        &archive_b,
        &private,
        "archive-sync-b-upgrade",
        "source-sync-b",
    );
    let mut upgraded_coverage: RestorationCoverage =
        serde_json::from_slice(&fs::read(archive_b_upgrade.join("coverage.json")).unwrap())
            .unwrap();
    upgraded_coverage.decoder_version = "2".to_string();
    fs::write(
        archive_b_upgrade.join("coverage.json"),
        serde_json::to_vec_pretty(&upgraded_coverage).unwrap(),
    )
    .unwrap();
    let mut upgraded_artifacts =
        read_ndjson::<CanonicalArtifact>(&archive_b_upgrade.join("artifacts.ndjson"));
    upgraded_artifacts[0].decoded_sha256 = Some("c".repeat(64));
    overwrite_ndjson(
        &archive_b_upgrade.join("artifacts.ndjson"),
        &upgraded_artifacts,
    );
    let decoder_upgrade = synchronize_replica(&archive_b_upgrade, &replica, &key).unwrap();
    assert!(!decoder_upgrade.idempotent);
    assert_eq!(decoder_upgrade.previous_source_fingerprint, "source-sync-b");
    assert_eq!(decoder_upgrade.current_source_fingerprint, "source-sync-b");
    assert_eq!(decoder_upgrade.changed_count, 1);
    let upgraded_status = replica_status(&replica, &key).unwrap();
    assert_eq!(upgraded_status.decoder_version.as_deref(), Some("2"));
    assert_ne!(
        upgraded_status.checkpoint_revision.as_deref(),
        Some(pre_upgrade_revision.as_str())
    );
    assert!(search_replica_messages(
        &replica,
        &key,
        &ReplicaMessageFilter::default(),
        pre_sync_message_cursor.as_deref(),
        10,
    )
    .is_err());
    assert!(
        synchronize_replica(&archive_b_upgrade, &replica, &key)
            .unwrap()
            .idempotent
    );

    let first_changes = get_replica_changes(&replica, &key, None, 1).unwrap();
    assert_eq!(first_changes.items.len(), 1);
    assert!(first_changes.next_cursor.is_some());
    let resumed_changes =
        get_replica_changes(&replica, &key, first_changes.next_cursor.as_deref(), 100).unwrap();
    assert!(resumed_changes.items.len() >= 2);
    assert!(resumed_changes
        .items
        .windows(2)
        .all(|pair| pair[0].sequence < pair[1].sequence));
    assert!(resumed_changes.next_cursor.is_some());
    let drained_changes =
        get_replica_changes(&replica, &key, resumed_changes.next_cursor.as_deref(), 100).unwrap();
    assert!(drained_changes.items.is_empty());
    assert!(drained_changes.next_cursor.is_none());
    let second_replica = private.join("second-replica.db");
    bootstrap_replica(&archive, &second_replica, &key).unwrap();
    assert!(get_replica_changes(
        &second_replica,
        &key,
        first_changes.next_cursor.as_deref(),
        100,
    )
    .is_err());

    let archive_c = clone_archive(&archive_b, &private, "archive-sync-c", "source-sync-c");
    let mut malformed = fs::read(archive_c.join("messages.ndjson")).unwrap();
    malformed.extend_from_slice(b"{not-valid-json}\n");
    fs::write(archive_c.join("messages.ndjson"), malformed).unwrap();
    assert!(synchronize_replica(&archive_c, &replica, &key).is_err());
    let after_failed_sync = replica_status(&replica, &key).unwrap();
    assert_eq!(
        after_failed_sync.current_source_fingerprint.as_deref(),
        Some("source-sync-b")
    );
    assert_eq!(after_failed_sync.message_count, 2);

    let archive_d = clone_archive(&archive_b, &private, "archive-sync-d", "source-sync-d");
    let retained = read_ndjson::<CanonicalMessage>(&archive_d.join("messages.ndjson"))
        .into_iter()
        .filter(|message| message.canonical_id == "message-b")
        .collect::<Vec<_>>();
    overwrite_ndjson(&archive_d.join("messages.ndjson"), &retained);
    let mut artifacts = read_ndjson::<CanonicalArtifact>(&archive_d.join("artifacts.ndjson"));
    artifacts[0].availability = ArtifactAvailability::NotDownloaded;
    artifacts[0].source_local_path = None;
    artifacts[0].account_relative_path = None;
    artifacts[0].source_byte_count = None;
    artifacts[0].source_device_id = None;
    artifacts[0].source_file_id = None;
    artifacts[0].source_modified_seconds = None;
    artifacts[0].source_modified_nanoseconds = None;
    artifacts[0].source_sha256 = None;
    artifacts[0].detected_format = None;
    artifacts[0].decoded_local_path = None;
    artifacts[0].decoded_byte_count = None;
    artifacts[0].decoded_sha256 = None;
    artifacts[0].decoded_format = None;
    artifacts[0].decode_state = ArtifactDecodeState::NotRequired;
    overwrite_ndjson(&archive_d.join("artifacts.ndjson"), &artifacts);
    let deletion = synchronize_replica(&archive_d, &replica, &key).unwrap();
    assert_eq!(deletion.added_count, 0);
    assert_eq!(deletion.changed_count, 1);
    assert_eq!(deletion.removed_count, 1);
    assert_eq!(deletion.message_count, 1);
    assert!(search_replica_messages(
        &replica,
        &key,
        &ReplicaMessageFilter::default(),
        pre_sync_message_cursor.as_deref(),
        10,
    )
    .is_err());
    let idempotent_sync = synchronize_replica(&archive_d, &replica, &key).unwrap();
    assert!(idempotent_sync.idempotent);
    assert_eq!(idempotent_sync.added_count, 0);
    assert_eq!(idempotent_sync.changed_count, 0);
    assert_eq!(idempotent_sync.removed_count, 0);

    let integrity_archive = clone_archive(
        &archive_d,
        &private,
        "archive-integrity-scan",
        "source-integrity-scan",
    );
    let mut integrity_report: RestorationReport =
        serde_json::from_slice(&fs::read(integrity_archive.join("report.json")).unwrap()).unwrap();
    integrity_report.acquisition = Some(SnapshotAcquisitionEvidence {
        format_version: 1,
        mode: SnapshotAcquisitionMode::IntegrityScan,
        previous_source_fingerprint: Some("source-sync-d".to_string()),
        reconciliation_window_seconds: 0,
        changed_source_set_ids: Vec::new(),
        reconciliation_source_set_ids: Vec::new(),
        deleted_source_set_ids: Vec::new(),
        source_sets: Vec::new(),
        last_integrity_scan_at: None,
    });
    fs::write(
        integrity_archive.join("report.json"),
        serde_json::to_vec_pretty(&integrity_report).unwrap(),
    )
    .unwrap();
    synchronize_replica(&integrity_archive, &replica, &key).unwrap();
    let integrity_status = replica_status(&replica, &key).unwrap();
    assert_eq!(
        integrity_status.last_sync_kind.as_deref(),
        Some("integrityScan")
    );
    assert_eq!(
        integrity_status.acquisition_mode,
        Some(SnapshotAcquisitionMode::IntegrityScan)
    );
    assert!(integrity_status
        .last_integrity_scan_unix_nanoseconds
        .is_some());
    assert!(integrity_status.integrity_scan_age_seconds.is_some());

    let connection = keyed_connection(&replica);
    let retained_fts: i64 = connection
        .query_row(
            "SELECT count(*) FROM message_fts WHERE message_fts MATCH 'retained'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let deleted_fts: i64 = connection
        .query_row(
            "SELECT count(*) FROM message_fts WHERE message_fts MATCH 'edited'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained_fts, 1);
    assert_eq!(deleted_fts, 0);
    drop(connection);

    let other_archive = build_archive(&private, "archive-b", "account-b", "source-b");
    assert!(bootstrap_replica(&other_archive, &replica, &key).is_err());

    downgrade_to_schema_1(&replica);
    let migrated = replica_status(&replica, &key).unwrap();
    assert_eq!(migrated.schema_version, 4);
    let backup = fs::read_dir(&private)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("pre-migration-v1"))
        })
        .unwrap();
    assert_eq!(file_mode(&backup), 0o600);
    let backup_bytes = fs::read(&backup).unwrap();
    assert_ne!(&backup_bytes[..16], b"SQLite format 3\0");
    let backup_connection = keyed_connection(&backup);
    let backup_version: i64 = backup_connection
        .query_row(
            "SELECT schema_version FROM replica_schema WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(backup_version, 1);
}

fn build_archive(parent: &Path, name: &str, account: &str, fingerprint: &str) -> PathBuf {
    let archive = parent.join(name);
    fs::create_dir(&archive).unwrap();
    fs::set_permissions(&archive, fs::Permissions::from_mode(0o700)).unwrap();
    let source_directory = archive.join("source-account/msg");
    let derived_directory = archive.join("derived");
    fs::create_dir_all(&source_directory).unwrap();
    fs::create_dir(&derived_directory).unwrap();
    let source_path = source_directory.join("image.jpg");
    let decoded_path = derived_directory.join("image.jpg");
    write_private(&source_path, b"source-image");
    write_private(&decoded_path, b"decoded-image");
    let source_path = fs::canonicalize(source_path).unwrap();
    let decoded_path = fs::canonicalize(decoded_path).unwrap();
    let source_metadata = fs::metadata(&source_path).unwrap();
    let integrity = RestorationIntegrity {
        source_row_count: 1,
        restored_row_count: 1,
        conversation_count: 1,
        participant_count: 1,
        unique_artifact_count: 1,
        downloaded_artifact_count: 1,
        decoded_artifact_count: 1,
        ..Default::default()
    };
    let completion = RestorationCompletion::evaluate(&integrity);
    let report = RestorationReport {
        format_version: 2,
        account_id: account.to_string(),
        source_fingerprint: fingerprint.to_string(),
        client_build_compatibility: Default::default(),
        acquisition: None,
        archive_scope: Default::default(),
        media_phase: Default::default(),
        messages_path: archive.join("messages.ndjson").display().to_string(),
        rejections_path: archive.join("rejections.ndjson").display().to_string(),
        artifacts_path: archive.join("artifacts.ndjson").display().to_string(),
        conversations_path: archive.join("conversations.ndjson").display().to_string(),
        participants_path: archive.join("participants.ndjson").display().to_string(),
        cached_moments_path: None,
        cached_moment_interactions_path: None,
        cached_surfaces_path: None,
        coverage_path: archive.join("coverage.json").display().to_string(),
        report_path: archive.join("report.json").display().to_string(),
        integrity,
        completion,
    };
    write_json(&archive.join("report.json"), &report);
    write_json(
        &archive.join("coverage.json"),
        &RestorationCoverage {
            format_version: 2,
            decoder_name: "synthetic".to_string(),
            decoder_version: "1".to_string(),
            snapshot_manifest_format_version: 1,
            schema_profile_fingerprint: None,
            message_tables: Vec::new(),
            all_tables: Vec::new(),
            logical_type_counts: BTreeMap::new(),
            logical_sub_type_counts: BTreeMap::new(),
            unknown_payload_reason_counts: BTreeMap::new(),
            semantic_gap_reason_counts: BTreeMap::new(),
        },
    );
    let conversation = CanonicalConversation {
        conversation_id: "conversation-a".to_string(),
        account_id: account.to_string(),
        source_identifier_base64: "Y29udmVyc2F0aW9u".to_string(),
        kind: ConversationKind::Direct,
        participant_ids: vec!["participant-a".to_string()],
        memberships: Vec::new(),
        owner_participant_id: None,
        entity_decode_state: EntityDecodeState::Complete,
        source_records: Vec::new(),
    };
    let participant = CanonicalParticipant {
        participant_id: "participant-a".to_string(),
        account_id: account.to_string(),
        source_identifier_base64: "cGFydGljaXBhbnQ=".to_string(),
        alias_base64: None,
        remark_base64: None,
        nickname_base64: None,
        display_name_base64: None,
        local_profile_state: LocalProfileState::Hydrated,
        conversation_ids: vec!["conversation-a".to_string()],
        source_records: Vec::new(),
    };
    let artifact = CanonicalArtifact {
        artifact_id: "artifact-a".to_string(),
        kind: ArtifactKind::Image,
        role: ArtifactRole::Original,
        availability: ArtifactAvailability::Downloaded,
        source_md5: None,
        source_local_path: Some(source_path.display().to_string()),
        account_relative_path: Some("msg/image.jpg".to_string()),
        source_byte_count: Some(source_metadata.len()),
        source_device_id: Some(source_metadata.dev()),
        source_file_id: Some(source_metadata.ino()),
        source_modified_seconds: Some(source_metadata.mtime()),
        source_modified_nanoseconds: Some(source_metadata.mtime_nsec()),
        source_sha256: Some(hex::encode(Sha256::digest(b"source-image"))),
        detected_format: Some("jpeg".to_string()),
        materialized_local_path: None,
        decoded_local_path: Some(decoded_path.display().to_string()),
        decoded_byte_count: Some(13),
        decoded_sha256: Some(hex::encode(Sha256::digest(b"decoded-image"))),
        decoded_format: Some("jpeg".to_string()),
        decode_state: ArtifactDecodeState::Decoded,
        verification_detail: Some("synthetic source and derivative were verified".to_string()),
        source_resource_set_id: None,
        source_resource_logical_path: None,
        source_resource_table_id: None,
        source_resource_table_name: None,
        source_resource_row_id: None,
    };
    let message = CanonicalMessage {
        canonical_id: "message-a".to_string(),
        account_id: account.to_string(),
        source_set_id: "set-a".to_string(),
        source_logical_path: "private".to_string(),
        source_table_id: "table-a".to_string(),
        source_table_name: "message".to_string(),
        source_row_id: 1,
        conversation_id: "conversation-a".to_string(),
        conversation_source_identifier_base64: "Y29udmVyc2F0aW9u".to_string(),
        sender_id: Some("participant-a".to_string()),
        sender_source_identifier_base64: None,
        local_id: Some(1),
        server_id: Some(2),
        sort_sequence: Some(3),
        created_at_unix: Some(1_700_000_000),
        conversation_ordinal: 0,
        ordering_basis: MessageOrderingBasis::SortSequence,
        raw_type: Some(1),
        logical_type: Some(1),
        sub_type: Some(0),
        status: Some(2),
        direction: MessageDirection::Incoming,
        direction_evidence: DirectionEvidence::SenderMatchesConversation,
        content_base64: Some("ZW5jcnlwdGVkIHJlcGxpY2EgcHJpdmF0ZSB0ZXh0".to_string()),
        packed_info_base64: None,
        compression_type: None,
        raw_columns: BTreeMap::new(),
        typed_payload: TypedPayload::Decoded(json!({"Text": PRIVATE_TEXT})),
        semantic_decode_state: SemanticDecodeState::Complete,
        semantic_gap_reason: None,
        relationships: Vec::new(),
        artifact_references: vec![MessageArtifactReference {
            artifact_id: "artifact-a".to_string(),
            role: ArtifactRole::Original,
            preferred: true,
        }],
    };
    write_ndjson(&archive.join("conversations.ndjson"), &[conversation]);
    write_ndjson(&archive.join("participants.ndjson"), &[participant]);
    write_ndjson(&archive.join("artifacts.ndjson"), &[artifact]);
    write_ndjson(&archive.join("messages.ndjson"), &[message]);
    archive
}

fn downgrade_to_schema_1(path: &Path) {
    let connection = keyed_connection(path);
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP INDEX cached_moment_by_time;
             DROP INDEX cached_moment_by_author_time;
             DROP INDEX cached_moment_by_type_time;
             DROP INDEX cached_interaction_by_time;
             DROP TABLE cached_surface_state;
             DROP TABLE cached_moment_interaction;
             DROP TABLE cached_moment;
             DROP INDEX message_by_conversation_time;
             DROP INDEX message_by_sender;
             DROP INDEX message_by_type;
             DROP INDEX relationship_by_target;
             DROP INDEX change_by_account_sequence;
             DROP TABLE sync_seen;
             DROP TABLE replica_generation;
             DROP TABLE source_checkpoint;
             DROP TABLE sync_run;
             DROP TABLE change_log;
             DROP TABLE message_fts;
             DELETE FROM migration_history WHERE schema_version >= 2;
             UPDATE replica_schema SET schema_version = 1 WHERE singleton = 1;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
}

fn clone_archive(source: &Path, parent: &Path, name: &str, fingerprint: &str) -> PathBuf {
    let destination = parent.join(name);
    fs::create_dir(&destination).unwrap();
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).unwrap();
    for name in [
        "report.json",
        "coverage.json",
        "conversations.ndjson",
        "participants.ndjson",
        "messages.ndjson",
        "artifacts.ndjson",
    ] {
        let path = destination.join(name);
        fs::copy(source.join(name), &path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut report: RestorationReport =
        serde_json::from_slice(&fs::read(destination.join("report.json")).unwrap()).unwrap();
    report.source_fingerprint = fingerprint.to_string();
    fs::write(
        destination.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    destination
}

fn read_ndjson<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn overwrite_ndjson<T: Serialize>(path: &Path, values: &[T]) {
    let mut bytes = Vec::new();
    for value in values {
        serde_json::to_writer(&mut bytes, value).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn keyed_connection(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(&format!(
            "PRAGMA cipher_compatibility = 4; PRAGMA key = \"x'{}'\";",
            hex::encode(KEY_BYTES)
        ))
        .unwrap();
    connection
        .query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))
        .unwrap();
    connection
}

fn write_json(path: &Path, value: &impl Serialize) {
    write_private(path, &serde_json::to_vec_pretty(value).unwrap());
}

fn write_ndjson<T: Serialize>(path: &Path, values: &[T]) {
    let mut bytes = Vec::new();
    for value in values {
        serde_json::to_writer(&mut bytes, value).unwrap();
        bytes.push(b'\n');
    }
    write_private(path, &bytes);
}

fn write_private(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

fn file_mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
