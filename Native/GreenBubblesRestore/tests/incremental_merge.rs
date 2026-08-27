use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use greenbubbles_restore::merge::merge_incremental_archive;
use greenbubbles_restore::replica::bootstrap_replica;
use greenbubbles_restore::{
    ArtifactAvailability, ArtifactDecodeState, ArtifactKind, ArtifactRole,
    CachedSurfaceCompleteness, CachedSurfaceCoverage, CachedSurfaceTableCoverage,
    CachedSurfaceTableRole, CanonicalArtifact, CanonicalCachedMoment, CanonicalConversation,
    CanonicalMessage, CanonicalParticipant, ConversationKind, ConversationMembership,
    ConversationMembershipRole, DirectionEvidence, EntityDecodeState, LocalProfileState,
    MessageArtifactReference, MessageDirection, MessageOrderingBasis, MessageRelationship,
    MessageRelationshipKind, RawSQLiteValue, RelationshipResolutionState, ReplicaKey,
    RestorationArchiveScope, RestorationCompletion, RestorationCoverage, RestorationIntegrity,
    RestorationReport, SemanticDecodeState, SnapshotAcquisitionEvidence, SnapshotAcquisitionMode,
    SnapshotSourceSetInventory, TableCoverageRole, TableSchemaCoverage, TypedPayload,
};
use serde::Serialize;
use serde_json::json;
use sha2::Digest;

const ACCOUNT: &str = "synthetic-account";
const PREVIOUS_FINGERPRINT: &str =
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CURRENT_FINGERPRINT: &str =
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[test]
fn merges_selected_source_sets_reorders_globally_and_resolves_cross_shard_relationships() {
    let fixture = tempfile::tempdir().unwrap();
    let private = fixture.path().join("private");
    fs::create_dir(&private).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    let previous = build_archive(&private, "previous", false);
    let fragment = build_archive(&private, "fragment", true);
    let output = private.join("merged");
    let key = ReplicaKey::from_bytes([0x44; 32]);
    let rejected_replica = private.join("fragment-replica.db");
    assert!(bootstrap_replica(&fragment, &rejected_replica, &key).is_err());
    assert!(!rejected_replica.exists());

    let report = merge_incremental_archive(&previous, &fragment, &output).unwrap();
    assert_eq!(report.previous_source_fingerprint, PREVIOUS_FINGERPRINT);
    assert_eq!(report.current_source_fingerprint, CURRENT_FINGERPRINT);
    assert_eq!(report.message_count, 2);
    assert_eq!(
        fs::metadata(&output).unwrap().permissions().mode() & 0o777,
        0o700
    );

    let messages = read_ndjson::<CanonicalMessage>(&output.join("messages.ndjson"));
    assert_eq!(
        messages
            .iter()
            .map(|message| message.canonical_id.as_str())
            .collect::<Vec<_>>(),
        ["message-new-a", "message-b"]
    );
    assert_eq!(messages[0].conversation_ordinal, 0);
    assert_eq!(messages[1].conversation_ordinal, 1);
    assert_eq!(
        messages[0].relationships[0].target_canonical_id.as_deref(),
        Some("message-b")
    );
    assert_eq!(
        messages[0].relationships[0].resolution_state,
        RelationshipResolutionState::Resolved
    );

    let merged_report: RestorationReport = read_json(&output.join("report.json"));
    assert_eq!(
        merged_report.archive_scope,
        RestorationArchiveScope::Authoritative
    );
    assert!(merged_report.integrity.row_equation_holds());
    assert_eq!(merged_report.integrity.source_row_count, 2);
    assert_eq!(merged_report.integrity.restored_row_count, 2);
    assert_eq!(merged_report.integrity.cached_moment_count, 2);
    assert!(merged_report.cached_moments_path.is_some());
    let cached = read_ndjson::<CanonicalCachedMoment>(&output.join("cached-moments.ndjson"));
    assert_eq!(
        cached
            .iter()
            .map(|moment| moment.canonical_id.as_str())
            .collect::<Vec<_>>(),
        ["cached-b", "cached-new-a"]
    );
    let cached_coverage: CachedSurfaceCoverage = read_json(&output.join("cached-surfaces.json"));
    assert_eq!(cached_coverage.moment_count, 2);
    assert_eq!(cached_coverage.tables.len(), 2);

    let coverage: RestorationCoverage = read_json(&output.join("coverage.json"));
    assert_eq!(coverage.message_tables.len(), 2);
    assert!(coverage
        .message_tables
        .iter()
        .any(|table| table.source_set_id == "set-a" && table.source_row_count == 1));
    assert!(coverage
        .message_tables
        .iter()
        .any(|table| table.source_set_id == "set-b" && table.source_row_count == 1));
    let artifacts = read_ndjson::<CanonicalArtifact>(&output.join("artifacts.ndjson"));
    let relocated = PathBuf::from(artifacts[0].materialized_local_path.as_ref().unwrap());
    assert!(relocated.starts_with(&output));
    assert_eq!(fs::read(relocated).unwrap(), b"synthetic-media");

    let replica = private.join("merged-replica.db");
    let bootstrapped = bootstrap_replica(&output, &replica, &key).unwrap();
    assert_eq!(bootstrapped.message_count, 2);
    assert_eq!(bootstrapped.cached_moment_count, 2);
}

#[test]
fn rejects_a_fragment_with_the_wrong_authoritative_baseline() {
    let fixture = tempfile::tempdir().unwrap();
    let private = fixture.path().join("private");
    fs::create_dir(&private).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    let previous = build_archive(&private, "previous", false);
    let fragment = build_archive(&private, "fragment", true);
    let mut report: RestorationReport = read_json(&fragment.join("report.json"));
    report
        .acquisition
        .as_mut()
        .unwrap()
        .previous_source_fingerprint = Some("c".repeat(64));
    overwrite_json(&fragment.join("report.json"), &report);
    let output = private.join("must-not-exist");

    assert!(merge_incremental_archive(&previous, &fragment, &output).is_err());
    assert!(!output.exists());
}

fn build_archive(parent: &Path, name: &str, fragment: bool) -> PathBuf {
    let archive = parent.join(name);
    fs::create_dir(&archive).unwrap();
    fs::set_permissions(&archive, fs::Permissions::from_mode(0o700)).unwrap();
    let source_fingerprint = if fragment {
        CURRENT_FINGERPRINT
    } else {
        PREVIOUS_FINGERPRINT
    };
    let messages = if fragment {
        vec![message("message-new-a", "set-a", 1, 50, true)]
    } else {
        vec![
            message("message-old-a", "set-a", 1, 100, false),
            message("message-b", "set-b", 2, 75, false),
        ]
    };
    let cached_moments = if fragment {
        vec![cached_moment("cached-new-a", "set-a", 1)]
    } else {
        vec![
            cached_moment("cached-old-a", "set-a", 1),
            cached_moment("cached-b", "set-b", 2),
        ]
    };
    let integrity = RestorationIntegrity {
        database_count: if fragment { 1 } else { 2 },
        message_table_count: if fragment { 1 } else { 2 },
        source_row_count: messages.len() as u64,
        restored_row_count: messages.len() as u64,
        conversation_count: 1,
        participant_count: 1,
        cached_moment_count: cached_moments.len() as u64,
        ..Default::default()
    };
    let report = RestorationReport {
        format_version: 4,
        account_id: ACCOUNT.to_string(),
        source_fingerprint: source_fingerprint.to_string(),
        client_build_compatibility: Default::default(),
        acquisition: fragment.then(acquisition),
        archive_scope: if fragment {
            RestorationArchiveScope::IncrementalFragment
        } else {
            RestorationArchiveScope::Authoritative
        },
        media_phase: Default::default(),
        messages_path: "private".to_string(),
        rejections_path: "private".to_string(),
        artifacts_path: "private".to_string(),
        conversations_path: "private".to_string(),
        participants_path: "private".to_string(),
        cached_moments_path: Some("private".to_string()),
        cached_moment_interactions_path: Some("private".to_string()),
        cached_surfaces_path: Some("private".to_string()),
        coverage_path: "private".to_string(),
        report_path: "private".to_string(),
        completion: RestorationCompletion::evaluate(&integrity),
        integrity,
    };
    let source_sets = if fragment {
        vec!["set-a"]
    } else {
        vec!["set-a", "set-b"]
    };
    let message_tables = source_sets
        .iter()
        .map(|source_set| greenbubbles_restore::MessageTableCoverage {
            source_set_id: (*source_set).to_string(),
            source_logical_path: format!("message/{source_set}.db"),
            source_table_id: format!("table-{source_set}"),
            source_table_name: format!("Msg_{source_set}"),
            source_row_count: 1,
            columns: vec!["local_id".to_string(), "message_content".to_string()],
        })
        .collect::<Vec<_>>();
    let all_tables = message_tables
        .iter()
        .map(|table| TableSchemaCoverage {
            source_set_id: table.source_set_id.clone(),
            source_logical_path: table.source_logical_path.clone(),
            source_table_id: table.source_table_id.clone(),
            source_table_name: table.source_table_name.clone(),
            columns: table.columns.clone(),
            role: TableCoverageRole::Message,
            classification_reason: "synthetic".to_string(),
        })
        .collect();
    let coverage = RestorationCoverage {
        format_version: 2,
        decoder_name: "synthetic".to_string(),
        decoder_version: "1".to_string(),
        snapshot_manifest_format_version: if fragment { 3 } else { 2 },
        message_tables,
        all_tables,
        logical_type_counts: BTreeMap::from([("1".to_string(), messages.len() as u64)]),
        logical_sub_type_counts: BTreeMap::from([("1:0".to_string(), messages.len() as u64)]),
        unknown_payload_reason_counts: BTreeMap::new(),
        semantic_gap_reason_counts: BTreeMap::new(),
    };
    let cached_coverage = CachedSurfaceCoverage {
        format_version: 1,
        observed_at: if fragment {
            "2026-08-27T04:00:00Z"
        } else {
            "2026-08-27T03:00:00Z"
        }
        .to_string(),
        cache_completeness: CachedSurfaceCompleteness::PartialLocalCache,
        source_database_present: true,
        moment_count: cached_moments.len() as u64,
        interaction_count: 0,
        semantic_gap_count: 0,
        tables: source_sets
            .iter()
            .map(|source_set| CachedSurfaceTableCoverage {
                source_set_id: (*source_set).to_string(),
                source_logical_path: format!("sns/{source_set}.db"),
                source_table_id: format!("sns-table-{source_set}"),
                source_table_name: "SnsTimeLine".to_string(),
                columns: vec![
                    "tid".to_string(),
                    "user_name".to_string(),
                    "content".to_string(),
                ],
                source_row_count: 1,
                restored_row_count: 1,
                role: CachedSurfaceTableRole::MomentTimeline,
                classification_reason: "synthetic exact signature".to_string(),
            })
            .collect(),
    };
    let conversation = CanonicalConversation {
        conversation_id: "conversation-a".to_string(),
        account_id: ACCOUNT.to_string(),
        source_identifier_base64: "Y29udmVyc2F0aW9u".to_string(),
        kind: ConversationKind::Direct,
        participant_ids: vec!["participant-a".to_string()],
        memberships: vec![ConversationMembership {
            participant_id: "participant-a".to_string(),
            role: ConversationMembershipRole::DirectPeer,
            display_name_base64: None,
        }],
        owner_participant_id: None,
        entity_decode_state: EntityDecodeState::Complete,
        source_records: Vec::new(),
    };
    let participant = CanonicalParticipant {
        participant_id: "participant-a".to_string(),
        account_id: ACCOUNT.to_string(),
        source_identifier_base64: "cGFydGljaXBhbnQ=".to_string(),
        alias_base64: None,
        remark_base64: None,
        nickname_base64: None,
        display_name_base64: None,
        local_profile_state: LocalProfileState::Hydrated,
        conversation_ids: vec!["conversation-a".to_string()],
        source_records: Vec::new(),
    };
    let connector_media = archive.join("connector-media.bin");
    write_private(&connector_media, b"synthetic-media");
    let media_sha256 = hex::encode(sha2::Sha256::digest(b"synthetic-media"));
    let artifact = CanonicalArtifact {
        artifact_id: "artifact-a".to_string(),
        kind: ArtifactKind::Voice,
        role: ArtifactRole::VoicePayload,
        availability: ArtifactAvailability::MaterializedFromDatabase,
        source_md5: None,
        source_local_path: None,
        account_relative_path: None,
        source_byte_count: Some(15),
        source_device_id: None,
        source_file_id: None,
        source_modified_seconds: None,
        source_modified_nanoseconds: None,
        source_sha256: Some(media_sha256),
        detected_format: Some("silk".to_string()),
        materialized_local_path: Some(connector_media.display().to_string()),
        decoded_local_path: None,
        decoded_byte_count: None,
        decoded_sha256: None,
        decoded_format: None,
        decode_state: ArtifactDecodeState::NotRequired,
        verification_detail: None,
        source_resource_set_id: Some("set-a".to_string()),
        source_resource_row_id: Some(1),
    };
    write_json(&archive.join("report.json"), &report);
    write_json(&archive.join("coverage.json"), &coverage);
    write_ndjson(&archive.join("messages.ndjson"), &messages);
    write_ndjson(&archive.join("conversations.ndjson"), &[conversation]);
    write_ndjson(&archive.join("participants.ndjson"), &[participant]);
    write_ndjson(&archive.join("artifacts.ndjson"), &[artifact]);
    write_ndjson(&archive.join("cached-moments.ndjson"), &cached_moments);
    write_ndjson::<greenbubbles_restore::CanonicalCachedMomentInteraction>(
        &archive.join("cached-moment-interactions.ndjson"),
        &[],
    );
    write_json(&archive.join("cached-surfaces.json"), &cached_coverage);
    write_ndjson::<greenbubbles_restore::RejectedRow>(&archive.join("rejections.ndjson"), &[]);
    archive
}

fn acquisition() -> SnapshotAcquisitionEvidence {
    SnapshotAcquisitionEvidence {
        format_version: 1,
        mode: SnapshotAcquisitionMode::Incremental,
        previous_source_fingerprint: Some(PREVIOUS_FINGERPRINT.to_string()),
        reconciliation_window_seconds: 900,
        changed_source_set_ids: vec!["set-a".to_string()],
        reconciliation_source_set_ids: Vec::new(),
        deleted_source_set_ids: Vec::new(),
        source_sets: vec![
            SnapshotSourceSetInventory {
                source_set_id: "set-a".to_string(),
                logical_path: "message/set-a.db".to_string(),
                files: Vec::new(),
            },
            SnapshotSourceSetInventory {
                source_set_id: "set-b".to_string(),
                logical_path: "message/set-b.db".to_string(),
                files: Vec::new(),
            },
        ],
    }
}

fn message(
    canonical_id: &str,
    source_set_id: &str,
    source_row_id: i64,
    sort_sequence: i64,
    cross_shard_reply: bool,
) -> CanonicalMessage {
    CanonicalMessage {
        canonical_id: canonical_id.to_string(),
        account_id: ACCOUNT.to_string(),
        source_set_id: source_set_id.to_string(),
        source_logical_path: format!("message/{source_set_id}.db"),
        source_table_id: format!("table-{source_set_id}"),
        source_table_name: format!("Msg_{source_set_id}"),
        source_row_id,
        conversation_id: "conversation-a".to_string(),
        conversation_source_identifier_base64: "Y29udmVyc2F0aW9u".to_string(),
        sender_id: Some("participant-a".to_string()),
        sender_source_identifier_base64: None,
        local_id: Some(source_row_id),
        server_id: Some(if source_set_id == "set-b" { 200 } else { 100 }),
        sort_sequence: Some(sort_sequence),
        created_at_unix: Some(1_700_000_000 + source_row_id),
        conversation_ordinal: 0,
        ordering_basis: MessageOrderingBasis::SortSequence,
        raw_type: Some(1),
        logical_type: Some(1),
        sub_type: Some(0),
        status: Some(2),
        direction: MessageDirection::Incoming,
        direction_evidence: DirectionEvidence::SenderMatchesConversation,
        content_base64: Some("c3ludGhldGlj".to_string()),
        packed_info_base64: None,
        compression_type: None,
        raw_columns: BTreeMap::new(),
        typed_payload: TypedPayload::Decoded(json!({"Text": canonical_id})),
        semantic_decode_state: SemanticDecodeState::Complete,
        semantic_gap_reason: None,
        relationships: cross_shard_reply
            .then_some(MessageRelationship {
                kind: MessageRelationshipKind::Reply,
                target_canonical_id: None,
                target_server_id: Some(200),
                target_local_id: None,
                resolved: false,
                resolution_state: RelationshipResolutionState::Pending,
                raw_reference_base64: None,
            })
            .into_iter()
            .collect(),
        artifact_references: vec![MessageArtifactReference {
            artifact_id: "artifact-a".to_string(),
            role: ArtifactRole::VoicePayload,
            preferred: true,
        }],
    }
}

fn cached_moment(
    canonical_id: &str,
    source_set_id: &str,
    source_row_id: i64,
) -> CanonicalCachedMoment {
    CanonicalCachedMoment {
        canonical_id: canonical_id.to_string(),
        account_id: ACCOUNT.to_string(),
        source_set_id: source_set_id.to_string(),
        source_logical_path: format!("sns/{source_set_id}.db"),
        source_table_id: format!("sns-table-{source_set_id}"),
        source_table_name: "SnsTimeLine".to_string(),
        source_row_id,
        timeline_id: RawSQLiteValue::Integer(source_row_id),
        author_id: Some("cached-author".to_string()),
        author_source_identifier_base64: None,
        created_at_unix: Some(1_700_000_000 + source_row_id),
        content_type: Some(1),
        content_description_base64: Some("c3ludGhldGlj".to_string()),
        title_base64: None,
        description_base64: None,
        content_url_base64: None,
        media_count: 0,
        like_count: 0,
        comment_count: 0,
        raw_content_base64: Some("PHhtbC8+".to_string()),
        raw_pack_info_base64: None,
        raw_columns: BTreeMap::new(),
        semantic_decode_state: SemanticDecodeState::Complete,
        semantic_gap_reason: None,
        cache_completeness: CachedSurfaceCompleteness::PartialLocalCache,
        observed_at: "2026-08-27T03:00:00Z".to_string(),
    }
}

fn read_ndjson<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> T {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn write_json(path: &Path, value: &impl Serialize) {
    write_private(path, &serde_json::to_vec_pretty(value).unwrap());
}

fn overwrite_json(path: &Path, value: &impl Serialize) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
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
