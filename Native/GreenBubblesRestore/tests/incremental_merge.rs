use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use greenbubbles_restore::merge::merge_incremental_archive;
use greenbubbles_restore::replica::{
    bootstrap_replica, bootstrap_replica_with_progress, synchronize_replica_with_progress,
};
use greenbubbles_restore::tools::{
    create_tool_policy, ConversationToolScope, LocalToolService, ToolCapability,
    ToolDataDestination, ToolMessageField, ToolSourceDatabaseFreshness,
};
use greenbubbles_restore::{
    audit::audit_archive, ArtifactAvailability, ArtifactDecodeState, ArtifactKind, ArtifactRole,
    CachedSurfaceCompleteness, CachedSurfaceCoverage, CachedSurfaceTableCoverage,
    CachedSurfaceTableRole, CanonicalArtifact, CanonicalCachedMoment, CanonicalConversation,
    CanonicalMessage, CanonicalParticipant, ConversationKind, ConversationMembership,
    ConversationMembershipRole, DirectionEvidence, EntityDecodeState, LocalProfileState,
    MessageArtifactReference, MessageDirection, MessageOrderingBasis, MessageRelationship,
    MessageRelationshipKind, ProgressEvent, ProgressObserver, ProgressPhase, RawSQLiteValue,
    RelationshipResolutionState, ReplicaKey, RestorationArchiveScope, RestorationCompletion,
    RestorationCoverage, RestorationDatabaseCoverage, RestorationIntegrity, RestorationReport,
    RestorationUnavailableDatabase, SemanticDecodeState, SnapshotAcquisitionEvidence,
    SnapshotAcquisitionMode, SnapshotSourceSetInventory, TableCoverageRole, TableSchemaCoverage,
    TypedPayload,
};
use serde::Serialize;
use serde_json::json;
use sha2::Digest;

const ACCOUNT: &str = "synthetic-account";
const PREVIOUS_FINGERPRINT: &str =
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CURRENT_FINGERPRINT: &str =
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[derive(Default)]
struct CapturedProgress(Mutex<Vec<ProgressEvent>>);

impl ProgressObserver for CapturedProgress {
    fn observe(&self, event: ProgressEvent) {
        self.0.lock().unwrap().push(event);
    }
}

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
    let fragment_error = bootstrap_replica(&fragment, &rejected_replica, &key).unwrap_err();
    assert!(fragment_error
        .to_string()
        .contains("archive scope is incrementalFragment"));
    assert!(!rejected_replica.exists());

    let diagnostic = build_archive(&private, "diagnostic", true);
    let mut diagnostic_report: RestorationReport = read_json(&diagnostic.join("report.json"));
    diagnostic_report.archive_scope = RestorationArchiveScope::DiagnosticSubset;
    overwrite_json(&diagnostic.join("report.json"), &diagnostic_report);
    let diagnostic_replica = private.join("diagnostic-replica.db");
    let diagnostic_error = bootstrap_replica(&diagnostic, &diagnostic_replica, &key).unwrap_err();
    assert!(diagnostic_error
        .to_string()
        .contains("archive scope is diagnosticSubset"));
    assert!(!diagnostic_replica.exists());

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
    let mut expected_cached_ids = vec![cached_id("set-a", 1), cached_id("set-b", 2)];
    expected_cached_ids.sort();
    assert_eq!(
        cached
            .iter()
            .map(|moment| moment.canonical_id.clone())
            .collect::<Vec<_>>(),
        expected_cached_ids
    );
    let cached_coverage: CachedSurfaceCoverage = read_json(&output.join("cached-surfaces.json"));
    assert_eq!(cached_coverage.moment_count, 2);
    assert_eq!(cached_coverage.tables.len(), 2);
    assert!(cached_coverage.schema_profile_fingerprint.is_some());

    let coverage: RestorationCoverage = read_json(&output.join("coverage.json"));
    assert_eq!(coverage.message_tables.len(), 2);
    assert!(coverage.schema_profile_fingerprint.is_some());
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
    assert!(relocated.starts_with(fs::canonicalize(&output).unwrap()));
    assert_eq!(fs::read(relocated).unwrap(), b"synthetic-media");

    let audit = audit_archive(&output).unwrap();
    assert_eq!(audit.format_version, 2);
    assert_eq!(audit.archive_format_version, 5);
    assert_eq!(audit.message_count, 2);
    assert_eq!(audit.cached_moment_count, 2);
    assert!(audit.all_recorded_artifact_files_match);
    assert!(audit.completion_evidence.source_scope_authoritative);
    assert!(Path::new(&merged_report.messages_path).is_absolute());
    assert!(Path::new(&merged_report.report_path).is_absolute());

    let coverage_path = output.join("coverage.json");
    let original_coverage = fs::read(&coverage_path).unwrap();
    let mut shifted_coverage = coverage.clone();
    shifted_coverage.message_tables[0].source_row_count = 2;
    shifted_coverage.message_tables[1].source_row_count = 0;
    overwrite_json(&coverage_path, &shifted_coverage);
    assert!(audit_archive(&output)
        .unwrap_err()
        .to_string()
        .contains("per-table source row accounting"));
    fs::write(&coverage_path, original_coverage).unwrap();
    assert!(audit_archive(&output).is_ok());

    let artifacts_path = output.join("artifacts.ndjson");
    let original_artifacts = fs::read(&artifacts_path).unwrap();
    let mut unauditable_artifacts = artifacts.clone();
    unauditable_artifacts[0].verification_detail = None;
    let mut unauditable_bytes = serde_json::to_vec(&unauditable_artifacts[0]).unwrap();
    unauditable_bytes.push(b'\n');
    fs::write(&artifacts_path, unauditable_bytes).unwrap();
    let rejected_replica = private.join("unaudited-replica.db");
    assert!(bootstrap_replica(&output, &rejected_replica, &key).is_err());
    assert!(!rejected_replica.exists());
    fs::write(&artifacts_path, original_artifacts).unwrap();

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

#[test]
fn unavailable_incremental_database_preserves_prior_records_and_still_synchronizes() {
    let fixture = tempfile::tempdir().unwrap();
    let private = fixture.path().join("private");
    fs::create_dir(&private).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    let previous = build_archive(&private, "previous-partial", false);
    let fragment = build_archive(&private, "fragment-partial", true);
    let mut fragment_report: RestorationReport = read_json(&fragment.join("report.json"));
    fragment_report
        .acquisition
        .as_mut()
        .unwrap()
        .changed_source_set_ids = vec!["set-a".to_string(), "set-b".to_string()];
    fragment_report.database_coverage = Some(RestorationDatabaseCoverage {
        format_version: 1,
        total_database_count: 2,
        attempted_database_count: 2,
        restored_database_count: 1,
        unavailable_database_count: 1,
        preserved_stale_database_count: 0,
        authoritative_database_coverage: false,
        snapshot_source_set_ids: vec!["set-a".to_string(), "set-b".to_string()],
        attempted_source_set_ids: vec!["set-a".to_string(), "set-b".to_string()],
        fresh_source_set_ids: vec!["set-a".to_string()],
        unavailable_source_set_ids: vec!["set-b".to_string()],
        preserved_stale_source_set_ids: Vec::new(),
        unavailable_databases: vec![RestorationUnavailableDatabase {
            source_set_id: "set-b".to_string(),
            logical_path: "message/set-b.db".to_string(),
            storage_family: "wcdbSqlcipher4".to_string(),
            database_byte_count: 4096,
            write_ahead_log_byte_count: 0,
            reason: "noExportedKeyAuthenticated".to_string(),
        }],
    });
    overwrite_json(&fragment.join("report.json"), &fragment_report);
    let output = private.join("merged-partial");
    merge_incremental_archive(&previous, &fragment, &output).unwrap();
    let report: RestorationReport = read_json(&output.join("report.json"));
    assert_eq!(
        report.archive_scope,
        RestorationArchiveScope::PartialDatabaseCoverage
    );
    let database_coverage = report.database_coverage.as_ref().unwrap();
    assert_eq!(database_coverage.fresh_source_set_ids, ["set-a"]);
    assert_eq!(database_coverage.unavailable_source_set_ids, ["set-b"]);
    assert_eq!(database_coverage.preserved_stale_source_set_ids, ["set-b"]);
    assert_eq!(
        database_coverage.unavailable_databases[0].reason,
        "noExportedKeyAuthenticated"
    );
    assert!(report.replica_mutation_eligible());
    let messages = read_ndjson::<CanonicalMessage>(&output.join("messages.ndjson"));
    assert!(messages
        .iter()
        .any(|message| message.canonical_id == "message-new-a"));
    assert!(messages
        .iter()
        .any(|message| message.canonical_id == "message-b"));
    let audit = audit_archive(&output).unwrap();
    assert!(!audit.authoritative_database_coverage);
    assert_eq!(audit.unavailable_database_count, 1);
    assert_eq!(audit.preserved_stale_database_count, 1);

    let key = ReplicaKey::from_bytes([0x45; 32]);
    let replica = private.join("partial-replica.db");
    let bootstrap_progress = CapturedProgress::default();
    let bootstrapped =
        bootstrap_replica_with_progress(&output, &replica, &key, &bootstrap_progress).unwrap();
    assert!(!bootstrapped.authoritative_database_coverage);
    assert_eq!(bootstrapped.unavailable_database_count, 1);
    assert_eq!(bootstrapped.preserved_stale_database_count, 1);
    let application_events = bootstrap_progress
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.phase == ProgressPhase::ReplicaApplication)
        .cloned()
        .collect::<Vec<_>>();
    assert!(!application_events.is_empty());
    assert!(application_events.iter().all(|event| {
        event.database_count == Some(2)
            && event.available_database_count == Some(1)
            && event.unavailable_database_count == Some(1)
    }));
    let sync_progress = CapturedProgress::default();
    let synchronized =
        synchronize_replica_with_progress(&output, &replica, &key, &sync_progress).unwrap();
    assert_eq!(synchronized.message_count, 2);
    assert!(synchronized.idempotent);
    assert!(!synchronized.authoritative_database_coverage);
    assert_eq!(synchronized.unavailable_database_count, 1);
    let sync_events = sync_progress.0.lock().unwrap();
    let final_application = sync_events
        .iter()
        .rev()
        .find(|event| event.phase == ProgressPhase::ReplicaApplication)
        .unwrap();
    assert_eq!(final_application.unavailable_database_count, Some(1));
    assert_eq!(
        final_application.phase_completed,
        final_application.phase_total
    );

    let conversation_id = scoped_id(ACCOUNT, b"conversation");
    let policy = private.join("partial-policy.json");
    create_tool_policy(
        &output,
        &policy,
        BTreeMap::from([(
            conversation_id.clone(),
            ConversationToolScope {
                capabilities: BTreeSet::from([ToolCapability::ReadRecentMessages]),
                message_fields: BTreeSet::from([ToolMessageField::Content]),
                not_before_unix: None,
                not_after_unix: None,
                allow_remote_model: false,
            },
        )]),
        10,
        4_096,
        4_096,
    )
    .unwrap();
    let service = LocalToolService::open(
        &output,
        &policy,
        &private.join("partial-tool-audit.ndjson"),
        "partial-test",
    )
    .unwrap();
    let minimized = service
        .read_recent_messages(&conversation_id, 10, ToolDataDestination::LocalModel)
        .unwrap()
        .messages;
    assert_eq!(minimized.len(), 2);
    assert_eq!(
        minimized
            .iter()
            .find(|message| message.canonical_id == "message-new-a")
            .unwrap()
            .source_database_freshness,
        ToolSourceDatabaseFreshness::Fresh
    );
    assert_eq!(
        minimized
            .iter()
            .find(|message| message.canonical_id == "message-b")
            .unwrap()
            .source_database_freshness,
        ToolSourceDatabaseFreshness::PreservedStale
    );
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
        vec![cached_moment("set-a", 1)]
    } else {
        vec![cached_moment("set-a", 1), cached_moment("set-b", 2)]
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
    let mut completion = RestorationCompletion::evaluate(&integrity);
    completion.full_restoration_achieved = false;
    let report = RestorationReport {
        format_version: 5,
        account_id: ACCOUNT.to_string(),
        self_participant_id: None,
        account_binding_evidence: None,
        storage: None,
        source_fingerprint: source_fingerprint.to_string(),
        client_build_compatibility: Default::default(),
        acquisition: fragment.then(acquisition),
        archive_scope: if fragment {
            RestorationArchiveScope::IncrementalFragment
        } else {
            RestorationArchiveScope::Authoritative
        },
        database_coverage: Some(database_coverage(fragment)),
        media_phase: Default::default(),
        messages_path: archive.join("messages.ndjson").display().to_string(),
        rejections_path: archive.join("rejections.ndjson").display().to_string(),
        artifacts_path: archive.join("artifacts.ndjson").display().to_string(),
        conversations_path: archive.join("conversations.ndjson").display().to_string(),
        participants_path: archive.join("participants.ndjson").display().to_string(),
        cached_moments_path: Some(archive.join("cached-moments.ndjson").display().to_string()),
        cached_moment_interactions_path: Some(
            archive
                .join("cached-moment-interactions.ndjson")
                .display()
                .to_string(),
        ),
        cached_surfaces_path: Some(archive.join("cached-surfaces.json").display().to_string()),
        coverage_path: archive.join("coverage.json").display().to_string(),
        report_path: archive.join("report.json").display().to_string(),
        completion,
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
            schema_fingerprint: Some(hex::encode(sha2::Sha256::digest(format!(
                "schema-{source_set}"
            )))),
        })
        .collect::<Vec<_>>();
    let mut all_tables = message_tables
        .iter()
        .map(|table| TableSchemaCoverage {
            source_set_id: table.source_set_id.clone(),
            source_logical_path: table.source_logical_path.clone(),
            source_table_id: table.source_table_id.clone(),
            source_table_name: table.source_table_name.clone(),
            columns: table.columns.clone(),
            source_row_count: Some(table.source_row_count),
            schema_fingerprint: table.schema_fingerprint.clone(),
            role: TableCoverageRole::Message,
            classification_reason: "synthetic".to_string(),
            availability: greenbubbles_restore::TableCoverageAvailability::Complete,
            limitation_code: None,
        })
        .collect::<Vec<_>>();
    all_tables.extend(source_sets.iter().map(|source_set| TableSchemaCoverage {
        source_set_id: (*source_set).to_string(),
        source_logical_path: "sns/sns.db".to_string(),
        source_table_id: format!("sns-table-{source_set}"),
        source_table_name: "SnsTimeLine".to_string(),
        columns: vec![
            "tid".to_string(),
            "user_name".to_string(),
            "content".to_string(),
        ],
        source_row_count: None,
        schema_fingerprint: Some(hex::encode(sha2::Sha256::digest(format!(
            "sns-schema-{source_set}"
        )))),
        role: TableCoverageRole::Other,
        classification_reason: "synthetic cached table".to_string(),
        availability: greenbubbles_restore::TableCoverageAvailability::Complete,
        limitation_code: None,
    }));
    all_tables.push(TableSchemaCoverage {
        source_set_id: "set-a".to_string(),
        source_logical_path: "media/media_0.db".to_string(),
        source_table_id: opaque_id(b"VoiceInfo"),
        source_table_name: "VoiceInfo".to_string(),
        columns: vec!["voice_data".to_string()],
        source_row_count: None,
        schema_fingerprint: Some(hex::encode(sha2::Sha256::digest("voice-schema"))),
        role: TableCoverageRole::KnownAuxiliary,
        classification_reason: "synthetic voice payload table".to_string(),
        availability: greenbubbles_restore::TableCoverageAvailability::Complete,
        limitation_code: None,
    });
    let coverage = RestorationCoverage {
        format_version: 2,
        decoder_name: "synthetic".to_string(),
        decoder_version: "1".to_string(),
        snapshot_manifest_format_version: if fragment { 3 } else { 2 },
        schema_profile_fingerprint: None,
        message_tables,
        all_tables,
        logical_type_counts: BTreeMap::from([("34".to_string(), messages.len() as u64)]),
        logical_sub_type_counts: BTreeMap::from([("34:0".to_string(), messages.len() as u64)]),
        unknown_payload_reason_counts: BTreeMap::new(),
        semantic_gap_reason_counts: BTreeMap::new(),
    };
    let cached_coverage = CachedSurfaceCoverage {
        format_version: 1,
        schema_profile_fingerprint: None,
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
        omitted_row_count: 0,
        limitation_codes: Vec::new(),
        tables: source_sets
            .iter()
            .map(|source_set| CachedSurfaceTableCoverage {
                source_set_id: (*source_set).to_string(),
                source_logical_path: "sns/sns.db".to_string(),
                source_table_id: format!("sns-table-{source_set}"),
                source_table_name: "SnsTimeLine".to_string(),
                columns: vec![
                    "tid".to_string(),
                    "user_name".to_string(),
                    "content".to_string(),
                ],
                schema_fingerprint: Some(hex::encode(sha2::Sha256::digest(format!(
                    "sns-schema-{source_set}"
                )))),
                source_row_count: 1,
                restored_row_count: 1,
                role: CachedSurfaceTableRole::MomentTimeline,
                classification_reason: "synthetic exact signature".to_string(),
                availability: greenbubbles_restore::TableCoverageAvailability::Complete,
                limitation_code: None,
            })
            .collect(),
    };
    let conversation = CanonicalConversation {
        conversation_id: scoped_id(ACCOUNT, b"conversation"),
        account_id: ACCOUNT.to_string(),
        source_identifier_base64: "Y29udmVyc2F0aW9u".to_string(),
        kind: ConversationKind::Direct,
        participant_ids: vec![scoped_id(ACCOUNT, b"participant")],
        memberships: vec![ConversationMembership {
            participant_id: scoped_id(ACCOUNT, b"participant"),
            role: ConversationMembershipRole::DirectPeer,
            display_name_base64: None,
        }],
        owner_participant_id: None,
        entity_decode_state: EntityDecodeState::Complete,
        source_records: Vec::new(),
    };
    let participant = CanonicalParticipant {
        participant_id: scoped_id(ACCOUNT, b"participant"),
        account_id: ACCOUNT.to_string(),
        source_identifier_base64: "cGFydGljaXBhbnQ=".to_string(),
        alias_base64: None,
        remark_base64: None,
        nickname_base64: None,
        display_name_base64: None,
        local_profile_state: LocalProfileState::Hydrated,
        conversation_ids: vec![scoped_id(ACCOUNT, b"conversation")],
        source_records: Vec::new(),
    };
    let connector_media = archive.join("connector-media.bin");
    write_private(&connector_media, b"synthetic-media");
    let media_sha256 = hex::encode(sha2::Sha256::digest(b"synthetic-media"));
    let artifact = CanonicalArtifact {
        artifact_id: "artifact-a".to_string(),
        kind: ArtifactKind::Voice,
        role: ArtifactRole::VoicePayload,
        roles: BTreeSet::from([ArtifactRole::VoicePayload]),
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
        decode_state: ArtifactDecodeState::Unsupported,
        verification_detail: Some(
            "synthetic lossless voice source has no playable derivative".to_string(),
        ),
        source_resource_set_id: Some("set-a".to_string()),
        source_resource_logical_path: Some("media/media_0.db".to_string()),
        source_resource_table_id: Some(opaque_id(b"VoiceInfo")),
        source_resource_table_name: Some("VoiceInfo".to_string()),
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

fn database_coverage(fragment: bool) -> RestorationDatabaseCoverage {
    let snapshot = vec!["set-a".to_string(), "set-b".to_string()];
    let attempted = if fragment {
        vec!["set-a".to_string()]
    } else {
        snapshot.clone()
    };
    let fresh = attempted.clone();
    RestorationDatabaseCoverage {
        format_version: 1,
        total_database_count: snapshot.len(),
        attempted_database_count: attempted.len(),
        restored_database_count: fresh.len(),
        unavailable_database_count: 0,
        preserved_stale_database_count: 0,
        authoritative_database_coverage: !fragment,
        snapshot_source_set_ids: snapshot,
        attempted_source_set_ids: attempted,
        fresh_source_set_ids: fresh,
        unavailable_source_set_ids: Vec::new(),
        preserved_stale_source_set_ids: Vec::new(),
        unavailable_databases: Vec::new(),
    }
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
        last_integrity_scan_at: None,
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
        conversation_id: scoped_id(ACCOUNT, b"conversation"),
        conversation_source_identifier_base64: "Y29udmVyc2F0aW9u".to_string(),
        sender_id: Some(scoped_id(ACCOUNT, b"participant")),
        sender_source_identifier_base64: Some("cGFydGljaXBhbnQ=".to_string()),
        local_id: Some(source_row_id),
        server_id: Some(if source_set_id == "set-b" { 200 } else { 100 }),
        sort_sequence: Some(sort_sequence),
        created_at_unix: Some(1_700_000_000 + source_row_id),
        conversation_ordinal: 0,
        ordering_basis: MessageOrderingBasis::SortSequence,
        raw_type: Some(34),
        logical_type: Some(34),
        sub_type: Some(0),
        status: Some(2),
        direction: MessageDirection::Incoming,
        direction_evidence: DirectionEvidence::SenderMatchesConversation,
        content_base64: Some("c3ludGhldGlj".to_string()),
        packed_info_base64: None,
        compression_type: None,
        raw_columns: BTreeMap::from([
            (
                "local_id".to_string(),
                RawSQLiteValue::Integer(source_row_id),
            ),
            (
                "message_content".to_string(),
                RawSQLiteValue::TextBase64("c3ludGhldGlj".to_string()),
            ),
        ]),
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

fn cached_moment(source_set_id: &str, source_row_id: i64) -> CanonicalCachedMoment {
    CanonicalCachedMoment {
        canonical_id: cached_id(source_set_id, source_row_id),
        account_id: ACCOUNT.to_string(),
        source_set_id: source_set_id.to_string(),
        source_logical_path: "sns/sns.db".to_string(),
        source_table_id: format!("sns-table-{source_set_id}"),
        source_table_name: "SnsTimeLine".to_string(),
        source_row_id,
        timeline_id: RawSQLiteValue::Integer(source_row_id),
        author_id: Some(scoped_id(ACCOUNT, b"cached-author")),
        author_source_identifier_base64: Some("Y2FjaGVkLWF1dGhvcg==".to_string()),
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

fn cached_id(source_set_id: &str, source_row_id: i64) -> String {
    hex::encode(sha2::Sha256::digest(format!(
        "{source_set_id}:sns-table-{source_set_id}:{source_row_id}"
    )))
}

fn scoped_id(scope: &str, value: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(scope.as_bytes());
    hasher.update([0]);
    hasher.update(value);
    hex::encode(hasher.finalize())
}

fn opaque_id(value: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(value))
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
