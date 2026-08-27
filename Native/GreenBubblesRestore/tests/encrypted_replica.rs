use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use greenbubbles_restore::replica::{
    bootstrap_replica, get_replica_changes, replica_status, synchronize_replica,
};
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
    assert_eq!(first.schema_version, 3);
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
    artifacts[0].decoded_local_path = None;
    artifacts[0].decode_state = ArtifactDecodeState::NotRequired;
    overwrite_ndjson(&archive_d.join("artifacts.ndjson"), &artifacts);
    let deletion = synchronize_replica(&archive_d, &replica, &key).unwrap();
    assert_eq!(deletion.added_count, 0);
    assert_eq!(deletion.changed_count, 1);
    assert_eq!(deletion.removed_count, 1);
    assert_eq!(deletion.message_count, 1);
    let idempotent_sync = synchronize_replica(&archive_d, &replica, &key).unwrap();
    assert!(idempotent_sync.idempotent);
    assert_eq!(idempotent_sync.added_count, 0);
    assert_eq!(idempotent_sync.changed_count, 0);
    assert_eq!(idempotent_sync.removed_count, 0);

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
    assert_eq!(migrated.schema_version, 3);
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
