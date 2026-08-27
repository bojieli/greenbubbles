use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::archive::ensure_private_directory;
use crate::replica::{bootstrap_replica, replica_status, synchronize_replica};
use crate::{
    CanonicalConversation, CanonicalMessage, CanonicalParticipant, ConversationKind,
    DirectionEvidence, EntityDecodeState, LocalProfileState, MessageDirection,
    MessageOrderingBasis, MessageRelationship, MessageRelationshipKind,
    RelationshipResolutionState, ReplicaKey, RestorationArchiveScope, RestorationCompletion,
    RestorationCoverage, RestorationIntegrity, RestorationMediaPhase, RestorationReport,
    RestoreError, SemanticDecodeState, TypedPayload,
};

const ACCOUNT_ID: &str = "synthetic-benchmark-account";
const CONVERSATION_ID: &str = "synthetic-benchmark-conversation";
const PARTICIPANT_ID: &str = "synthetic-benchmark-participant";
const SYNTHETIC_REPLICA_KEY: [u8; 32] = [0x5a; 32];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyntheticBenchmarkConfig {
    pub samples: usize,
    pub small_message_count: usize,
    pub large_message_count: usize,
    pub burst_message_count: usize,
}

impl Default for SyntheticBenchmarkConfig {
    fn default() -> Self {
        Self {
            samples: 7,
            small_message_count: 100,
            large_message_count: 5_000,
            burst_message_count: 100,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyntheticBenchmarkReport {
    pub format_version: u32,
    pub synthetic_only: bool,
    pub evidence_scope: String,
    pub real_corpus_objective_evaluated: bool,
    pub config: SyntheticBenchmarkConfig,
    pub cases: Vec<SyntheticBenchmarkCase>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyntheticBenchmarkCase {
    pub name: String,
    pub operation: String,
    pub samples: usize,
    pub input_message_count: usize,
    pub candidate_change_count: usize,
    pub archive_bytes: u64,
    pub p50_milliseconds: f64,
    pub p95_milliseconds: f64,
    pub maximum_milliseconds: f64,
    pub expected_added_count: u64,
    pub expected_changed_count: u64,
    pub expected_removed_count: u64,
    pub wake_hint_used: bool,
    pub fault_injected: bool,
    pub verified: bool,
}

struct CaseObservation {
    elapsed_nanoseconds: u128,
    archive_bytes: u64,
    added_count: u64,
    changed_count: u64,
    removed_count: u64,
    verified: bool,
}

#[derive(Clone, Copy)]
struct CaseDefinition {
    name: &'static str,
    operation: &'static str,
    input_message_count: usize,
    candidate_change_count: usize,
    expected_added_count: u64,
    expected_changed_count: u64,
    expected_removed_count: u64,
    wake_hint_used: bool,
    fault_injected: bool,
}

pub fn run_synthetic_benchmark(
    work_directory: &Path,
    config: &SyntheticBenchmarkConfig,
) -> Result<SyntheticBenchmarkReport, RestoreError> {
    validate_config(config)?;
    if work_directory.try_exists()? {
        ensure_private_directory(work_directory)?;
    } else {
        fs::create_dir(work_directory)?;
        fs::set_permissions(work_directory, fs::Permissions::from_mode(0o700))?;
    }
    let temporary = tempfile::Builder::new()
        .prefix("greenbubbles-synthetic-benchmark-")
        .tempdir_in(work_directory)?;
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))?;
    let key = ReplicaKey::from_bytes(SYNTHETIC_REPLICA_KEY);
    let mut cases = Vec::new();

    cases.push(measure_case(
        temporary.path(),
        config.samples,
        CaseDefinition {
            name: "bootstrap-small",
            operation: "replicaBootstrap",
            input_message_count: config.small_message_count,
            candidate_change_count: config.small_message_count,
            expected_added_count: 0,
            expected_changed_count: 0,
            expected_removed_count: 0,
            wake_hint_used: false,
            fault_injected: false,
        },
        |sample, directory| {
            let archive = build_archive(
                directory,
                "archive",
                &format!("bootstrap-small-{sample}"),
                "1",
                messages(config.small_message_count),
            )?;
            let replica = directory.join("replica.db");
            let bytes = archive_size(&archive)?;
            let started = Instant::now();
            let report = bootstrap_replica(&archive, &replica, &key)?;
            Ok(CaseObservation {
                elapsed_nanoseconds: started.elapsed().as_nanos(),
                archive_bytes: bytes,
                added_count: 0,
                changed_count: 0,
                removed_count: 0,
                verified: report.message_count == config.small_message_count as u64,
            })
        },
    )?);

    cases.push(measure_case(
        temporary.path(),
        config.samples,
        CaseDefinition {
            name: "bootstrap-large",
            operation: "replicaBootstrap",
            input_message_count: config.large_message_count,
            candidate_change_count: config.large_message_count,
            expected_added_count: 0,
            expected_changed_count: 0,
            expected_removed_count: 0,
            wake_hint_used: false,
            fault_injected: false,
        },
        |sample, directory| {
            let archive = build_archive(
                directory,
                "archive",
                &format!("bootstrap-large-{sample}"),
                "1",
                messages(config.large_message_count),
            )?;
            let replica = directory.join("replica.db");
            let bytes = archive_size(&archive)?;
            let started = Instant::now();
            let report = bootstrap_replica(&archive, &replica, &key)?;
            Ok(CaseObservation {
                elapsed_nanoseconds: started.elapsed().as_nanos(),
                archive_bytes: bytes,
                added_count: 0,
                changed_count: 0,
                removed_count: 0,
                verified: report.message_count == config.large_message_count as u64,
            })
        },
    )?);

    cases.push(measure_sync_case(
        temporary.path(),
        config,
        &key,
        "idle-no-op",
        "source-idle",
        messages(config.small_message_count),
        messages(config.small_message_count),
        0,
        (0, 0, 0),
        false,
        false,
    )?);

    let base = messages(config.small_message_count);
    let mut one_message = base.clone();
    one_message.push(message(config.small_message_count, 1));
    cases.push(measure_sync_case(
        temporary.path(),
        config,
        &key,
        "one-message",
        "source-one-message",
        base,
        one_message,
        1,
        (1, 0, 0),
        true,
        false,
    )?);

    let base = messages(config.small_message_count);
    let mut burst = base.clone();
    for index in 0..config.burst_message_count {
        burst.push(message(config.small_message_count + index, 1));
    }
    cases.push(measure_sync_case(
        temporary.path(),
        config,
        &key,
        "burst",
        "source-burst",
        base,
        burst,
        config.burst_message_count,
        (config.burst_message_count as u64, 0, 0),
        true,
        false,
    )?);

    let base = messages(config.small_message_count);
    let mut edited = base.clone();
    edited[config.small_message_count / 2].typed_payload =
        TypedPayload::Decoded(json!({"Text": "synthetic edited body"}));
    cases.push(measure_sync_case(
        temporary.path(),
        config,
        &key,
        "edit",
        "source-edit",
        base,
        edited,
        1,
        (0, 1, 0),
        true,
        false,
    )?);

    let base = messages(config.small_message_count);
    let mut recalled = base.clone();
    recalled.push(recall_message(config.small_message_count, &base[0]));
    cases.push(measure_sync_case(
        temporary.path(),
        config,
        &key,
        "recall",
        "source-recall",
        base,
        recalled,
        1,
        (1, 0, 0),
        true,
        false,
    )?);

    let base = messages(config.small_message_count);
    let deleted = base.iter().skip(1).cloned().collect::<Vec<_>>();
    cases.push(measure_sync_case(
        temporary.path(),
        config,
        &key,
        "deletion",
        "source-deletion",
        base,
        deleted,
        1,
        (0, 0, 1),
        true,
        false,
    )?);

    let base = messages(config.small_message_count);
    let mut missed = base.clone();
    missed.push(message(config.small_message_count, 1));
    cases.push(measure_sync_case(
        temporary.path(),
        config,
        &key,
        "missed-hint-reconciliation",
        "authoritativeSweepWithoutWakeHint",
        base,
        missed,
        1,
        (1, 0, 0),
        false,
        false,
    )?);

    cases.push(measure_decoder_upgrade(temporary.path(), config, &key)?);
    cases.push(measure_crash_restart(temporary.path(), config, &key)?);

    Ok(SyntheticBenchmarkReport {
        format_version: 1,
        synthetic_only: true,
        evidence_scope: "generated canonical archives and encrypted replica transactions; excludes live WeChat acquisition, SQLCipher source decoding, notification latency, and real media I/O".to_string(),
        real_corpus_objective_evaluated: false,
        config: config.clone(),
        cases,
    })
}

#[allow(clippy::too_many_arguments)]
fn measure_sync_case(
    root: &Path,
    config: &SyntheticBenchmarkConfig,
    key: &ReplicaKey,
    name: &'static str,
    operation: &'static str,
    before: Vec<CanonicalMessage>,
    after: Vec<CanonicalMessage>,
    candidates: usize,
    expected: (u64, u64, u64),
    wake_hint_used: bool,
    fault_injected: bool,
) -> Result<SyntheticBenchmarkCase, RestoreError> {
    let input_count = after.len();
    measure_case(
        root,
        config.samples,
        CaseDefinition {
            name,
            operation,
            input_message_count: input_count,
            candidate_change_count: candidates,
            expected_added_count: expected.0,
            expected_changed_count: expected.1,
            expected_removed_count: expected.2,
            wake_hint_used,
            fault_injected,
        },
        |sample, directory| {
            let same_source = name == "idle-no-op";
            let before_fingerprint = if same_source {
                format!("{name}-idle-{sample}")
            } else {
                format!("{name}-before-{sample}")
            };
            let after_fingerprint = if same_source {
                before_fingerprint.clone()
            } else {
                format!("{name}-after-{sample}")
            };
            let base = build_archive(
                directory,
                "before",
                &before_fingerprint,
                "1",
                before.clone(),
            )?;
            let next = build_archive(directory, "after", &after_fingerprint, "1", after.clone())?;
            let replica = directory.join("replica.db");
            bootstrap_replica(&base, &replica, key)?;
            let bytes = archive_size(&next)?;
            let started = Instant::now();
            let report = synchronize_replica(&next, &replica, key)?;
            Ok(CaseObservation {
                elapsed_nanoseconds: started.elapsed().as_nanos(),
                archive_bytes: bytes,
                added_count: report.added_count,
                changed_count: report.changed_count,
                removed_count: report.removed_count,
                verified: (
                    report.added_count,
                    report.changed_count,
                    report.removed_count,
                ) == expected
                    && report.idempotent == same_source,
            })
        },
    )
}

fn measure_decoder_upgrade(
    root: &Path,
    config: &SyntheticBenchmarkConfig,
    key: &ReplicaKey,
) -> Result<SyntheticBenchmarkCase, RestoreError> {
    measure_case(
        root,
        config.samples,
        CaseDefinition {
            name: "decoder-upgrade",
            operation: "sameSourceRevisionReconciliation",
            input_message_count: config.small_message_count,
            candidate_change_count: 0,
            expected_added_count: 0,
            expected_changed_count: 0,
            expected_removed_count: 0,
            wake_hint_used: false,
            fault_injected: false,
        },
        |sample, directory| {
            let fingerprint = format!("decoder-upgrade-{sample}");
            let base = build_archive(
                directory,
                "before",
                &fingerprint,
                "1",
                messages(config.small_message_count),
            )?;
            let upgraded = build_archive(
                directory,
                "after",
                &fingerprint,
                "2",
                messages(config.small_message_count),
            )?;
            let replica = directory.join("replica.db");
            bootstrap_replica(&base, &replica, key)?;
            let previous_revision = replica_status(&replica, key)?.checkpoint_revision;
            let bytes = archive_size(&upgraded)?;
            let started = Instant::now();
            let report = synchronize_replica(&upgraded, &replica, key)?;
            let elapsed = started.elapsed().as_nanos();
            let status = replica_status(&replica, key)?;
            Ok(CaseObservation {
                elapsed_nanoseconds: elapsed,
                archive_bytes: bytes,
                added_count: report.added_count,
                changed_count: report.changed_count,
                removed_count: report.removed_count,
                verified: !report.idempotent
                    && status.decoder_version.as_deref() == Some("2")
                    && status.checkpoint_revision != previous_revision,
            })
        },
    )
}

fn measure_crash_restart(
    root: &Path,
    config: &SyntheticBenchmarkConfig,
    key: &ReplicaKey,
) -> Result<SyntheticBenchmarkCase, RestoreError> {
    measure_case(
        root,
        config.samples,
        CaseDefinition {
            name: "crash-restart",
            operation: "rollbackReopenAndRetry",
            input_message_count: config.small_message_count + 1,
            candidate_change_count: 1,
            expected_added_count: 1,
            expected_changed_count: 0,
            expected_removed_count: 0,
            wake_hint_used: true,
            fault_injected: true,
        },
        |sample, directory| {
            let base_fingerprint = format!("crash-before-{sample}");
            let next_fingerprint = format!("crash-after-{sample}");
            let base_messages = messages(config.small_message_count);
            let mut next_messages = base_messages.clone();
            next_messages.push(message(config.small_message_count, 1));
            let base = build_archive(directory, "before", &base_fingerprint, "1", base_messages)?;
            let malformed = build_archive(
                directory,
                "malformed",
                &next_fingerprint,
                "1",
                next_messages.clone(),
            )?;
            let valid = build_archive(directory, "valid", &next_fingerprint, "1", next_messages)?;
            let mut file = OpenOptions::new()
                .append(true)
                .open(malformed.join("messages.ndjson"))?;
            file.write_all(b"{synthetic-crash\n")?;
            file.sync_all()?;
            let replica = directory.join("replica.db");
            bootstrap_replica(&base, &replica, key)?;
            let bytes = archive_size(&valid)?;
            let started = Instant::now();
            let rejected = synchronize_replica(&malformed, &replica, key).is_err();
            let retained = replica_status(&replica, key)?
                .current_source_fingerprint
                .as_deref()
                == Some(base_fingerprint.as_str());
            let report = synchronize_replica(&valid, &replica, key)?;
            Ok(CaseObservation {
                elapsed_nanoseconds: started.elapsed().as_nanos(),
                archive_bytes: bytes,
                added_count: report.added_count,
                changed_count: report.changed_count,
                removed_count: report.removed_count,
                verified: rejected
                    && retained
                    && (
                        report.added_count,
                        report.changed_count,
                        report.removed_count,
                    ) == (1, 0, 0),
            })
        },
    )
}

fn measure_case(
    root: &Path,
    samples: usize,
    definition: CaseDefinition,
    mut run: impl FnMut(usize, &Path) -> Result<CaseObservation, RestoreError>,
) -> Result<SyntheticBenchmarkCase, RestoreError> {
    let case_root = root.join(definition.name);
    fs::create_dir(&case_root)?;
    fs::set_permissions(&case_root, fs::Permissions::from_mode(0o700))?;
    let mut observations = Vec::with_capacity(samples);
    for sample in 0..samples {
        let directory = case_root.join(format!("sample-{sample}"));
        fs::create_dir(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        observations.push(run(sample, &directory)?);
    }
    let mut durations = observations
        .iter()
        .map(|observation| observation.elapsed_nanoseconds)
        .collect::<Vec<_>>();
    durations.sort_unstable();
    let expected = (
        definition.expected_added_count,
        definition.expected_changed_count,
        definition.expected_removed_count,
    );
    let verified = observations.iter().all(|observation| {
        observation.verified
            && (
                observation.added_count,
                observation.changed_count,
                observation.removed_count,
            ) == expected
    });
    Ok(SyntheticBenchmarkCase {
        name: definition.name.to_string(),
        operation: definition.operation.to_string(),
        samples,
        input_message_count: definition.input_message_count,
        candidate_change_count: definition.candidate_change_count,
        archive_bytes: observations
            .iter()
            .map(|observation| observation.archive_bytes)
            .max()
            .unwrap_or_default(),
        p50_milliseconds: milliseconds(percentile(&durations, 50)),
        p95_milliseconds: milliseconds(percentile(&durations, 95)),
        maximum_milliseconds: milliseconds(*durations.last().unwrap_or(&0)),
        expected_added_count: definition.expected_added_count,
        expected_changed_count: definition.expected_changed_count,
        expected_removed_count: definition.expected_removed_count,
        wake_hint_used: definition.wake_hint_used,
        fault_injected: definition.fault_injected,
        verified,
    })
}

fn percentile(values: &[u128], percentile: usize) -> u128 {
    if values.is_empty() {
        return 0;
    }
    let rank = (percentile * values.len()).div_ceil(100).saturating_sub(1);
    values[rank.min(values.len() - 1)]
}

fn milliseconds(nanoseconds: u128) -> f64 {
    nanoseconds as f64 / 1_000_000.0
}

fn validate_config(config: &SyntheticBenchmarkConfig) -> Result<(), RestoreError> {
    if !(1..=100).contains(&config.samples)
        || !(1..=1_000_000).contains(&config.small_message_count)
        || config.large_message_count < config.small_message_count
        || config.large_message_count > 1_000_000
        || !(1..=100_000).contains(&config.burst_message_count)
    {
        return Err(RestoreError::Integrity(
            "synthetic benchmark configuration is outside supported bounds".to_string(),
        ));
    }
    Ok(())
}

fn messages(count: usize) -> Vec<CanonicalMessage> {
    (0..count).map(|index| message(index, 0)).collect()
}

fn message(index: usize, generation: usize) -> CanonicalMessage {
    CanonicalMessage {
        canonical_id: format!("synthetic-message-{index}"),
        account_id: ACCOUNT_ID.to_string(),
        source_set_id: "synthetic-set".to_string(),
        source_logical_path: "synthetic/messages.db".to_string(),
        source_table_id: "synthetic-table".to_string(),
        source_table_name: "message".to_string(),
        source_row_id: index as i64 + 1,
        conversation_id: CONVERSATION_ID.to_string(),
        conversation_source_identifier_base64: "c3ludGhldGljLWNvbnZlcnNhdGlvbg==".to_string(),
        sender_id: Some(PARTICIPANT_ID.to_string()),
        sender_source_identifier_base64: None,
        local_id: Some(index as i64 + 1),
        server_id: Some(index as i64 + 10_000),
        sort_sequence: Some(index as i64),
        created_at_unix: Some(1_700_000_000 + index as i64),
        conversation_ordinal: index as u64,
        ordering_basis: MessageOrderingBasis::SortSequence,
        raw_type: Some(1),
        logical_type: Some(1),
        sub_type: Some(0),
        status: Some(2),
        direction: MessageDirection::Incoming,
        direction_evidence: DirectionEvidence::SenderMatchesConversation,
        content_base64: None,
        packed_info_base64: None,
        compression_type: None,
        raw_columns: BTreeMap::new(),
        typed_payload: TypedPayload::Decoded(json!({
            "Text": format!("synthetic message {index} generation {generation}")
        })),
        semantic_decode_state: SemanticDecodeState::Complete,
        semantic_gap_reason: None,
        relationships: Vec::new(),
        artifact_references: Vec::new(),
    }
}

fn recall_message(index: usize, target: &CanonicalMessage) -> CanonicalMessage {
    let mut result = message(index, 1);
    result.raw_type = Some(10_002);
    result.logical_type = Some(10_002);
    result.typed_payload = TypedPayload::Decoded(json!({"Recall": target.server_id}));
    result.relationships = vec![MessageRelationship {
        kind: MessageRelationshipKind::Recall,
        target_canonical_id: Some(target.canonical_id.clone()),
        target_server_id: target.server_id,
        target_local_id: target.local_id,
        resolved: true,
        resolution_state: RelationshipResolutionState::Resolved,
        raw_reference_base64: None,
    }];
    result
}

fn build_archive(
    parent: &Path,
    name: &str,
    fingerprint: &str,
    decoder_version: &str,
    messages: Vec<CanonicalMessage>,
) -> Result<PathBuf, RestoreError> {
    let archive = parent.join(name);
    fs::create_dir(&archive)?;
    fs::set_permissions(&archive, fs::Permissions::from_mode(0o700))?;
    let relationship_count = messages
        .iter()
        .map(|message| message.relationships.len() as u64)
        .sum();
    let mut integrity = RestorationIntegrity {
        database_count: 1,
        message_table_count: 1,
        source_row_count: messages.len() as u64,
        restored_row_count: messages.len() as u64,
        conversation_count: 1,
        participant_count: 1,
        relationship_reference_count: relationship_count,
        resolved_relationship_count: relationship_count,
        ..Default::default()
    };
    integrity
        .direction_counts
        .insert("incoming".to_string(), messages.len() as u64);
    integrity
        .logical_type_counts
        .insert("1".to_string(), messages.len() as u64);
    let completion = RestorationCompletion::evaluate(&integrity);
    let report = RestorationReport {
        format_version: 4,
        account_id: ACCOUNT_ID.to_string(),
        source_fingerprint: fingerprint.to_string(),
        client_build_compatibility: Default::default(),
        acquisition: None,
        archive_scope: RestorationArchiveScope::Authoritative,
        media_phase: RestorationMediaPhase::Resolved,
        messages_path: "synthetic".to_string(),
        rejections_path: "synthetic".to_string(),
        artifacts_path: "synthetic".to_string(),
        conversations_path: "synthetic".to_string(),
        participants_path: "synthetic".to_string(),
        coverage_path: "synthetic".to_string(),
        report_path: "synthetic".to_string(),
        integrity,
        completion,
    };
    let coverage = RestorationCoverage {
        format_version: 2,
        decoder_name: "greenbubbles-synthetic-benchmark".to_string(),
        decoder_version: decoder_version.to_string(),
        snapshot_manifest_format_version: 3,
        message_tables: Vec::new(),
        all_tables: Vec::new(),
        logical_type_counts: BTreeMap::from([("1".to_string(), messages.len() as u64)]),
        logical_sub_type_counts: BTreeMap::new(),
        unknown_payload_reason_counts: BTreeMap::new(),
        semantic_gap_reason_counts: BTreeMap::new(),
    };
    let conversation = CanonicalConversation {
        conversation_id: CONVERSATION_ID.to_string(),
        account_id: ACCOUNT_ID.to_string(),
        source_identifier_base64: "c3ludGhldGljLWNvbnZlcnNhdGlvbg==".to_string(),
        kind: ConversationKind::Direct,
        participant_ids: vec![PARTICIPANT_ID.to_string()],
        memberships: Vec::new(),
        owner_participant_id: None,
        entity_decode_state: EntityDecodeState::Complete,
        source_records: Vec::new(),
    };
    let participant = CanonicalParticipant {
        participant_id: PARTICIPANT_ID.to_string(),
        account_id: ACCOUNT_ID.to_string(),
        source_identifier_base64: "c3ludGhldGljLXBhcnRpY2lwYW50".to_string(),
        alias_base64: None,
        remark_base64: None,
        nickname_base64: None,
        display_name_base64: None,
        local_profile_state: LocalProfileState::Hydrated,
        conversation_ids: vec![CONVERSATION_ID.to_string()],
        source_records: Vec::new(),
    };
    write_json(&archive.join("report.json"), &report)?;
    write_json(&archive.join("coverage.json"), &coverage)?;
    write_ndjson(&archive.join("conversations.ndjson"), &[conversation])?;
    write_ndjson(&archive.join("participants.ndjson"), &[participant])?;
    write_ndjson::<crate::CanonicalArtifact>(&archive.join("artifacts.ndjson"), &[])?;
    write_ndjson(&archive.join("messages.ndjson"), &messages)?;
    write_ndjson::<crate::RejectedRow>(&archive.join("rejections.ndjson"), &[])?;
    Ok(archive)
}

fn archive_size(path: &Path) -> Result<u64, RestoreError> {
    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), RestoreError> {
    let mut writer = private_writer(path)?;
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn write_ndjson<T: Serialize>(path: &Path, values: &[T]) -> Result<(), RestoreError> {
    let mut writer = private_writer(path)?;
    for value in values {
        serde_json::to_writer(&mut writer, value)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(())
}

fn private_writer(path: &Path) -> Result<BufWriter<File>, RestoreError> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    Ok(BufWriter::new(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exercises_every_synthetic_sync_and_fault_case() {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let report = run_synthetic_benchmark(
            temporary.path(),
            &SyntheticBenchmarkConfig {
                samples: 1,
                small_message_count: 4,
                large_message_count: 8,
                burst_message_count: 2,
            },
        )
        .unwrap();
        assert!(report.synthetic_only);
        assert!(!report.real_corpus_objective_evaluated);
        assert_eq!(report.cases.len(), 11);
        assert!(report.cases.iter().all(|case| case.verified));
        assert!(report
            .cases
            .iter()
            .any(|case| case.name == "missed-hint-reconciliation" && !case.wake_hint_used));
        assert!(report
            .cases
            .iter()
            .any(|case| case.name == "crash-restart" && case.fault_injected));
    }
}
