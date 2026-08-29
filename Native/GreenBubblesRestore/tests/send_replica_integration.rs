//! Exercises the send adapter's replica-backed paths against a real
//! bootstrapped replica rather than a hand-built fixture.
//!
//! Two code paths only ever had their pure logic tested: creating an
//! attachment draft through the connector's own recipient resolution, and
//! deciding whether a sent message actually appears in the account's own data.
//! Both read a real encrypted replica, so both are exercised here end to end.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use greenbubbles_restore::action::ActionCapability;
use greenbubbles_restore::connector::{load_action_draft, ConnectorService, DraftAttachment};
use greenbubbles_restore::replica::bootstrap_replica;
use greenbubbles_restore::send_adapter::{observe_send_in_replica, SendMatchStrength};
use greenbubbles_restore::send_contract::{normalized_send_text_sha256, SendRolloutStage};
use greenbubbles_restore::send_outbox::{OutboxEntry, OutboxEntryState};
use greenbubbles_restore::tools::{
    create_tool_policy, ConversationToolScope, ToolCapability, ToolMessageField,
};
use greenbubbles_restore::{
    ArtifactAvailability, ArtifactDecodeState, ArtifactKind, ArtifactRole, CanonicalArtifact,
    CanonicalConversation, CanonicalMessage, CanonicalParticipant, ConversationKind,
    DirectionEvidence, EntityDecodeState, LocalProfileState, MessageArtifactReference,
    MessageDirection, MessageOrderingBasis, ReplicaKey, RestorationCompletion, RestorationCoverage,
    RestorationIntegrity, RestorationReport, SemanticDecodeState, TypedPayload,
};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};

const ACCOUNT: &str = "replica-account";
const CONVERSATION: &str = "conversation-a";
const SENT_TEXT: &str = "the adapter sent this line";
const SENT_FILE_NAME: &str = "quarterly.pdf";

fn write_private(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

fn write_json(path: &Path, value: &impl Serialize) {
    write_private(path, &serde_json::to_vec_pretty(value).unwrap());
}

fn write_ndjson(path: &Path, values: &[impl Serialize]) {
    let mut bytes = Vec::new();
    for value in values {
        bytes.extend_from_slice(&serde_json::to_vec(value).unwrap());
        bytes.push(b'\n');
    }
    write_private(path, &bytes);
}

/// One outgoing message, shaped the way the reconciler has to recognize it.
fn outgoing(
    canonical_id: &str,
    ordinal: u64,
    payload: serde_json::Value,
    artifacts: Vec<MessageArtifactReference>,
) -> CanonicalMessage {
    CanonicalMessage {
        canonical_id: canonical_id.to_string(),
        account_id: ACCOUNT.to_string(),
        source_set_id: "set-a".to_string(),
        source_logical_path: "private".to_string(),
        source_table_id: "table-a".to_string(),
        source_table_name: "message".to_string(),
        source_row_id: ordinal as i64 + 1,
        conversation_id: CONVERSATION.to_string(),
        conversation_source_identifier_base64: "Y29udmVyc2F0aW9u".to_string(),
        sender_id: Some("self-participant".to_string()),
        sender_source_identifier_base64: None,
        local_id: Some(ordinal as i64 + 1),
        server_id: Some(ordinal as i64 + 100),
        sort_sequence: Some(ordinal as i64 + 1),
        created_at_unix: Some(1_700_000_000 + ordinal as i64),
        conversation_ordinal: ordinal,
        ordering_basis: MessageOrderingBasis::SortSequence,
        raw_type: Some(1),
        logical_type: Some(1),
        sub_type: Some(0),
        status: Some(2),
        direction: MessageDirection::Outgoing,
        direction_evidence: DirectionEvidence::SenderMatchesConversation,
        content_base64: None,
        packed_info_base64: None,
        compression_type: None,
        raw_columns: BTreeMap::new(),
        typed_payload: TypedPayload::Decoded(payload),
        semantic_decode_state: SemanticDecodeState::Complete,
        semantic_gap_reason: None,
        relationships: Vec::new(),
        artifact_references: artifacts,
    }
}

/// Builds an archive holding the three outgoing shapes the reconciler must
/// tell apart: a text line, a named file, and a re-encoded image.
fn build_archive(parent: &Path) -> PathBuf {
    let archive = parent.join("archive");
    fs::create_dir(&archive).unwrap();
    fs::set_permissions(&archive, fs::Permissions::from_mode(0o700)).unwrap();
    let media = archive.join("source-account/msg");
    fs::create_dir_all(&media).unwrap();
    let source_path = media.join("image.jpg");
    write_private(&source_path, b"source-image");
    let source_path = fs::canonicalize(source_path).unwrap();
    let metadata = fs::metadata(&source_path).unwrap();

    let integrity = RestorationIntegrity {
        source_row_count: 3,
        restored_row_count: 3,
        conversation_count: 1,
        participant_count: 1,
        unique_artifact_count: 2,
        downloaded_artifact_count: 2,
        decoded_artifact_count: 2,
        artifact_reference_count: 2,
        ..Default::default()
    };
    let completion = RestorationCompletion::evaluate(&integrity);
    write_json(
        &archive.join("report.json"),
        &RestorationReport {
            format_version: 2,
            account_id: ACCOUNT.to_string(),
            self_participant_id: Some("self-participant".to_string()),
            account_binding_evidence: None,
            storage: None,
            source_fingerprint: "source-a".to_string(),
            client_build_compatibility: Default::default(),
            acquisition: None,
            archive_scope: Default::default(),
            database_coverage: None,
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
        },
    );
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
    write_ndjson(
        &archive.join("conversations.ndjson"),
        &[CanonicalConversation {
            conversation_id: CONVERSATION.to_string(),
            account_id: ACCOUNT.to_string(),
            source_identifier_base64: "Y29udmVyc2F0aW9u".to_string(),
            kind: ConversationKind::Direct,
            participant_ids: vec!["participant-a".to_string()],
            memberships: Vec::new(),
            owner_participant_id: None,
            entity_decode_state: EntityDecodeState::Complete,
            source_records: Vec::new(),
        }],
    );
    write_ndjson(
        &archive.join("participants.ndjson"),
        &[CanonicalParticipant {
            participant_id: "participant-a".to_string(),
            account_id: ACCOUNT.to_string(),
            source_identifier_base64: "cGFydGljaXBhbnQ=".to_string(),
            alias_base64: None,
            remark_base64: None,
            nickname_base64: None,
            display_name_base64: None,
            local_profile_state: LocalProfileState::Hydrated,
            conversation_ids: vec![CONVERSATION.to_string()],
            source_records: Vec::new(),
        }],
    );
    let artifact = |identifier: &str| CanonicalArtifact {
        artifact_id: identifier.to_string(),
        kind: ArtifactKind::Image,
        role: ArtifactRole::Original,
        roles: BTreeSet::from([ArtifactRole::Original]),
        availability: ArtifactAvailability::Downloaded,
        source_md5: None,
        source_local_path: Some(source_path.display().to_string()),
        account_relative_path: Some("msg/image.jpg".to_string()),
        source_byte_count: Some(metadata.len()),
        source_device_id: Some(metadata.dev()),
        source_file_id: Some(metadata.ino()),
        source_modified_seconds: Some(metadata.mtime()),
        source_modified_nanoseconds: Some(metadata.mtime_nsec()),
        source_sha256: Some(hex::encode(Sha256::digest(b"source-image"))),
        detected_format: Some("jpeg".to_string()),
        materialized_local_path: None,
        decoded_local_path: None,
        decoded_byte_count: None,
        decoded_sha256: None,
        decoded_format: None,
        decode_state: ArtifactDecodeState::Decoded,
        verification_detail: None,
        source_resource_set_id: None,
        source_resource_logical_path: None,
        source_resource_table_id: None,
        source_resource_table_name: None,
        source_resource_row_id: None,
    };
    write_ndjson(
        &archive.join("artifacts.ndjson"),
        &[artifact("artifact-file"), artifact("artifact-image")],
    );
    let reference = |identifier: &str| {
        vec![MessageArtifactReference {
            artifact_id: identifier.to_string(),
            role: ArtifactRole::Original,
            preferred: true,
        }]
    };
    write_ndjson(
        &archive.join("messages.ndjson"),
        &[
            outgoing("message-text", 0, json!({ "Text": SENT_TEXT }), Vec::new()),
            outgoing(
                "message-file",
                1,
                json!({ "File": { "title": SENT_FILE_NAME, "file_ext": "pdf" } }),
                reference("artifact-file"),
            ),
            // A re-encoded image: an artifact reference, and nothing nameable.
            outgoing(
                "message-image",
                2,
                json!({ "Image": { "sub_type": 0 } }),
                reference("artifact-image"),
            ),
        ],
    );
    archive
}

fn entry(capability: ActionCapability, body: &str, file_name: Option<&str>) -> OutboxEntry {
    let sha = |value: char| -> String { std::iter::repeat_n(value, 64).collect() };
    OutboxEntry {
        action_id: sha('1'),
        draft_id: sha('2'),
        approval_id: sha('3'),
        idempotency_key: sha('4'),
        capability_id: sha('5'),
        capability_binding_sha256: sha('6'),
        account_id: ACCOUNT.to_string(),
        conversation_id: CONVERSATION.to_string(),
        body_sha256: hex::encode(Sha256::digest(body.as_bytes())),
        normalized_body_sha256: normalized_send_text_sha256(body),
        capability,
        attachment_sha256: file_name.map(|_| sha('7')),
        display_file_name: file_name.map(str::to_string),
        staging_directory: file_name.map(|_| "/tmp/staging/aa".to_string()),
        bytes_preserved_in_transit: capability != ActionCapability::ImageSend,
        rollout_stage: SendRolloutStage::SelfSend,
        permit_send: true,
        state: OutboxEntryState::AwaitingReconciliation,
        reserved_at_unix_nanoseconds: 1_600_000_000_000_000_000,
        attempted_at_unix_nanoseconds: Some(1_600_000_000_000_000_000),
        deadline_unix_nanoseconds: 1_900_000_000_000_000_000,
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    replica: PathBuf,
    key: ReplicaKey,
    private: PathBuf,
    drafts: PathBuf,
    policy: PathBuf,
    audit: PathBuf,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let private = root.path().join("private");
    let drafts = private.join("drafts");
    fs::create_dir(&private).unwrap();
    fs::create_dir(&drafts).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&drafts, fs::Permissions::from_mode(0o700)).unwrap();
    let archive = build_archive(&private);
    let replica = private.join("replica.db");
    let key = ReplicaKey::from_bytes([0x51; 32]);
    bootstrap_replica(&archive, &replica, &key).unwrap();
    let policy = private.join("policy.json");
    create_tool_policy(
        &archive,
        &policy,
        BTreeMap::from([(
            CONVERSATION.to_string(),
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
    Fixture {
        _root: root,
        replica,
        key,
        audit: private.join("connector-audit.ndjson"),
        private,
        drafts,
        policy,
    }
}

#[test]
fn an_owner_attachment_draft_is_created_from_the_real_replica_and_reloads_intact() {
    let fixture = fixture();
    let service = ConnectorService::open(
        &fixture.replica,
        &fixture.key,
        &fixture.policy,
        &fixture.audit,
        &fixture.drafts,
    )
    .unwrap();
    let contents = b"quarterly numbers".to_vec();
    let attachment = DraftAttachment {
        artifact_id: hex::encode(Sha256::digest(b"quarterly.pdf")),
        kind: ArtifactKind::Document,
        role: ArtifactRole::FilePayload,
        digest_kind: "sourceSha256".to_string(),
        sha256: hex::encode(Sha256::digest(&contents)),
        byte_count: Some(contents.len() as u64),
        display_file_name: SENT_FILE_NAME.to_string(),
    };
    let draft = service
        .create_owner_attachment_draft(
            CONVERSATION,
            attachment,
            ActionCapability::FileSend,
            "local-owner",
            3_600,
        )
        .unwrap();

    // The recipient evidence comes from the connector's own resolution, so the
    // title the send gate compares against is produced by the same code path a
    // text draft uses.
    assert_eq!(draft.conversation_id, CONVERSATION);
    assert!(!draft.recipient.human_label.is_empty());
    assert_eq!(draft.attachment_intent, Some(ActionCapability::FileSend));
    assert_eq!(draft.attachments.len(), 1);
    assert!(draft.rendered_text.is_empty());

    // It survives the same owner-only load the send adapter performs.
    let path = fixture.drafts.join(format!("{}.json", draft.draft_id));
    let reloaded = load_action_draft(&path).unwrap();
    assert_eq!(reloaded.draft_id, draft.draft_id);
    assert_eq!(reloaded.attachment_intent, Some(ActionCapability::FileSend));
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
}

#[test]
fn an_attachment_draft_is_refused_for_a_conversation_the_replica_does_not_hold() {
    let fixture = fixture();
    let service = ConnectorService::open(
        &fixture.replica,
        &fixture.key,
        &fixture.policy,
        &fixture.audit,
        &fixture.drafts,
    )
    .unwrap();
    let attachment = DraftAttachment {
        artifact_id: hex::encode(Sha256::digest(b"x")),
        kind: ArtifactKind::Document,
        role: ArtifactRole::FilePayload,
        digest_kind: "sourceSha256".to_string(),
        sha256: hex::encode(Sha256::digest(b"x")),
        byte_count: Some(1),
        display_file_name: "note.txt".to_string(),
    };
    assert!(service
        .create_owner_attachment_draft(
            "conversation-that-does-not-exist",
            attachment.clone(),
            ActionCapability::FileSend,
            "local-owner",
            3_600,
        )
        .is_err());
    // A text capability is not an attachment intent, whatever the caller says.
    assert!(service
        .create_owner_attachment_draft(
            CONVERSATION,
            attachment,
            ActionCapability::TextSend,
            "local-owner",
            3_600,
        )
        .is_err());
}

#[test]
fn a_text_send_is_reconciled_against_the_replica_by_its_body_digest() {
    let fixture = fixture();
    let observation = observe_send_in_replica(
        &fixture.replica,
        &fixture.key,
        &entry(ActionCapability::TextSend, SENT_TEXT, None),
        86_400,
        1_700_000_100_000_000_000,
    )
    .unwrap();
    assert!(observation.outgoing_message_found);
    assert!(observation.normalized_body_matched);
    assert_eq!(observation.match_strength, SendMatchStrength::BodyDigest);
    assert_eq!(observation.canonical_id.as_deref(), Some("message-text"));
    assert_eq!(observation.account_id, ACCOUNT);
}

#[test]
fn a_body_the_replica_does_not_hold_is_never_matched() {
    let fixture = fixture();
    let observation = observe_send_in_replica(
        &fixture.replica,
        &fixture.key,
        &entry(ActionCapability::TextSend, "a line nobody sent", None),
        86_400,
        1_700_000_100_000_000_000,
    )
    .unwrap();
    assert!(!observation.normalized_body_matched);
    assert_eq!(observation.match_strength, SendMatchStrength::None);
    assert!(observation.canonical_id.is_none());
}

#[test]
fn a_file_send_is_reconciled_by_the_name_the_replica_recorded() {
    let fixture = fixture();
    let observation = observe_send_in_replica(
        &fixture.replica,
        &fixture.key,
        &entry(ActionCapability::FileSend, "", Some(SENT_FILE_NAME)),
        86_400,
        1_700_000_100_000_000_000,
    )
    .unwrap();
    assert!(observation.attachment_reference_found);
    assert!(observation.display_file_name_matched);
    assert_eq!(
        observation.match_strength,
        SendMatchStrength::AttachmentFileName
    );
    assert_eq!(observation.canonical_id.as_deref(), Some("message-file"));
}

#[test]
fn an_image_send_falls_back_to_presence_because_its_name_did_not_survive() {
    let fixture = fixture();
    let observation = observe_send_in_replica(
        &fixture.replica,
        &fixture.key,
        // The client re-encoded it, so the approved name appears nowhere.
        &entry(ActionCapability::ImageSend, "", Some("holiday.png")),
        86_400,
        1_700_000_100_000_000_000,
    )
    .unwrap();
    assert!(observation.attachment_reference_found);
    assert!(!observation.display_file_name_matched);
    assert_eq!(
        observation.match_strength,
        SendMatchStrength::AttachmentPresenceOnly
    );
    assert!(observation.canonical_id.is_some());
}

#[test]
fn a_file_send_never_settles_on_presence_alone() {
    let fixture = fixture();
    let observation = observe_send_in_replica(
        &fixture.replica,
        &fixture.key,
        // A file send whose name is nowhere in the replica must not be matched
        // by the mere presence of some other outgoing attachment.
        &entry(ActionCapability::FileSend, "", Some("nowhere.pdf")),
        86_400,
        1_700_000_100_000_000_000,
    )
    .unwrap();
    assert!(observation.attachment_reference_found);
    assert!(!observation.display_file_name_matched);
    assert_eq!(observation.match_strength, SendMatchStrength::None);
}

#[test]
fn a_replica_for_another_account_is_refused() {
    let fixture = fixture();
    let mut foreign = entry(ActionCapability::TextSend, SENT_TEXT, None);
    foreign.account_id = "someone-elses-account".to_string();
    assert!(observe_send_in_replica(
        &fixture.replica,
        &fixture.key,
        &foreign,
        86_400,
        1_700_000_100_000_000_000,
    )
    .is_err());
    let _ = &fixture.private;
}
