use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use greenbubbles_restore::replica::{bootstrap_replica, replica_status};
use greenbubbles_restore::{
    ArtifactAvailability, ArtifactDecodeState, ArtifactKind, ArtifactRole, CanonicalArtifact,
    CanonicalConversation, CanonicalMessage, CanonicalParticipant, ConversationKind,
    DirectionEvidence, EntityDecodeState, LocalProfileState, MessageArtifactReference,
    MessageDirection, MessageOrderingBasis, ReplicaKey, RestorationCompletion, RestorationCoverage,
    RestorationIntegrity, RestorationReport, SemanticDecodeState, TypedPayload,
};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;

const KEY_BYTES: [u8; 32] = [0x31; 32];
const WRONG_KEY_BYTES: [u8; 32] = [0x32; 32];
const PRIVATE_TEXT: &str = "encrypted replica private text";
const PRIVATE_PATH: &str = "/synthetic/private/image.jpg";

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
    assert_eq!(first.schema_version, 2);
    assert_eq!(first.conversation_count, 1);
    assert_eq!(first.participant_count, 1);
    assert_eq!(first.message_count, 1);
    assert_eq!(first.artifact_count, 1);
    assert_eq!(first.message_artifact_count, 1);
    assert_eq!(file_mode(&replica), 0o600);

    let bytes = fs::read(&replica).unwrap();
    assert_ne!(&bytes[..16], b"SQLite format 3\0");
    assert!(!contains_bytes(&bytes, PRIVATE_TEXT.as_bytes()));
    assert!(!contains_bytes(&bytes, PRIVATE_PATH.as_bytes()));
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
    assert!(replica_status(&replica, &ReplicaKey::from_bytes(WRONG_KEY_BYTES)).is_err());

    let repeated = bootstrap_replica(&archive, &replica, &key).unwrap();
    assert!(repeated.idempotent);
    assert_eq!(repeated.message_count, 1);

    let other_archive = build_archive(&private, "archive-b", "account-b", "source-b");
    assert!(bootstrap_replica(&other_archive, &replica, &key).is_err());

    downgrade_to_schema_1(&replica);
    let migrated = replica_status(&replica, &key).unwrap();
    assert_eq!(migrated.schema_version, 2);
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
        messages_path: "private".to_string(),
        rejections_path: "private".to_string(),
        artifacts_path: "private".to_string(),
        conversations_path: "private".to_string(),
        participants_path: "private".to_string(),
        coverage_path: "private".to_string(),
        report_path: "private".to_string(),
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
        source_local_path: Some(PRIVATE_PATH.to_string()),
        account_relative_path: Some("msg/image.jpg".to_string()),
        source_byte_count: Some(12),
        source_device_id: Some(1),
        source_file_id: Some(2),
        source_modified_seconds: Some(3),
        source_modified_nanoseconds: Some(4),
        source_sha256: Some("source-hash".to_string()),
        detected_format: Some("jpeg".to_string()),
        materialized_local_path: None,
        decoded_local_path: Some("/synthetic/private/decoded.jpg".to_string()),
        decoded_byte_count: Some(12),
        decoded_sha256: Some("decoded-hash".to_string()),
        decoded_format: Some("jpeg".to_string()),
        decode_state: ArtifactDecodeState::Decoded,
        verification_detail: None,
        source_resource_set_id: None,
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
             DROP INDEX message_by_conversation_time;
             DROP INDEX message_by_sender;
             DROP INDEX message_by_type;
             DROP INDEX relationship_by_target;
             DROP INDEX change_by_account_sequence;
             DROP TABLE source_checkpoint;
             DROP TABLE sync_run;
             DROP TABLE change_log;
             DROP TABLE message_fts;
             DELETE FROM migration_history WHERE schema_version = 2;
             UPDATE replica_schema SET schema_version = 1 WHERE singleton = 1;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
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
