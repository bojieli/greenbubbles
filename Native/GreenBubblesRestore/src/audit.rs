use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::archive::{ensure_private_directory, ensure_private_regular_file};
use crate::schema::{schema_profile_fingerprint, validate_cached_coverage_schema};
use crate::{
    ArtifactAvailability, ArtifactDecodeState, ArtifactKind, CachedSurfaceCoverage,
    CachedSurfaceTableRole, CanonicalArtifact, CanonicalCachedMoment,
    CanonicalCachedMomentInteraction, CanonicalConversation, CanonicalMessage,
    CanonicalParticipant, ConversationKind, ConversationMembershipRole, DirectionEvidence,
    EntityDecodeState, LocalProfileState, MessageDirection, NoProgress, ProgressEvent,
    ProgressObserver, ProgressPhase, ProgressState, ProgressUnit, RawSQLiteValue, RejectedRow,
    RelationshipResolutionState, RestorationArchiveScope, RestorationCompletion,
    RestorationCoverage, RestorationMediaPhase, RestorationReport, RestoreError,
    SemanticDecodeState, TableCoverageRole, TypedPayload,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveAuditReport {
    pub format_version: u32,
    pub privacy_safe_summary: bool,
    pub archive_format_version: u32,
    pub coverage_format_version: u32,
    pub archive_scope: RestorationArchiveScope,
    pub authoritative_database_coverage: bool,
    pub total_database_count: usize,
    pub restored_database_count: usize,
    pub unavailable_database_count: usize,
    pub preserved_stale_database_count: usize,
    pub media_phase: RestorationMediaPhase,
    pub client_build_production_compatible: bool,
    pub message_count: u64,
    pub rejection_count: u64,
    pub artifact_count: u64,
    pub artifact_reference_count: u64,
    pub relationship_reference_count: u64,
    pub conversation_count: u64,
    pub participant_count: u64,
    pub cached_moment_count: u64,
    pub cached_moment_interaction_count: u64,
    #[serde(default)]
    pub cached_surface_omitted_row_count: u64,
    pub verified_external_source_file_count: u64,
    pub verified_connector_owned_file_count: u64,
    pub row_equation_holds: bool,
    pub report_matches_archive: bool,
    pub all_artifact_references_resolve: bool,
    pub all_resolved_relationships_resolve: bool,
    pub all_recorded_artifact_files_match: bool,
    pub full_restoration_claimed: bool,
    pub full_restoration_verified: bool,
    pub semantic_gap_count: u64,
    pub message_candidate_gap_count: u64,
    pub missing_artifact_count: u64,
    pub ambiguous_artifact_count: u64,
    pub corrupt_artifact_count: u64,
    pub unsafe_artifact_count: u64,
    pub artifact_decode_gap_count: u64,
    pub entity_decode_gap_count: u64,
    pub unresolved_relationship_count: u64,
    pub account_holder_bound: bool,
    pub direction_conflict_count: u64,
    pub completion_evidence: AuditedRestorationCompletionEvidence,
}

impl ArchiveAuditReport {
    /// Canonical source records retained by the archive. Conversations,
    /// participants, and artifacts are derived/supporting ledgers and are not
    /// counted as restored source records.
    pub fn restored_record_count(&self) -> u64 {
        self.message_count
            .saturating_add(self.cached_moment_count)
            .saturating_add(self.cached_moment_interaction_count)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditedRestorationCompletionEvidence {
    pub format_version: u32,
    pub row_accounting_complete: bool,
    pub observed_message_type_coverage_complete: bool,
    pub direction_resolution_complete: bool,
    pub entity_reconstruction_complete: bool,
    pub relationship_resolution_complete: bool,
    pub artifact_verification_complete: bool,
    pub artifact_decoding_complete: bool,
    pub source_scope_authoritative: bool,
    pub media_phase_resolved: bool,
    pub client_build_production_compatible: bool,
    pub technical_restoration_complete: bool,
    pub non_empty_message_corpus_observed: bool,
    pub media_reference_corpus_observed: bool,
    pub verified_local_media_observed: bool,
    pub external_authorization_attestation_required: bool,
    pub disposable_scenario_attestation_required: bool,
    pub observed_corpus_scope_only: bool,
}

#[derive(Default)]
struct MessageAudit {
    count: u64,
    source_identities: HashSet<(String, String, i64)>,
    source_table_counts: HashMap<(String, String), u64>,
    canonical_ids: HashSet<String>,
    canonical_conversations: HashMap<String, String>,
    conversation_ids: HashSet<String>,
    conversation_sources: HashMap<String, String>,
    participant_ids: HashSet<String>,
    participant_sources: HashMap<String, String>,
    participant_memberships: Vec<(String, String)>,
    artifact_ids: HashSet<String>,
    artifact_references: Vec<(String, crate::ArtifactRole)>,
    artifact_reference_count: u64,
    relationship_reference_count: u64,
    resolved_relationship_count: u64,
    absent_relationship_target_count: u64,
    missing_relationship_identifier_count: u64,
    ambiguous_relationship_count: u64,
    pending_relationship_count: u64,
    resolved_relationships: Vec<(String, String)>,
    logical_type_counts: BTreeMap<String, u64>,
    logical_sub_type_counts: BTreeMap<String, u64>,
    unknown_payload_reason_counts: BTreeMap<String, u64>,
    semantic_gap_reason_counts: BTreeMap<String, u64>,
    direction_counts: BTreeMap<String, u64>,
    direction_conflict_count: u64,
    ordering_basis_counts: BTreeMap<String, u64>,
}

#[derive(Default)]
struct RejectionAudit {
    count: u64,
    source_identities: HashSet<(String, String, Option<i64>)>,
    source_table_counts: HashMap<(String, String), u64>,
}

#[derive(Default)]
struct ArtifactAudit {
    count: u64,
    identifiers: HashSet<String>,
    roles: HashMap<String, BTreeSet<crate::ArtifactRole>>,
    external_paths: BTreeSet<PathBuf>,
    connector_paths: BTreeSet<PathBuf>,
    downloaded: u64,
    materialized: u64,
    missing: u64,
    ambiguous: u64,
    corrupt: u64,
    unsafe_count: u64,
    decoded: u64,
    decode_gaps: u64,
    account_root_unavailable: u64,
}

#[derive(Clone)]
struct VerifiedFile {
    byte_count: u64,
    sha256: String,
    device_id: u64,
    file_id: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

pub fn audit_archive(archive_directory: &Path) -> Result<ArchiveAuditReport, RestoreError> {
    audit_archive_with_progress(archive_directory, &NoProgress)
}

pub fn audit_archive_with_progress(
    archive_directory: &Path,
    observer: &dyn ProgressObserver,
) -> Result<ArchiveAuditReport, RestoreError> {
    ensure_private_directory(archive_directory)?;
    let archive_root = fs::canonicalize(archive_directory)?;
    let mut progress = ArchiveAuditProgress::new(&archive_root, observer)?;
    let report_path = archive_root.join("report.json");
    let coverage_path = archive_root.join("coverage.json");
    let report: RestorationReport = read_json(&report_path)?;
    let coverage: RestorationCoverage = read_json(&coverage_path)?;

    if !matches!(report.format_version, 3..=6) || !matches!(coverage.format_version, 2..=4) {
        return Err(integrity(
            "archive or coverage format is not supported by this auditor",
        ));
    }
    verify_database_coverage(&report)?;
    verify_database_record_coverage(&report, &coverage)?;
    verify_report_paths(&archive_root, &report)?;
    verify_coverage(&coverage, &report)?;
    verify_account_holder_binding(&report)?;

    let conversations = read_unique_ndjson::<CanonicalConversation, _, _>(
        &archive_root.join("conversations.ndjson"),
        |value| value.conversation_id.clone(),
        "conversation",
        &mut progress,
    )?;
    let participants = read_unique_ndjson::<CanonicalParticipant, _, _>(
        &archive_root.join("participants.ndjson"),
        |value| value.participant_id.clone(),
        "participant",
        &mut progress,
    )?;
    verify_entities(&conversations, &participants, &report, &coverage)?;

    let messages = audit_messages(
        &archive_root.join("messages.ndjson"),
        &report,
        &coverage,
        &mut progress,
    )?;
    verify_message_entities(&messages, &conversations, &participants)?;

    let rejections = audit_rejections(&archive_root.join("rejections.ndjson"), &mut progress)?;
    if rejections.count != report.integrity.rejected_row_count {
        return Err(integrity("rejection ledger count does not match report"));
    }
    verify_source_row_accounting(&coverage, &messages, &rejections)?;

    let artifacts = audit_artifacts(
        &archive_root,
        &archive_root.join("artifacts.ndjson"),
        &coverage,
        &mut progress,
    )?;
    verify_message_artifacts(&messages, &artifacts)?;
    verify_integrity_counts(
        &report,
        &coverage,
        &messages,
        &rejections,
        &artifacts,
        &conversations,
        &participants,
    )?;

    let (cached_moment_count, cached_interaction_count) =
        audit_cached_surfaces(&archive_root, &report, &coverage, &mut progress)?;
    if cached_moment_count != report.integrity.cached_moment_count
        || cached_interaction_count != report.integrity.cached_moment_interaction_count
    {
        return Err(integrity(
            "cached-surface record counts do not match report",
        ));
    }
    verify_storage_evidence(&archive_root, &report)?;

    let audited_completion = verify_completion(&report)?;
    let full_restoration_verified = report.completion.full_restoration_achieved
        && report.archive_scope == RestorationArchiveScope::Authoritative
        && report.media_phase == RestorationMediaPhase::Resolved
        && report.client_build_compatibility.production_compatible;
    let completion_evidence = audited_completion_evidence(
        &audited_completion,
        report.archive_scope,
        report.media_phase,
        report.client_build_compatibility.production_compatible,
        messages.count,
        messages.artifact_reference_count,
        artifacts.external_paths.len() as u64 + artifacts.connector_paths.len() as u64,
    );

    let result = ArchiveAuditReport {
        format_version: 2,
        privacy_safe_summary: true,
        archive_format_version: report.format_version,
        coverage_format_version: coverage.format_version,
        archive_scope: report.archive_scope,
        authoritative_database_coverage: report
            .database_coverage
            .as_ref()
            .is_none_or(|coverage| coverage.authoritative_database_coverage),
        total_database_count: report
            .database_coverage
            .as_ref()
            .map_or(report.integrity.database_count as usize, |coverage| {
                coverage.total_database_count
            }),
        restored_database_count: report
            .database_coverage
            .as_ref()
            .map_or(report.integrity.database_count as usize, |coverage| {
                coverage.restored_database_count
            }),
        unavailable_database_count: report
            .database_coverage
            .as_ref()
            .map_or(0, |coverage| coverage.unavailable_database_count),
        preserved_stale_database_count: report
            .database_coverage
            .as_ref()
            .map_or(0, |coverage| coverage.preserved_stale_database_count),
        media_phase: report.media_phase,
        client_build_production_compatible: report.client_build_compatibility.production_compatible,
        message_count: messages.count,
        rejection_count: rejections.count,
        artifact_count: artifacts.count,
        artifact_reference_count: messages.artifact_reference_count,
        relationship_reference_count: messages.relationship_reference_count,
        conversation_count: conversations.len() as u64,
        participant_count: participants.len() as u64,
        cached_moment_count,
        cached_moment_interaction_count: cached_interaction_count,
        cached_surface_omitted_row_count: report.integrity.cached_surface_omitted_row_count,
        verified_external_source_file_count: artifacts.external_paths.len() as u64,
        verified_connector_owned_file_count: artifacts.connector_paths.len() as u64,
        row_equation_holds: true,
        report_matches_archive: true,
        all_artifact_references_resolve: true,
        all_resolved_relationships_resolve: true,
        all_recorded_artifact_files_match: true,
        full_restoration_claimed: report.completion.full_restoration_achieved,
        full_restoration_verified,
        semantic_gap_count: report.integrity.semantic_gap_count,
        message_candidate_gap_count: report.integrity.message_candidate_gap_count,
        missing_artifact_count: report.integrity.missing_artifact_count,
        ambiguous_artifact_count: report.integrity.ambiguous_artifact_count,
        corrupt_artifact_count: report.integrity.corrupt_artifact_count,
        unsafe_artifact_count: report.integrity.unsafe_artifact_count,
        artifact_decode_gap_count: report.integrity.artifact_decode_gap_count,
        entity_decode_gap_count: report.integrity.entity_decode_gap_count,
        unresolved_relationship_count: report.integrity.unresolved_relationship_count,
        account_holder_bound: report.self_participant_id.is_some(),
        direction_conflict_count: messages.direction_conflict_count,
        completion_evidence,
    };
    progress.finish(&result);
    Ok(result)
}

fn verify_database_coverage(report: &RestorationReport) -> Result<(), RestoreError> {
    if report.format_version < 5 {
        if report.archive_scope == RestorationArchiveScope::PartialDatabaseCoverage {
            return Err(integrity(
                "partial database coverage requires archive format 5 evidence",
            ));
        }
        return Ok(());
    }
    let coverage = report
        .database_coverage
        .as_ref()
        .ok_or_else(|| integrity("archive format 5 report has no database coverage evidence"))?;
    if !coverage.is_valid() {
        return Err(integrity("database coverage evidence is inconsistent"));
    }
    match report.archive_scope {
        RestorationArchiveScope::Authoritative if !coverage.authoritative_database_coverage => Err(
            integrity("authoritative archive has incomplete database coverage"),
        ),
        RestorationArchiveScope::PartialDatabaseCoverage
            if coverage.authoritative_database_coverage
                || coverage.attempted_source_set_ids != coverage.snapshot_source_set_ids =>
        {
            Err(integrity(
                "partial archive does not account for the complete database inventory",
            ))
        }
        _ => Ok(()),
    }
}

fn verify_database_record_coverage(
    report: &RestorationReport,
    coverage: &RestorationCoverage,
) -> Result<(), RestoreError> {
    let Some(database_coverage) = report.database_coverage.as_ref() else {
        return Ok(());
    };
    let included = database_coverage.included_source_set_ids();
    if coverage
        .all_tables
        .iter()
        .any(|table| !included.contains(table.source_set_id.as_str()))
    {
        return Err(integrity(
            "archive contains table records outside its included database coverage",
        ));
    }
    Ok(())
}

pub fn verify_recorded_artifact_files(
    archive_directory: &Path,
    artifact: &CanonicalArtifact,
) -> Result<(), RestoreError> {
    ensure_private_directory(archive_directory)?;
    let root = fs::canonicalize(archive_directory)?;
    let mut verified_files = HashMap::new();
    if artifact_has_external_source(artifact) {
        let source_path = required_artifact_path(
            artifact.source_local_path.as_deref(),
            "downloaded artifact lacks its source path",
        )?;
        verify_external_source(artifact, &source_path, &mut verified_files)?;
    }
    if let Some(materialized) = artifact.materialized_local_path.as_deref() {
        verify_connector_file(
            &root,
            &PathBuf::from(materialized),
            artifact.source_byte_count,
            artifact.source_sha256.as_deref(),
            &mut verified_files,
        )?;
    }
    if let Some(decoded) = artifact.decoded_local_path.as_deref() {
        verify_connector_file(
            &root,
            &PathBuf::from(decoded),
            artifact.decoded_byte_count,
            artifact.decoded_sha256.as_deref(),
            &mut verified_files,
        )?;
    }
    Ok(())
}

pub fn validate_canonical_artifact(
    artifact: &CanonicalArtifact,
    coverage: &RestorationCoverage,
) -> Result<(), RestoreError> {
    let covered_tables = coverage
        .all_tables
        .iter()
        .map(|table| {
            (
                (
                    table.source_set_id.as_str(),
                    table.source_logical_path.as_str(),
                    table.source_table_id.as_str(),
                ),
                table,
            )
        })
        .collect::<HashMap<_, _>>();
    validate_artifact_state(artifact, &covered_tables)
}

fn verify_report_paths(root: &Path, report: &RestorationReport) -> Result<(), RestoreError> {
    let required = [
        (&report.messages_path, "messages.ndjson"),
        (&report.rejections_path, "rejections.ndjson"),
        (&report.artifacts_path, "artifacts.ndjson"),
        (&report.conversations_path, "conversations.ndjson"),
        (&report.participants_path, "participants.ndjson"),
        (&report.coverage_path, "coverage.json"),
        (&report.report_path, "report.json"),
    ];
    for (recorded, name) in required {
        verify_recorded_archive_path(root, recorded, name)?;
    }
    let cached = [
        (
            report.cached_moments_path.as_deref(),
            "cached-moments.ndjson",
        ),
        (
            report.cached_moment_interactions_path.as_deref(),
            "cached-moment-interactions.ndjson",
        ),
        (
            report.cached_surfaces_path.as_deref(),
            "cached-surfaces.json",
        ),
    ];
    for (recorded, name) in cached {
        let recorded = recorded
            .ok_or_else(|| integrity("cached-surface path triplet is incomplete in report"))?;
        verify_recorded_archive_path(root, recorded, name)?;
    }
    Ok(())
}

fn verify_storage_evidence(root: &Path, report: &RestorationReport) -> Result<(), RestoreError> {
    let Some(storage) = &report.storage else {
        return Ok(());
    };
    if storage.format_version != 1
        || storage.message_record_count != report.integrity.source_row_count
        || storage.observed_table_record_count != report.integrity.observed_table_row_count
        || storage.estimated_peak_byte_count
            != storage
                .estimated_archive_byte_count
                .saturating_add(storage.estimated_staging_byte_count)
        || storage.required_free_byte_count < storage.estimated_peak_byte_count
        || storage.available_free_byte_count_at_start < storage.required_free_byte_count
        || storage.peak_staging_file_byte_count == 0
        || (storage.message_record_count > 0
            && (storage.staged_uncompressed_byte_count == 0
                || storage.staged_compressed_byte_count == 0))
    {
        return Err(integrity(
            "restoration storage evidence is incomplete or inconsistent",
        ));
    }

    let mut actual_archive_byte_count = 0_u64;
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|error| {
            RestoreError::Io(
                error
                    .into_io_error()
                    .unwrap_or_else(|| std::io::Error::other("could not inspect archive size")),
            )
        })?;
        if entry.file_type().is_symlink() {
            return Err(integrity(
                "a completed restoration archive contains a symbolic link",
            ));
        }
        if entry.depth() == 1
            && entry.file_type().is_dir()
            && entry.file_name().as_bytes().starts_with(b".staging-")
        {
            return Err(integrity(
                "a completed restoration archive retains an ordering spool",
            ));
        }
        if entry.file_type().is_file() {
            ensure_private_regular_file(entry.path())?;
            actual_archive_byte_count = actual_archive_byte_count.saturating_add(
                entry
                    .metadata()
                    .map_err(|error| {
                        RestoreError::Io(error.into_io_error().unwrap_or_else(|| {
                            std::io::Error::other("could not inspect archive file size")
                        }))
                    })?
                    .len(),
            );
        }
    }
    if storage.actual_archive_byte_count != actual_archive_byte_count {
        return Err(integrity(
            "restoration archive byte count does not match storage evidence",
        ));
    }
    Ok(())
}

fn verify_recorded_archive_path(
    root: &Path,
    recorded: &str,
    expected_name: &str,
) -> Result<(), RestoreError> {
    let expected = root.join(expected_name);
    ensure_private_regular_file(&expected)?;
    let actual = fs::canonicalize(recorded)?;
    if actual != expected {
        return Err(integrity(
            "a report path does not identify its archive-owned file",
        ));
    }
    Ok(())
}

fn verify_coverage(
    coverage: &RestorationCoverage,
    report: &RestorationReport,
) -> Result<(), RestoreError> {
    let mut all_table_ids = HashSet::new();
    let mut all_tables = HashMap::new();
    let mut message_table_ids = HashSet::new();
    let mut message_source_ids = HashSet::new();
    let mut schema_counts = BTreeMap::new();
    let mut table_role_counts = BTreeMap::new();
    let mut table_classification_reason_counts = BTreeMap::new();
    let mut observed_table_rows = 0_u64;
    let mut candidate_gaps = 0_u64;
    for table in &coverage.all_tables {
        let identity = (
            table.source_set_id.clone(),
            table.source_logical_path.clone(),
            table.source_table_id.clone(),
        );
        if table.source_set_id.is_empty()
            || table.source_logical_path.is_empty()
            || table.source_table_id.is_empty()
            || table.source_table_name.is_empty()
            || table.columns.iter().any(String::is_empty)
            || table.columns.iter().collect::<HashSet<_>>().len() != table.columns.len()
            || !all_table_ids.insert(identity.clone())
            || all_tables.insert(identity.clone(), table).is_some()
        {
            return Err(integrity("coverage contains a duplicate table identity"));
        }
        let unavailable = table.availability == crate::TableCoverageAvailability::Unavailable;
        if (!unavailable
            && table
                .schema_fingerprint
                .as_deref()
                .is_none_or(|value| !is_lower_hex(value, 64)))
            || (unavailable
                && (table.role != TableCoverageRole::UnhandledMessageCandidate
                    || table.limitation_code.as_deref().is_none_or(str::is_empty)))
        {
            return Err(integrity(
                "coverage table lacks a complete schema fingerprint",
            ));
        }
        if coverage.format_version >= 4 {
            if unavailable {
                if table.source_row_count.is_some() {
                    return Err(integrity(
                        "unavailable coverage table claims a complete observed row count",
                    ));
                }
            } else {
                observed_table_rows = observed_table_rows.saturating_add(
                    table
                        .source_row_count
                        .ok_or_else(|| integrity("coverage table lacks its observed row count"))?,
                );
            }
        }
        let role_name = match table.role {
            TableCoverageRole::Message => "message",
            TableCoverageRole::KnownAuxiliary => "knownAuxiliary",
            TableCoverageRole::Other => "other",
            TableCoverageRole::UnhandledMessageCandidate => "unhandledMessageCandidate",
        };
        *table_role_counts.entry(role_name.to_string()).or_default() += 1;
        *table_classification_reason_counts
            .entry(table.classification_reason.clone())
            .or_default() += 1;
        if table.availability == crate::TableCoverageAvailability::Partial {
            if table.limitation_code.as_deref().is_none_or(str::is_empty) {
                return Err(integrity(
                    "partially readable coverage table lacks a limitation code",
                ));
            }
            candidate_gaps = candidate_gaps.saturating_add(1);
        }
        match table.role {
            TableCoverageRole::Message => {
                message_table_ids.insert(identity);
                if !message_source_ids
                    .insert((table.source_set_id.clone(), table.source_table_id.clone()))
                {
                    return Err(integrity(
                        "message coverage contains an ambiguous source-table identity",
                    ));
                }
                *schema_counts
                    .entry(table.schema_fingerprint.clone().unwrap())
                    .or_default() += 1;
            }
            TableCoverageRole::UnhandledMessageCandidate => {
                if table.availability != crate::TableCoverageAvailability::Partial {
                    candidate_gaps = candidate_gaps.saturating_add(1);
                }
            }
            TableCoverageRole::KnownAuxiliary | TableCoverageRole::Other => {}
        }
    }
    let covered_message_ids = coverage
        .message_tables
        .iter()
        .map(|table| {
            (
                table.source_set_id.clone(),
                table.source_logical_path.clone(),
                table.source_table_id.clone(),
            )
        })
        .collect::<HashSet<_>>();
    if covered_message_ids.len() != coverage.message_tables.len()
        || covered_message_ids != message_table_ids
    {
        return Err(integrity(
            "message-table coverage does not match the complete table ledger",
        ));
    }
    for table in &coverage.message_tables {
        let identity = (
            table.source_set_id.clone(),
            table.source_logical_path.clone(),
            table.source_table_id.clone(),
        );
        let complete = all_tables
            .get(&identity)
            .ok_or_else(|| integrity("message table is absent from complete coverage"))?;
        if complete.source_table_name != table.source_table_name
            || complete.columns != table.columns
            || complete.schema_fingerprint != table.schema_fingerprint
            || (coverage.format_version >= 4
                && complete.source_row_count != Some(table.source_row_count))
        {
            return Err(integrity(
                "message-table provenance disagrees with complete coverage",
            ));
        }
    }
    let source_rows = coverage
        .message_tables
        .iter()
        .map(|table| table.source_row_count)
        .sum::<u64>();
    if source_rows != report.integrity.source_row_count
        || coverage.message_tables.len() as u64 != report.integrity.message_table_count
        || candidate_gaps != report.integrity.message_candidate_gap_count
        || schema_counts != report.integrity.message_schema_counts
    {
        return Err(integrity(
            "coverage counts do not match restoration integrity",
        ));
    }
    if coverage.format_version >= 4
        && (observed_table_rows != report.integrity.observed_table_row_count
            || table_role_counts != report.integrity.table_role_counts
            || table_classification_reason_counts
                != report.integrity.table_classification_reason_counts)
    {
        return Err(integrity(
            "complete table-ledger counts do not match restoration integrity",
        ));
    }
    let profile = schema_profile_fingerprint(coverage.all_tables.iter().map(|table| {
        (
            table.source_logical_path.as_str(),
            table.source_table_name.as_str(),
            table.schema_fingerprint.as_deref(),
        )
    }));
    if coverage.schema_profile_fingerprint != profile {
        return Err(integrity(
            "coverage schema-profile fingerprint is inconsistent",
        ));
    }
    Ok(())
}

fn audit_messages(
    path: &Path,
    report: &RestorationReport,
    coverage: &RestorationCoverage,
    progress: &mut ArchiveAuditProgress<'_>,
) -> Result<MessageAudit, RestoreError> {
    let mut result = MessageAudit::default();
    let covered_tables = coverage
        .message_tables
        .iter()
        .map(|table| {
            (
                (
                    table.source_set_id.as_str(),
                    table.source_logical_path.as_str(),
                    table.source_table_id.as_str(),
                ),
                table,
            )
        })
        .collect::<HashMap<_, _>>();
    let mut previous_conversation: Option<String> = None;
    let mut expected_ordinal = 0_u64;
    let mut conversation_basis = None;
    read_ndjson(path, progress, |message: CanonicalMessage| {
        if message.canonical_id.is_empty()
            || message.account_id != report.account_id
            || message.conversation_id.is_empty()
        {
            return Err(integrity("message identity or account binding is invalid"));
        }
        validate_base64(&message.conversation_source_identifier_base64)?;
        validate_scoped_identifier(
            &report.account_id,
            Some(&message.conversation_source_identifier_base64),
            Some(&message.conversation_id),
            "message conversation",
        )?;
        validate_scoped_identifier(
            &report.account_id,
            message.sender_source_identifier_base64.as_deref(),
            message.sender_id.as_deref(),
            "message sender",
        )?;
        validate_optional_base64(message.content_base64.as_deref())?;
        validate_optional_base64(message.packed_info_base64.as_deref())?;
        validate_raw_columns(&message.raw_columns)?;
        let source_table = covered_tables
            .get(&(
                message.source_set_id.as_str(),
                message.source_logical_path.as_str(),
                message.source_table_id.as_str(),
            ))
            .ok_or_else(|| integrity("message provenance is absent from table coverage"))?;
        audit_message_direction(&message, report, &source_table.columns, &mut result)?;
        let raw_column_names = message
            .raw_columns
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let covered_column_names = source_table
            .columns
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if message.source_table_name != source_table.source_table_name
            || raw_column_names != covered_column_names
        {
            return Err(integrity(
                "message row provenance disagrees with its covered source table",
            ));
        }
        if !result.canonical_ids.insert(message.canonical_id.clone())
            || !result.source_identities.insert((
                message.source_set_id.clone(),
                message.source_table_id.clone(),
                message.source_row_id,
            ))
        {
            return Err(integrity("message archive contains a duplicate identity"));
        }
        *result
            .source_table_counts
            .entry((
                message.source_set_id.clone(),
                message.source_table_id.clone(),
            ))
            .or_default() += 1;
        match previous_conversation.as_deref() {
            Some(previous) if previous == message.conversation_id => {
                expected_ordinal += 1;
                if conversation_basis != Some(message.ordering_basis) {
                    return Err(integrity(
                        "one conversation uses inconsistent ordering bases",
                    ));
                }
            }
            Some(previous) if previous < message.conversation_id.as_str() => {
                expected_ordinal = 0;
                conversation_basis = Some(message.ordering_basis);
            }
            None => {
                expected_ordinal = 0;
                conversation_basis = Some(message.ordering_basis);
            }
            _ => {
                return Err(integrity(
                    "message conversations are not in deterministic order",
                ))
            }
        }
        if message.conversation_ordinal != expected_ordinal {
            return Err(integrity(
                "message conversation ordinals are not contiguous",
            ));
        }
        previous_conversation = Some(message.conversation_id.clone());

        result.canonical_conversations.insert(
            message.canonical_id.clone(),
            message.conversation_id.clone(),
        );
        result
            .conversation_ids
            .insert(message.conversation_id.clone());
        if result
            .conversation_sources
            .insert(
                message.conversation_id.clone(),
                message.conversation_source_identifier_base64.clone(),
            )
            .is_some_and(|value| value != message.conversation_source_identifier_base64)
        {
            return Err(integrity(
                "one conversation has inconsistent source identifiers",
            ));
        }
        if let Some(sender) = &message.sender_id {
            result.participant_ids.insert(sender.clone());
            if result
                .participant_sources
                .insert(
                    sender.clone(),
                    message.sender_source_identifier_base64.clone().unwrap(),
                )
                .is_some_and(|value| {
                    Some(value.as_str()) != message.sender_source_identifier_base64.as_deref()
                })
            {
                return Err(integrity(
                    "one message sender has inconsistent source identifiers",
                ));
            }
            result
                .participant_memberships
                .push((message.conversation_id.clone(), sender.clone()));
        }
        let mut message_artifacts = HashSet::new();
        for reference in &message.artifact_references {
            if !message_artifacts.insert(reference.artifact_id.clone()) {
                return Err(integrity(
                    "one message contains a duplicate artifact reference",
                ));
            }
            result.artifact_ids.insert(reference.artifact_id.clone());
            result
                .artifact_references
                .push((reference.artifact_id.clone(), reference.role));
        }
        let media_bearing = message_requires_artifact(&message);
        if media_bearing != !message.artifact_references.is_empty() {
            return Err(integrity(
                "message artifact references disagree with its logical media type",
            ));
        }
        if !message.artifact_references.is_empty()
            && message
                .artifact_references
                .iter()
                .filter(|value| value.preferred)
                .count()
                != 1
        {
            return Err(integrity(
                "media-bearing message does not have exactly one preferred artifact",
            ));
        }
        result.artifact_reference_count += message.artifact_references.len() as u64;

        for relationship in &message.relationships {
            validate_optional_base64(relationship.raw_reference_base64.as_deref())?;
            let state_resolved =
                relationship.resolution_state == RelationshipResolutionState::Resolved;
            if relationship.resolved != state_resolved {
                return Err(integrity(
                    "relationship resolved flag disagrees with its state",
                ));
            }
            match relationship.resolution_state {
                RelationshipResolutionState::Resolved => {
                    let target = relationship.target_canonical_id.clone().ok_or_else(|| {
                        integrity("resolved relationship lacks a canonical target")
                    })?;
                    result
                        .resolved_relationships
                        .push((message.conversation_id.clone(), target));
                    result.resolved_relationship_count += 1;
                }
                RelationshipResolutionState::TargetNotPresentLocally => {
                    if relationship.target_canonical_id.is_some()
                        || (relationship.target_server_id.is_none()
                            && relationship.target_local_id.is_none())
                    {
                        return Err(integrity(
                            "absent relationship target has inconsistent identity evidence",
                        ));
                    }
                    result.absent_relationship_target_count += 1;
                }
                RelationshipResolutionState::ReferenceIdentifierMissing => {
                    if relationship.target_canonical_id.is_some()
                        || relationship.target_server_id.is_some()
                        || relationship.target_local_id.is_some()
                    {
                        return Err(integrity(
                            "missing relationship identifier has unexpected target evidence",
                        ));
                    }
                    result.missing_relationship_identifier_count += 1;
                }
                RelationshipResolutionState::Ambiguous => {
                    if relationship.target_canonical_id.is_some()
                        || (relationship.target_server_id.is_none()
                            && relationship.target_local_id.is_none())
                    {
                        return Err(integrity(
                            "ambiguous relationship has inconsistent identity evidence",
                        ));
                    }
                    result.ambiguous_relationship_count += 1;
                }
                RelationshipResolutionState::Pending => {
                    if relationship.target_canonical_id.is_some() {
                        return Err(integrity(
                            "pending relationship unexpectedly has a canonical target",
                        ));
                    }
                    result.pending_relationship_count += 1;
                }
            }
        }
        result.relationship_reference_count += message.relationships.len() as u64;

        let logical = message
            .logical_type
            .map(|value| value.to_string())
            .unwrap_or_else(|| "missing".to_string());
        *result.logical_type_counts.entry(logical).or_default() += 1;
        let subtype = match (message.logical_type, message.sub_type) {
            (Some(logical), Some(subtype)) => format!("{logical}:{subtype}"),
            _ => "missing".to_string(),
        };
        *result.logical_sub_type_counts.entry(subtype).or_default() += 1;
        crate::nested_xml::validate_canonical_message(&message).map_err(integrity)?;
        if let TypedPayload::Unknown { reason } = &message.typed_payload {
            *result
                .unknown_payload_reason_counts
                .entry(reason.clone())
                .or_default() += 1;
        }
        if message.semantic_decode_state != SemanticDecodeState::Complete {
            let reason = message
                .semantic_gap_reason
                .clone()
                .unwrap_or_else(|| "unspecified semantic coverage gap".to_string());
            *result.semantic_gap_reason_counts.entry(reason).or_default() += 1;
        }
        *result
            .direction_counts
            .entry(format!("{:?}", message.direction).to_ascii_lowercase())
            .or_default() += 1;
        let basis = match message.ordering_basis {
            crate::MessageOrderingBasis::SortSequence => "sortSequence",
            crate::MessageOrderingBasis::ServerId => "serverId",
            crate::MessageOrderingBasis::CreatedAt => "createdAt",
            crate::MessageOrderingBasis::LocalId => "localId",
            crate::MessageOrderingBasis::HybridSourceFallback => "hybridSourceFallback",
        };
        *result
            .ordering_basis_counts
            .entry(basis.to_string())
            .or_default() += 1;
        result.count += 1;
        Ok(())
    })?;

    for (conversation, target) in &result.resolved_relationships {
        let target_conversation = result
            .canonical_conversations
            .get(target)
            .ok_or_else(|| integrity("resolved relationship target is absent"))?;
        if target_conversation != conversation {
            return Err(integrity(
                "resolved relationship crosses conversation scope",
            ));
        }
    }
    Ok(result)
}

fn verify_account_holder_binding(report: &RestorationReport) -> Result<(), RestoreError> {
    if report.format_version >= 6 {
        if report
            .self_participant_id
            .as_deref()
            .is_none_or(|identifier| !is_lower_hex(identifier, 64))
            || report.account_binding_evidence.is_none()
        {
            return Err(integrity(
                "archive format 6 lacks a valid account-holder binding",
            ));
        }
    } else if report.self_participant_id.is_some() || report.account_binding_evidence.is_some() {
        return Err(integrity(
            "legacy archive unexpectedly contains account-holder binding evidence",
        ));
    }
    Ok(())
}

fn audit_message_direction(
    message: &CanonicalMessage,
    report: &RestorationReport,
    source_columns: &[String],
    result: &mut MessageAudit,
) -> Result<(), RestoreError> {
    if report.format_version < 6 {
        return Ok(());
    }
    let self_participant_id = report
        .self_participant_id
        .as_deref()
        .ok_or_else(|| integrity("account-holder identity disappeared during audit"))?;
    if message.logical_type == Some(49) && message.sub_type == Some(62) {
        if let Some(expected_sender) = crate::restore::typed_payload_raw_xml(&message.typed_payload)
            .and_then(|raw_xml| {
                crate::nested_xml::unique_identifier_element_text(raw_xml, "fromusername")
            })
        {
            let observed_sender = message
                .sender_source_identifier_base64
                .as_deref()
                .and_then(|value| base64::engine::general_purpose::STANDARD.decode(value).ok())
                .and_then(|bytes| String::from_utf8(bytes).ok());
            if observed_sender.as_deref() != Some(expected_sender.as_str()) {
                return Err(integrity(
                    "Pat message sender disagrees with its source XML",
                ));
            }
        }
    }
    let explicit = explicit_direction(&message.raw_columns, source_columns);
    match message.sender_id.as_deref() {
        Some(sender) => {
            let expected = if sender == self_participant_id {
                MessageDirection::Outgoing
            } else {
                MessageDirection::Incoming
            };
            if message.direction != expected {
                return Err(integrity(
                    "message direction disagrees with the bound account holder",
                ));
            }
            let conflict = explicit.is_some_and(|direction| direction != expected);
            let expected_evidence = if conflict {
                DirectionEvidence::SenderAccountConflictWithExplicitSourceColumn
            } else if expected == MessageDirection::Outgoing {
                DirectionEvidence::SenderMatchesAccount
            } else {
                DirectionEvidence::SenderDiffersFromAccount
            };
            if message.direction_evidence != expected_evidence {
                return Err(integrity(
                    "message direction evidence disagrees with its source columns",
                ));
            }
            if conflict {
                result.direction_conflict_count += 1;
            }
        }
        None => match explicit {
            Some(direction)
                if message.direction == direction
                    && message.direction_evidence == DirectionEvidence::ExplicitSourceColumn => {}
            None if message.direction == MessageDirection::Unknown
                && message.direction_evidence == DirectionEvidence::Unresolved => {}
            _ => {
                return Err(integrity(
                    "sender-less message direction lacks consistent source evidence",
                ))
            }
        },
    }
    Ok(())
}

fn explicit_direction(
    values: &BTreeMap<String, RawSQLiteValue>,
    source_columns: &[String],
) -> Option<MessageDirection> {
    const NAMES: [&str; 4] = ["is_sender", "is_sender_", "is_send", "is_sent_by_self"];
    let name = source_columns.iter().find(|name| {
        NAMES
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
    })?;
    raw_i64(values.get(name)?).map(|value| {
        if value == 0 {
            MessageDirection::Incoming
        } else {
            MessageDirection::Outgoing
        }
    })
}

fn raw_i64(value: &RawSQLiteValue) -> Option<i64> {
    match value {
        RawSQLiteValue::Integer(value) => Some(*value),
        RawSQLiteValue::Real(value) => Some(*value as i64),
        RawSQLiteValue::TextBase64(value) => base64::engine::general_purpose::STANDARD
            .decode(value)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|value| value.parse().ok()),
        RawSQLiteValue::Null | RawSQLiteValue::BlobBase64(_) => None,
    }
}

fn audit_rejections(
    path: &Path,
    progress: &mut ArchiveAuditProgress<'_>,
) -> Result<RejectionAudit, RestoreError> {
    let mut result = RejectionAudit::default();
    read_ndjson(path, progress, |row: RejectedRow| {
        if row.source_set_id.is_empty()
            || row.source_table_id.is_empty()
            || row.reason.is_empty()
            || !result.source_identities.insert((
                row.source_set_id.clone(),
                row.source_table_id.clone(),
                row.source_row_id,
            ))
        {
            return Err(integrity(
                "rejection ledger contains an invalid or duplicate row",
            ));
        }
        *result
            .source_table_counts
            .entry((row.source_set_id, row.source_table_id))
            .or_default() += 1;
        result.count += 1;
        Ok(())
    })?;
    Ok(result)
}

fn verify_source_row_accounting(
    coverage: &RestorationCoverage,
    messages: &MessageAudit,
    rejections: &RejectionAudit,
) -> Result<(), RestoreError> {
    let expected = coverage
        .message_tables
        .iter()
        .map(|table| {
            (
                (table.source_set_id.clone(), table.source_table_id.clone()),
                table.source_row_count,
            )
        })
        .collect::<HashMap<_, _>>();
    if messages
        .source_table_counts
        .keys()
        .chain(rejections.source_table_counts.keys())
        .any(|identity| !expected.contains_key(identity))
    {
        return Err(integrity(
            "a restored or rejected row belongs to an uncovered message table",
        ));
    }
    for (identity, expected_count) in expected {
        let restored = messages
            .source_table_counts
            .get(&identity)
            .copied()
            .unwrap_or_default();
        let rejected = rejections
            .source_table_counts
            .get(&identity)
            .copied()
            .unwrap_or_default();
        if restored + rejected != expected_count {
            return Err(integrity(
                "per-table source row accounting does not match coverage",
            ));
        }
    }
    if rejections.source_identities.iter().any(|identity| {
        identity.2.is_some_and(|row_id| {
            messages
                .source_identities
                .contains(&(identity.0.clone(), identity.1.clone(), row_id))
        })
    }) {
        return Err(integrity(
            "one source row appears in both restored and rejected ledgers",
        ));
    }
    Ok(())
}

fn audit_artifacts(
    root: &Path,
    path: &Path,
    coverage: &RestorationCoverage,
    progress: &mut ArchiveAuditProgress<'_>,
) -> Result<ArtifactAudit, RestoreError> {
    let mut result = ArtifactAudit::default();
    let mut verified_files = HashMap::<PathBuf, VerifiedFile>::new();
    let covered_tables = coverage
        .all_tables
        .iter()
        .map(|table| {
            (
                (
                    table.source_set_id.as_str(),
                    table.source_logical_path.as_str(),
                    table.source_table_id.as_str(),
                ),
                table,
            )
        })
        .collect::<HashMap<_, _>>();
    read_ndjson(path, progress, |artifact: CanonicalArtifact| {
        if artifact.artifact_id.is_empty()
            || !result.identifiers.insert(artifact.artifact_id.clone())
        {
            return Err(integrity(
                "artifact ledger contains a duplicate or empty identity",
            ));
        }
        let mut roles = artifact.roles.clone();
        if roles.is_empty() {
            // Archives written before per-artifact role sets record a single
            // role; treat it as the complete set.
            roles.insert(artifact.role);
        } else if !roles.contains(&artifact.role) {
            return Err(integrity(
                "artifact role set does not contain its primary role",
            ));
        }
        result.roles.insert(artifact.artifact_id.clone(), roles);
        result.count += 1;
        if artifact
            .source_md5
            .as_deref()
            .is_some_and(|value| !is_lower_hex(value, 32))
            || artifact
                .source_sha256
                .as_deref()
                .is_some_and(|value| !is_lower_hex(value, 64))
            || artifact
                .decoded_sha256
                .as_deref()
                .is_some_and(|value| !is_lower_hex(value, 64))
        {
            return Err(integrity("artifact ledger contains a malformed digest"));
        }
        validate_artifact_state(&artifact, &covered_tables)?;
        match artifact.availability {
            ArtifactAvailability::Downloaded => result.downloaded += 1,
            ArtifactAvailability::MaterializedFromDatabase => result.materialized += 1,
            ArtifactAvailability::NotDownloaded
            | ArtifactAvailability::RemoteOnly
            | ArtifactAvailability::Expired
            | ArtifactAvailability::Deleted
            | ArtifactAvailability::MetadataMissing => result.missing += 1,
            ArtifactAvailability::AccountRootUnavailable => {
                result.missing += 1;
                result.account_root_unavailable += 1;
            }
            ArtifactAvailability::Ambiguous => result.ambiguous += 1,
            ArtifactAvailability::Corrupt => result.corrupt += 1,
            ArtifactAvailability::UnsafePath => result.unsafe_count += 1,
        }

        if artifact_has_external_source(&artifact) {
            let source_path = required_artifact_path(
                artifact.source_local_path.as_deref(),
                "downloaded artifact lacks its source path",
            )?;
            verify_external_source(&artifact, &source_path, &mut verified_files)?;
            result.external_paths.insert(source_path);
        }
        if artifact.materialized_local_path.is_some() {
            let materialized = required_artifact_path(
                artifact.materialized_local_path.as_deref(),
                "database artifact lacks its materialized path",
            )?;
            verify_connector_file(
                root,
                &materialized,
                artifact.source_byte_count,
                artifact.source_sha256.as_deref(),
                &mut verified_files,
            )?;
            result.connector_paths.insert(materialized);
        }
        if artifact.decode_state == ArtifactDecodeState::Decoded {
            let decoded = required_artifact_path(
                artifact.decoded_local_path.as_deref(),
                "decoded artifact lacks its derivative path",
            )?;
            verify_connector_file(
                root,
                &decoded,
                artifact.decoded_byte_count,
                artifact.decoded_sha256.as_deref(),
                &mut verified_files,
            )?;
            if artifact.decoded_format.as_deref().is_none_or(str::is_empty) {
                return Err(integrity("decoded artifact lacks a detected output format"));
            }
            result.connector_paths.insert(decoded);
            result.decoded += 1;
        } else if artifact.decoded_local_path.is_some()
            || artifact.decoded_byte_count.is_some()
            || artifact.decoded_sha256.is_some()
            || artifact.decoded_format.is_some()
        {
            return Err(integrity(
                "non-decoded artifact unexpectedly records a derivative",
            ));
        }
        if matches!(
            artifact.availability,
            ArtifactAvailability::Downloaded
                | ArtifactAvailability::MaterializedFromDatabase
                | ArtifactAvailability::Ambiguous
        ) && matches!(
            artifact.decode_state,
            ArtifactDecodeState::KeyUnavailable
                | ArtifactDecodeState::Unsupported
                | ArtifactDecodeState::Failed
        ) {
            result.decode_gaps += 1;
        }
        Ok(())
    })?;
    Ok(result)
}

fn validate_artifact_state(
    artifact: &CanonicalArtifact,
    covered_tables: &HashMap<(&str, &str, &str), &crate::TableSchemaCoverage>,
) -> Result<(), RestoreError> {
    if artifact
        .verification_detail
        .as_deref()
        .is_none_or(str::is_empty)
    {
        return Err(integrity("artifact lacks a nonempty verification detail"));
    }
    if artifact
        .source_modified_nanoseconds
        .is_some_and(|value| !(0..1_000_000_000).contains(&value))
    {
        return Err(integrity(
            "artifact source timestamp has invalid nanoseconds",
        ));
    }

    validate_artifact_resource_provenance(artifact, covered_tables)?;

    let external_complete = artifact.source_local_path.is_some()
        && artifact.account_relative_path.is_some()
        && artifact.source_byte_count.is_some()
        && artifact.source_device_id.is_some()
        && artifact.source_file_id.is_some()
        && artifact.source_modified_seconds.is_some()
        && artifact.source_modified_nanoseconds.is_some()
        && artifact.source_sha256.is_some()
        && artifact
            .detected_format
            .as_deref()
            .is_some_and(|value| !value.is_empty());
    let external_present = artifact_has_external_source(artifact);
    let materialized_complete = artifact.materialized_local_path.is_some()
        && artifact.source_byte_count.is_some()
        && artifact.source_sha256.is_some()
        && artifact
            .detected_format
            .as_deref()
            .is_some_and(|value| !value.is_empty());
    let materialized_present = artifact.materialized_local_path.is_some();
    let external_identity_present = artifact.source_local_path.is_some()
        || artifact.account_relative_path.is_some()
        || artifact.source_device_id.is_some()
        || artifact.source_file_id.is_some()
        || artifact.source_modified_seconds.is_some()
        || artifact.source_modified_nanoseconds.is_some();

    match artifact.availability {
        ArtifactAvailability::Downloaded => {
            if !external_complete || materialized_present {
                return Err(integrity(
                    "downloaded artifact has incomplete or contradictory source evidence",
                ));
            }
        }
        ArtifactAvailability::MaterializedFromDatabase => {
            if !materialized_complete || external_identity_present {
                return Err(integrity(
                    "database-materialized artifact has incomplete or contradictory source evidence",
                ));
            }
        }
        ArtifactAvailability::Ambiguous | ArtifactAvailability::Corrupt => {
            if external_complete == materialized_complete
                || (external_present && !external_complete)
                || (materialized_present && !materialized_complete)
                || (materialized_complete && external_identity_present)
            {
                return Err(integrity(
                    "ambiguous or corrupt artifact does not identify exactly one complete local source",
                ));
            }
        }
        ArtifactAvailability::NotDownloaded
        | ArtifactAvailability::RemoteOnly
        | ArtifactAvailability::Expired
        | ArtifactAvailability::Deleted
        | ArtifactAvailability::MetadataMissing
        | ArtifactAvailability::UnsafePath
        | ArtifactAvailability::AccountRootUnavailable => {
            if external_present
                || materialized_present
                || artifact.source_byte_count.is_some()
                || artifact.source_sha256.is_some()
                || artifact.detected_format.is_some()
            {
                return Err(integrity(
                    "unavailable artifact unexpectedly retains verified local-file evidence",
                ));
            }
        }
    }

    let source_is_verified = matches!(
        artifact.availability,
        ArtifactAvailability::Downloaded
            | ArtifactAvailability::MaterializedFromDatabase
            | ArtifactAvailability::Ambiguous
    );
    let source_is_present =
        source_is_verified || artifact.availability == ArtifactAvailability::Corrupt;
    match artifact.decode_state {
        ArtifactDecodeState::Decoded => {
            if !source_is_verified
                || artifact.decoded_local_path.is_none()
                || artifact.decoded_byte_count.is_none()
                || artifact.decoded_sha256.is_none()
                || artifact.decoded_format.as_deref().is_none_or(str::is_empty)
            {
                return Err(integrity(
                    "decoded artifact has incomplete or incompatible derivative evidence",
                ));
            }
        }
        ArtifactDecodeState::KeyUnavailable => {
            if !source_is_verified
                || !matches!(
                    artifact.kind,
                    ArtifactKind::Image | ArtifactKind::AnimatedImage
                )
            {
                return Err(integrity(
                    "artifact key-unavailable state is incompatible with its source or media kind",
                ));
            }
        }
        ArtifactDecodeState::Unsupported | ArtifactDecodeState::Failed => {
            if !source_is_present {
                return Err(integrity(
                    "artifact decode gap has no verified local source",
                ));
            }
        }
        ArtifactDecodeState::NotRequired => {
            if artifact.kind == ArtifactKind::Voice
                && matches!(
                    artifact.availability,
                    ArtifactAvailability::MaterializedFromDatabase
                        | ArtifactAvailability::Ambiguous
                )
            {
                return Err(integrity(
                    "materialized voice artifact lacks an explicit decode outcome",
                ));
            }
        }
    }

    if matches!(
        artifact.kind,
        ArtifactKind::Image | ArtifactKind::AnimatedImage
    ) && artifact
        .source_local_path
        .as_deref()
        .and_then(|path| Path::new(path).extension())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dat"))
        && artifact.decode_state == ArtifactDecodeState::NotRequired
    {
        return Err(integrity(
            "encoded image source lacks an explicit decode outcome",
        ));
    }
    Ok(())
}

fn validate_artifact_resource_provenance(
    artifact: &CanonicalArtifact,
    covered_tables: &HashMap<(&str, &str, &str), &crate::TableSchemaCoverage>,
) -> Result<(), RestoreError> {
    let present = [
        artifact.source_resource_set_id.is_some(),
        artifact.source_resource_logical_path.is_some(),
        artifact.source_resource_table_id.is_some(),
        artifact.source_resource_table_name.is_some(),
        artifact.source_resource_row_id.is_some(),
    ];
    let present_count = present.into_iter().filter(|value| *value).count();
    if present_count == 0 {
        if artifact.availability == ArtifactAvailability::MaterializedFromDatabase {
            return Err(integrity(
                "database-materialized artifact lacks source-row provenance",
            ));
        }
        return Ok(());
    }
    if present_count != present.len()
        || artifact
            .source_resource_row_id
            .is_none_or(|row_id| row_id <= 0)
    {
        return Err(integrity(
            "artifact source-row provenance is incomplete or invalid",
        ));
    }
    let table = covered_tables
        .get(&(
            artifact.source_resource_set_id.as_deref().unwrap(),
            artifact.source_resource_logical_path.as_deref().unwrap(),
            artifact.source_resource_table_id.as_deref().unwrap(),
        ))
        .ok_or_else(|| integrity("artifact source-row provenance is absent from coverage"))?;
    if table.source_table_name != artifact.source_resource_table_name.as_deref().unwrap()
        || table.role != TableCoverageRole::KnownAuxiliary
        || !matches!(
            table.source_table_name.to_ascii_lowercase().as_str(),
            "messageresourceinfo" | "voiceinfo"
        )
    {
        return Err(integrity(
            "artifact source-row provenance does not identify a covered resource table",
        ));
    }
    if artifact.availability == ArtifactAvailability::MaterializedFromDatabase
        && !table.source_table_name.eq_ignore_ascii_case("VoiceInfo")
    {
        return Err(integrity(
            "database-materialized artifact provenance does not identify VoiceInfo",
        ));
    }
    Ok(())
}

fn artifact_has_external_source(artifact: &CanonicalArtifact) -> bool {
    artifact.source_local_path.is_some()
        || artifact.account_relative_path.is_some()
        || artifact.source_device_id.is_some()
        || artifact.source_file_id.is_some()
        || artifact.source_modified_seconds.is_some()
        || artifact.source_modified_nanoseconds.is_some()
}

fn message_requires_artifact(message: &CanonicalMessage) -> bool {
    matches!(
        (message.logical_type, message.sub_type.unwrap_or_default()),
        (Some(3 | 34 | 43 | 47), _) | (Some(49), 2 | 3 | 4 | 6 | 8 | 51 | 63 | 74)
    )
}

fn verify_external_source(
    artifact: &CanonicalArtifact,
    path: &Path,
    cache: &mut HashMap<PathBuf, VerifiedFile>,
) -> Result<(), RestoreError> {
    if !path.is_absolute() || fs::canonicalize(path)? != path {
        return Err(integrity(
            "artifact source path is not an absolute canonical path",
        ));
    }
    let relative = required_artifact_path(
        artifact.account_relative_path.as_deref(),
        "downloaded artifact lacks its account-relative path",
    )?;
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || !path.ends_with(&relative)
    {
        return Err(integrity(
            "artifact account-relative path is unsafe or mismatched",
        ));
    }
    let verified = verified_file(path, false, cache)?;
    if Some(verified.byte_count) != artifact.source_byte_count
        || Some(verified.device_id) != artifact.source_device_id
        || Some(verified.file_id) != artifact.source_file_id
        || Some(verified.modified_seconds) != artifact.source_modified_seconds
        || Some(verified.modified_nanoseconds) != artifact.source_modified_nanoseconds
        || Some(verified.sha256.as_str()) != artifact.source_sha256.as_deref()
    {
        return Err(integrity(
            "artifact source file no longer matches recorded evidence",
        ));
    }
    Ok(())
}

fn verify_connector_file(
    root: &Path,
    path: &Path,
    expected_bytes: Option<u64>,
    expected_sha256: Option<&str>,
    cache: &mut HashMap<PathBuf, VerifiedFile>,
) -> Result<(), RestoreError> {
    if !path.is_absolute() {
        return Err(integrity(
            "connector-owned artifact path escapes the archive",
        ));
    }
    let canonical = fs::canonicalize(path)?;
    if !canonical.starts_with(root) {
        return Err(integrity(
            "connector-owned artifact path escapes the archive",
        ));
    }
    ensure_no_symlink_components(root, &canonical)?;
    let verified = verified_file(&canonical, true, cache)?;
    if Some(verified.byte_count) != expected_bytes
        || Some(verified.sha256.as_str()) != expected_sha256
    {
        return Err(integrity(
            "connector-owned artifact file fails digest or size verification",
        ));
    }
    Ok(())
}

fn verified_file(
    path: &Path,
    owner_only: bool,
    cache: &mut HashMap<PathBuf, VerifiedFile>,
) -> Result<VerifiedFile, RestoreError> {
    if let Some(value) = cache.get(path) {
        return Ok(value.clone());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let before = file.metadata()?;
    if !before.is_file()
        || (owner_only && (before.permissions().mode() & 0o077 != 0 || before.nlink() != 1))
    {
        return Err(integrity(
            "recorded artifact is not an acceptable regular file",
        ));
    }
    let mut hasher = Sha256::new();
    let mut byte_count = 0_u64;
    let mut buffer = vec![0_u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        byte_count += count as u64;
    }
    let after = file.metadata()?;
    if !same_file_version(&before, &after) || byte_count != before.len() {
        return Err(integrity("recorded artifact changed while it was audited"));
    }
    let value = VerifiedFile {
        byte_count,
        sha256: hex::encode(hasher.finalize()),
        device_id: before.dev(),
        file_id: before.ino(),
        modified_seconds: before.mtime(),
        modified_nanoseconds: before.mtime_nsec(),
    };
    cache.insert(path.to_path_buf(), value.clone());
    Ok(value)
}

fn ensure_no_symlink_components(root: &Path, path: &Path) -> Result<(), RestoreError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| integrity("connector-owned path escapes its archive root"))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(integrity("connector-owned path has a non-normal component"));
        }
        current.push(component.as_os_str());
        if fs::symlink_metadata(&current)?.file_type().is_symlink() {
            return Err(integrity("connector-owned path traverses a symlink"));
        }
    }
    Ok(())
}

fn verify_entities(
    conversations: &HashMap<String, CanonicalConversation>,
    participants: &HashMap<String, CanonicalParticipant>,
    report: &RestorationReport,
    coverage: &RestorationCoverage,
) -> Result<(), RestoreError> {
    let covered_tables = coverage
        .all_tables
        .iter()
        .map(|table| {
            (
                (
                    table.source_set_id.as_str(),
                    table.source_logical_path.as_str(),
                    table.source_table_id.as_str(),
                ),
                table,
            )
        })
        .collect::<HashMap<_, _>>();
    let mut entity_source_rows = HashSet::new();
    for (identifier, conversation) in conversations {
        if identifier.is_empty()
            || conversation.account_id != report.account_id
            || conversation
                .participant_ids
                .iter()
                .collect::<HashSet<_>>()
                .len()
                != conversation.participant_ids.len()
        {
            return Err(integrity(
                "conversation entity is invalid or contains duplicate members",
            ));
        }
        validate_base64(&conversation.source_identifier_base64)?;
        validate_scoped_identifier(
            &report.account_id,
            Some(&conversation.source_identifier_base64),
            Some(identifier),
            "conversation",
        )?;
        for source in &conversation.source_records {
            validate_entity_source_record(source, &covered_tables, &mut entity_source_rows)?;
        }
        for participant in &conversation.participant_ids {
            if !participants.contains_key(participant) {
                return Err(integrity("conversation references an absent participant"));
            }
        }
        for membership in &conversation.memberships {
            validate_optional_base64(membership.display_name_base64.as_deref())?;
            if !participants.contains_key(&membership.participant_id)
                || !conversation
                    .participant_ids
                    .contains(&membership.participant_id)
            {
                return Err(integrity(
                    "conversation membership is not bidirectionally valid",
                ));
            }
        }
        let participant_ids = conversation
            .participant_ids
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let membership_ids = conversation
            .memberships
            .iter()
            .map(|membership| membership.participant_id.clone())
            .collect::<HashSet<_>>();
        let unique_memberships = conversation
            .memberships
            .iter()
            .map(|membership| {
                (
                    membership.participant_id.as_str(),
                    format!("{:?}", membership.role),
                )
            })
            .collect::<HashSet<_>>();
        if participant_ids != membership_ids
            || unique_memberships.len() != conversation.memberships.len()
        {
            return Err(integrity(
                "conversation participant and membership lists disagree",
            ));
        }
        if conversation
            .owner_participant_id
            .as_ref()
            .is_some_and(|owner| !conversation.participant_ids.contains(owner))
        {
            return Err(integrity(
                "conversation owner is absent from its participant list",
            ));
        }
        if let Some(owner) = &conversation.owner_participant_id {
            if !conversation.memberships.iter().any(|membership| {
                &membership.participant_id == owner
                    && membership.role == ConversationMembershipRole::Owner
            }) {
                return Err(integrity("conversation owner lacks an owner membership"));
            }
        }
    }
    for (identifier, participant) in participants {
        if identifier.is_empty()
            || participant.account_id != report.account_id
            || participant
                .conversation_ids
                .iter()
                .collect::<HashSet<_>>()
                .len()
                != participant.conversation_ids.len()
        {
            return Err(integrity(
                "participant entity is invalid or has duplicate conversations",
            ));
        }
        validate_base64(&participant.source_identifier_base64)?;
        validate_scoped_identifier(
            &report.account_id,
            Some(&participant.source_identifier_base64),
            Some(identifier),
            "participant",
        )?;
        validate_optional_base64(participant.alias_base64.as_deref())?;
        validate_optional_base64(participant.remark_base64.as_deref())?;
        validate_optional_base64(participant.nickname_base64.as_deref())?;
        validate_optional_base64(participant.display_name_base64.as_deref())?;
        for source in &participant.source_records {
            validate_entity_source_record(source, &covered_tables, &mut entity_source_rows)?;
        }
        for conversation in &participant.conversation_ids {
            let conversation = conversations
                .get(conversation)
                .ok_or_else(|| integrity("participant references an absent conversation"))?;
            if !conversation.participant_ids.contains(identifier) {
                return Err(integrity("participant relationship is not bidirectional"));
            }
        }
    }
    Ok(())
}

fn verify_message_entities(
    messages: &MessageAudit,
    conversations: &HashMap<String, CanonicalConversation>,
    participants: &HashMap<String, CanonicalParticipant>,
) -> Result<(), RestoreError> {
    if messages
        .conversation_ids
        .iter()
        .any(|identifier| !conversations.contains_key(identifier))
        || messages
            .participant_ids
            .iter()
            .any(|identifier| !participants.contains_key(identifier))
    {
        return Err(integrity(
            "message references an absent conversation or sender",
        ));
    }
    for (conversation_id, participant_id) in &messages.participant_memberships {
        let conversation = conversations
            .get(conversation_id)
            .ok_or_else(|| integrity("message conversation is absent"))?;
        if !conversation.participant_ids.contains(participant_id) {
            return Err(integrity(
                "message sender is absent from the conversation membership",
            ));
        }
    }
    for (identifier, source) in &messages.conversation_sources {
        if conversations
            .get(identifier)
            .is_none_or(|conversation| &conversation.source_identifier_base64 != source)
        {
            return Err(integrity(
                "message conversation source disagrees with its canonical entity",
            ));
        }
    }
    for (identifier, source) in &messages.participant_sources {
        if participants
            .get(identifier)
            .is_none_or(|participant| &participant.source_identifier_base64 != source)
        {
            return Err(integrity(
                "message sender source disagrees with its canonical participant",
            ));
        }
    }
    Ok(())
}

fn validate_entity_source_record<'a>(
    source: &crate::EntitySourceRecord,
    covered_tables: &HashMap<(&'a str, &'a str, &'a str), &'a crate::TableSchemaCoverage>,
    source_rows: &mut HashSet<(String, String, String, i64)>,
) -> Result<(), RestoreError> {
    if source.source_set_id.is_empty()
        || source.source_logical_path.is_empty()
        || source.source_table_id.is_empty()
        || source.source_table_name.is_empty()
        || !source_rows.insert((
            source.source_set_id.clone(),
            source.source_logical_path.clone(),
            source.source_table_id.clone(),
            source.source_row_id,
        ))
    {
        return Err(integrity(
            "entity source record has missing or duplicate provenance",
        ));
    }
    validate_raw_columns(&source.raw_columns)?;
    let table = covered_tables
        .get(&(
            source.source_set_id.as_str(),
            source.source_logical_path.as_str(),
            source.source_table_id.as_str(),
        ))
        .ok_or_else(|| integrity("entity source record is absent from table coverage"))?;
    let raw_columns = source
        .raw_columns
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let table_columns = table
        .columns
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if source.source_table_name != table.source_table_name || raw_columns != table_columns {
        return Err(integrity(
            "entity source record disagrees with its covered table",
        ));
    }
    Ok(())
}

fn verify_message_artifacts(
    messages: &MessageAudit,
    artifacts: &ArtifactAudit,
) -> Result<(), RestoreError> {
    if messages
        .artifact_ids
        .iter()
        .any(|identifier| !artifacts.identifiers.contains(identifier))
        || artifacts
            .identifiers
            .iter()
            .any(|identifier| !messages.artifact_ids.contains(identifier))
    {
        return Err(integrity(
            "message and artifact ledgers are not fully linked",
        ));
    }
    for (identifier, role) in &messages.artifact_references {
        // Artifact identity is content-based, so one artifact can serve
        // several roles across messages; every referenced role must be one
        // the artifact ledger records for that identity.
        if !artifacts
            .roles
            .get(identifier)
            .is_some_and(|roles| roles.contains(role))
        {
            return Err(integrity(
                "message artifact role disagrees with the artifact ledger",
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify_integrity_counts(
    report: &RestorationReport,
    coverage: &RestorationCoverage,
    messages: &MessageAudit,
    rejections: &RejectionAudit,
    artifacts: &ArtifactAudit,
    conversations: &HashMap<String, CanonicalConversation>,
    participants: &HashMap<String, CanonicalParticipant>,
) -> Result<(), RestoreError> {
    let integrity_report = &report.integrity;
    let unknown_payload_count = messages
        .unknown_payload_reason_counts
        .values()
        .copied()
        .sum::<u64>();
    let semantic_gap_count = messages
        .semantic_gap_reason_counts
        .values()
        .copied()
        .sum::<u64>();
    if messages.count != integrity_report.restored_row_count
        || rejections.count != integrity_report.rejected_row_count
        || messages.count + rejections.count != integrity_report.source_row_count
        || integrity_report.duplicate_canonical_id_count != 0
        || messages.logical_type_counts != integrity_report.logical_type_counts
        || messages.logical_sub_type_counts != integrity_report.logical_sub_type_counts
        || messages.unknown_payload_reason_counts != integrity_report.unknown_payload_reason_counts
        || messages.semantic_gap_reason_counts != integrity_report.semantic_gap_reason_counts
        || unknown_payload_count != integrity_report.unknown_payload_count
        || semantic_gap_count != integrity_report.semantic_gap_count
        || messages.direction_counts != integrity_report.direction_counts
        || messages.direction_conflict_count != integrity_report.direction_conflict_count
        || messages.ordering_basis_counts != integrity_report.ordering_basis_counts
        || messages.artifact_reference_count != integrity_report.artifact_reference_count
        || messages.relationship_reference_count != integrity_report.relationship_reference_count
        || artifacts.count != integrity_report.unique_artifact_count
        || artifacts.downloaded != integrity_report.downloaded_artifact_count
        || artifacts.materialized != integrity_report.materialized_artifact_count
        || artifacts.missing != integrity_report.missing_artifact_count
        || artifacts.ambiguous != integrity_report.ambiguous_artifact_count
        || artifacts.corrupt != integrity_report.corrupt_artifact_count
        || artifacts.unsafe_count != integrity_report.unsafe_artifact_count
        || artifacts.decoded != integrity_report.decoded_artifact_count
        || artifacts.decode_gaps != integrity_report.artifact_decode_gap_count
        || artifacts.account_root_unavailable
            != integrity_report.account_root_unavailable_artifact_count
        || conversations.len() as u64 != integrity_report.conversation_count
        || participants.len() as u64 != integrity_report.participant_count
        || coverage.logical_type_counts != messages.logical_type_counts
        || coverage.logical_sub_type_counts != messages.logical_sub_type_counts
        || coverage.unknown_payload_reason_counts != messages.unknown_payload_reason_counts
        || coverage.semantic_gap_reason_counts != messages.semantic_gap_reason_counts
    {
        return Err(integrity(
            "archive records do not reproduce reported integrity counts",
        ));
    }

    let unresolved = messages.absent_relationship_target_count
        + messages.missing_relationship_identifier_count
        + messages.ambiguous_relationship_count
        + messages.pending_relationship_count;
    if messages.resolved_relationship_count != integrity_report.resolved_relationship_count
        || unresolved != integrity_report.unresolved_relationship_count
        || messages.absent_relationship_target_count
            != integrity_report.absent_relationship_target_count
        || messages.missing_relationship_identifier_count
            != integrity_report.missing_relationship_identifier_count
        || messages.ambiguous_relationship_count != integrity_report.ambiguous_relationship_count
        || messages.resolved_relationship_count + unresolved
            != integrity_report.relationship_reference_count
    {
        return Err(integrity(
            "relationship resolution-state counts do not reproduce reported integrity",
        ));
    }
    let missing_profiles = participants
        .values()
        .filter(|value| value.local_profile_state == LocalProfileState::MissingLocalRecord)
        .count() as u64;
    let unresolved_conversations = conversations
        .values()
        .filter(|value| value.kind == ConversationKind::Unresolved)
        .count() as u64;
    let entity_gaps = conversations
        .values()
        .filter(|value| value.entity_decode_state != EntityDecodeState::Complete)
        .count() as u64;
    let entity_source_rows = conversations
        .values()
        .map(|value| value.source_records.len() as u64)
        .sum::<u64>()
        + participants
            .values()
            .map(|value| value.source_records.len() as u64)
            .sum::<u64>();
    let unique_group_members = conversations
        .values()
        .flat_map(|value| &value.memberships)
        .filter(|membership| membership.role == ConversationMembershipRole::Member)
        .count() as u64;
    if missing_profiles != integrity_report.missing_local_profile_count
        || unresolved_conversations != integrity_report.unresolved_conversation_count
        || entity_source_rows != integrity_report.entity_source_row_count
        || unique_group_members != integrity_report.group_member_count
        || entity_gaps != integrity_report.entity_decode_gap_count
    {
        return Err(integrity(
            "entity records do not reproduce reported coverage gaps",
        ));
    }
    Ok(())
}

fn audit_cached_surfaces(
    root: &Path,
    report: &RestorationReport,
    restoration_coverage: &RestorationCoverage,
    progress: &mut ArchiveAuditProgress<'_>,
) -> Result<(u64, u64), RestoreError> {
    let coverage: CachedSurfaceCoverage = read_json(&root.join("cached-surfaces.json"))?;
    if !matches!(coverage.format_version, 1 | 2) {
        return Err(integrity("cached-surface coverage format is unsupported"));
    }
    validate_cached_coverage_schema(&coverage)?;
    let moments = read_unique_ndjson::<CanonicalCachedMoment, _, _>(
        &root.join("cached-moments.ndjson"),
        |value| value.canonical_id.clone(),
        "cached Moment",
        progress,
    )?;
    let interactions = read_unique_ndjson::<CanonicalCachedMomentInteraction, _, _>(
        &root.join("cached-moment-interactions.ndjson"),
        |value| value.canonical_id.clone(),
        "cached Moment interaction",
        progress,
    )?;
    let mut moment_source_rows = HashSet::new();
    let mut interaction_source_rows = HashSet::new();
    let mut moment_table_counts = HashMap::<(String, String, String), u64>::new();
    let mut interaction_table_counts = HashMap::<(String, String, String), u64>::new();
    let mut observed_semantic_gaps = 0_u64;
    for moment in moments.values() {
        if moment.canonical_id.is_empty()
            || moment.account_id != report.account_id
            || moment.source_set_id.is_empty()
            || moment.source_logical_path.is_empty()
            || moment.source_table_id.is_empty()
            || moment.source_table_name.is_empty()
            || moment.observed_at.is_empty()
            || !moment_source_rows.insert((
                moment.source_set_id.clone(),
                moment.source_table_id.clone(),
                moment.source_row_id,
            ))
        {
            return Err(integrity(
                "cached Moment identity, provenance, or observation time is invalid",
            ));
        }
        let identity = format!(
            "{}:{}:{}",
            moment.source_set_id, moment.source_table_id, moment.source_row_id
        );
        if moment.canonical_id != hex::encode(Sha256::digest(identity.as_bytes())) {
            return Err(integrity(
                "cached Moment canonical identity is not source-deterministic",
            ));
        }
        validate_optional_base64(moment.author_source_identifier_base64.as_deref())?;
        validate_optional_base64(moment.content_description_base64.as_deref())?;
        validate_optional_base64(moment.title_base64.as_deref())?;
        validate_optional_base64(moment.description_base64.as_deref())?;
        validate_optional_base64(moment.content_url_base64.as_deref())?;
        validate_optional_base64(moment.raw_content_base64.as_deref())?;
        validate_optional_base64(moment.raw_pack_info_base64.as_deref())?;
        validate_raw_value(&moment.timeline_id)?;
        validate_raw_columns(&moment.raw_columns)?;
        validate_scoped_identifier(
            &report.account_id,
            moment.author_source_identifier_base64.as_deref(),
            moment.author_id.as_deref(),
            "cached Moment author",
        )?;
        if moment.semantic_decode_state != SemanticDecodeState::Complete {
            if moment
                .semantic_gap_reason
                .as_deref()
                .is_none_or(str::is_empty)
            {
                return Err(integrity(
                    "cached Moment semantic gap lacks an explicit reason",
                ));
            }
            observed_semantic_gaps += 1;
        } else if moment.semantic_gap_reason.is_some() {
            return Err(integrity(
                "complete cached Moment unexpectedly records a semantic gap",
            ));
        }
        *moment_table_counts
            .entry((
                moment.source_set_id.clone(),
                moment.source_logical_path.clone(),
                moment.source_table_id.clone(),
            ))
            .or_default() += 1;
    }
    for interaction in interactions.values() {
        if interaction.canonical_id.is_empty()
            || interaction.account_id != report.account_id
            || interaction.source_set_id.is_empty()
            || interaction.source_logical_path.is_empty()
            || interaction.source_table_id.is_empty()
            || interaction.source_table_name.is_empty()
            || interaction.observed_at.is_empty()
            || !interaction_source_rows.insert((
                interaction.source_set_id.clone(),
                interaction.source_table_id.clone(),
                interaction.source_row_id,
            ))
        {
            return Err(integrity(
                "cached interaction identity, provenance, or observation time is invalid",
            ));
        }
        let identity = format!(
            "{}:{}:{}",
            interaction.source_set_id, interaction.source_table_id, interaction.source_row_id
        );
        if interaction.canonical_id != hex::encode(Sha256::digest(identity.as_bytes())) {
            return Err(integrity(
                "cached interaction canonical identity is not source-deterministic",
            ));
        }
        validate_optional_base64(interaction.from_source_identifier_base64.as_deref())?;
        validate_optional_base64(interaction.from_nickname_base64.as_deref())?;
        validate_optional_base64(interaction.to_source_identifier_base64.as_deref())?;
        validate_optional_base64(interaction.to_nickname_base64.as_deref())?;
        validate_optional_base64(interaction.content_base64.as_deref())?;
        validate_raw_value(&interaction.feed_id)?;
        validate_raw_columns(&interaction.raw_columns)?;
        validate_scoped_identifier(
            &report.account_id,
            interaction.from_source_identifier_base64.as_deref(),
            interaction.from_participant_id.as_deref(),
            "cached interaction source participant",
        )?;
        validate_scoped_identifier(
            &report.account_id,
            interaction.to_source_identifier_base64.as_deref(),
            interaction.to_participant_id.as_deref(),
            "cached interaction target participant",
        )?;
        let expected_kind = match interaction.raw_type {
            Some(1) => crate::CachedMomentInteractionKind::Comment,
            Some(2) => crate::CachedMomentInteractionKind::Like,
            _ => crate::CachedMomentInteractionKind::Unknown,
        };
        if interaction.kind != expected_kind {
            return Err(integrity(
                "cached interaction kind disagrees with its raw type",
            ));
        }
        *interaction_table_counts
            .entry((
                interaction.source_set_id.clone(),
                interaction.source_logical_path.clone(),
                interaction.source_table_id.clone(),
            ))
            .or_default() += 1;
    }
    let mut table_ids = HashSet::new();
    let mut moment_rows = 0_u64;
    let mut interaction_rows = 0_u64;
    for table in &coverage.tables {
        let identity = (
            table.source_set_id.clone(),
            table.source_logical_path.clone(),
            table.source_table_id.clone(),
        );
        if !table_ids.insert(identity.clone())
            || table
                .schema_fingerprint
                .as_deref()
                .is_some_and(|value| !is_lower_hex(value, 64))
            || (table.availability == crate::TableCoverageAvailability::Complete
                && table
                    .schema_fingerprint
                    .as_deref()
                    .is_none_or(|value| !is_lower_hex(value, 64)))
        {
            return Err(integrity(
                "cached coverage has duplicate or incomplete table evidence",
            ));
        }
        match table.role {
            CachedSurfaceTableRole::MomentTimeline => {
                moment_rows += table.restored_row_count;
                if moment_table_counts
                    .get(&identity)
                    .copied()
                    .unwrap_or_default()
                    != table.restored_row_count
                {
                    return Err(integrity("cached Moment table row equation failed"));
                }
            }
            CachedSurfaceTableRole::MomentInteraction => {
                interaction_rows += table.restored_row_count;
                if interaction_table_counts
                    .get(&identity)
                    .copied()
                    .unwrap_or_default()
                    != table.restored_row_count
                {
                    return Err(integrity("cached interaction table row equation failed"));
                }
            }
            CachedSurfaceTableRole::UnsupportedCandidate | CachedSurfaceTableRole::Other => {
                if table.restored_row_count != 0
                    || moment_table_counts.contains_key(&identity)
                    || interaction_table_counts.contains_key(&identity)
                {
                    return Err(integrity(
                        "unsupported cached table unexpectedly has canonical records",
                    ));
                }
            }
        }
    }
    if moment_table_counts
        .keys()
        .chain(interaction_table_counts.keys())
        .any(|identity| !table_ids.contains(identity))
    {
        return Err(integrity(
            "cached record belongs to a table absent from cached coverage",
        ));
    }
    let restoration_cached_tables = restoration_coverage
        .all_tables
        .iter()
        .filter(|table| {
            let logical = table.source_logical_path.to_ascii_lowercase();
            logical == "sns/sns.db" || logical.ends_with("/sns.db") || logical == "sns.db"
        })
        .map(|table| {
            (
                (
                    table.source_set_id.clone(),
                    table.source_logical_path.clone(),
                    table.source_table_id.clone(),
                ),
                table,
            )
        })
        .collect::<HashMap<_, _>>();
    let unavailable_source_sets = report
        .database_coverage
        .as_ref()
        .map(|database| {
            database
                .unavailable_source_set_ids
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let cached_inventory_degraded = coverage.limitation_codes.iter().any(|code| {
        matches!(
            code.as_str(),
            "cachedSurfaceDatabaseUnavailable" | "cachedSurfaceTableUnavailable"
        )
    });
    if table_ids.iter().any(|identity| {
        !restoration_cached_tables.contains_key(identity)
            && !unavailable_source_sets.contains(identity.0.as_str())
    }) || (!cached_inventory_degraded
        && restoration_cached_tables
            .keys()
            .any(|identity| !table_ids.contains(identity)))
    {
        return Err(integrity(
            "cached table ledger does not match complete restoration coverage",
        ));
    }
    for table in &coverage.tables {
        let identity = (
            table.source_set_id.clone(),
            table.source_logical_path.clone(),
            table.source_table_id.clone(),
        );
        let Some(source) = restoration_cached_tables.get(&identity) else {
            if table.availability == crate::TableCoverageAvailability::Unavailable
                && unavailable_source_sets.contains(identity.0.as_str())
            {
                continue;
            }
            return Err(integrity(
                "cached table is absent from restoration coverage",
            ));
        };
        let degraded_metadata = table.availability != crate::TableCoverageAvailability::Complete
            && table.limitation_code.is_some();
        if source.source_table_name != table.source_table_name
            || (!degraded_metadata && source.columns != table.columns)
            || (!degraded_metadata && source.schema_fingerprint != table.schema_fingerprint)
        {
            return Err(integrity(
                "cached table provenance disagrees with restoration coverage",
            ));
        }
    }
    let profile = schema_profile_fingerprint(coverage.tables.iter().map(|table| {
        (
            table.source_logical_path.as_str(),
            table.source_table_name.as_str(),
            table.schema_fingerprint.as_deref(),
        )
    }));
    if coverage.schema_profile_fingerprint != profile
        || moment_rows != moments.len() as u64
        || interaction_rows != interactions.len() as u64
        || coverage.moment_count != moments.len() as u64
        || coverage.interaction_count != interactions.len() as u64
        || observed_semantic_gaps != coverage.semantic_gap_count
        || coverage.semantic_gap_count != report.integrity.cached_surface_semantic_gap_count
        || coverage.omitted_row_count != report.integrity.cached_surface_omitted_row_count
    {
        return Err(integrity(
            "cached-surface coverage does not match its record ledgers",
        ));
    }
    Ok((moments.len() as u64, interactions.len() as u64))
}

fn verify_completion(report: &RestorationReport) -> Result<RestorationCompletion, RestoreError> {
    let expected = RestorationCompletion::evaluate_report(report);
    let component_fields_match = expected.row_equation_holds
        == report.completion.row_equation_holds
        && expected.zero_rejected_rows == report.completion.zero_rejected_rows
        && expected.canonical_identities_unique == report.completion.canonical_identities_unique
        && expected.semantic_message_coverage_complete
            == report.completion.semantic_message_coverage_complete
        && expected.directions_complete == report.completion.directions_complete
        && expected.entity_coverage_complete == report.completion.entity_coverage_complete
        && expected.relationship_coverage_complete
            == report.completion.relationship_coverage_complete
        && expected.artifact_verification_complete
            == report.completion.artifact_verification_complete
        && expected.artifact_decoding_complete == report.completion.artifact_decoding_complete;
    if !component_fields_match
        || (report.completion.full_restoration_achieved && !expected.full_restoration_achieved)
    {
        return Err(integrity(
            "completion verdict is inconsistent with audited evidence",
        ));
    }
    Ok(expected)
}

fn audited_completion_evidence(
    completion: &RestorationCompletion,
    archive_scope: RestorationArchiveScope,
    media_phase: RestorationMediaPhase,
    client_build_production_compatible: bool,
    message_count: u64,
    artifact_reference_count: u64,
    verified_local_file_count: u64,
) -> AuditedRestorationCompletionEvidence {
    let row_accounting_complete = completion.row_equation_holds
        && completion.zero_rejected_rows
        && completion.canonical_identities_unique;
    AuditedRestorationCompletionEvidence {
        format_version: 1,
        row_accounting_complete,
        observed_message_type_coverage_complete: completion.semantic_message_coverage_complete,
        direction_resolution_complete: completion.directions_complete,
        entity_reconstruction_complete: completion.entity_coverage_complete,
        relationship_resolution_complete: completion.relationship_coverage_complete,
        artifact_verification_complete: completion.artifact_verification_complete,
        artifact_decoding_complete: completion.artifact_decoding_complete,
        source_scope_authoritative: archive_scope == RestorationArchiveScope::Authoritative,
        media_phase_resolved: media_phase == RestorationMediaPhase::Resolved,
        client_build_production_compatible,
        technical_restoration_complete: completion.full_restoration_achieved,
        non_empty_message_corpus_observed: message_count > 0,
        media_reference_corpus_observed: artifact_reference_count > 0,
        verified_local_media_observed: verified_local_file_count > 0,
        external_authorization_attestation_required: true,
        disposable_scenario_attestation_required: true,
        observed_corpus_scope_only: true,
    }
}

fn required_artifact_path(value: Option<&str>, message: &str) -> Result<PathBuf, RestoreError> {
    value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| integrity(message))
}

struct AuditLedgerPlan {
    path: PathBuf,
    byte_count: u64,
}

struct ArchiveAuditProgress<'a> {
    observer: &'a dyn ProgressObserver,
    ledgers: Vec<AuditLedgerPlan>,
    ledger_index: usize,
    total_bytes: u64,
    completed_work: u64,
    total_work: u64,
    started_at: Instant,
}

impl<'a> ArchiveAuditProgress<'a> {
    fn new(
        root: &Path,
        observer: &'a dyn ProgressObserver,
    ) -> Result<ArchiveAuditProgress<'a>, RestoreError> {
        let mut ledgers = Vec::new();
        let mut total_bytes = 0_u64;
        let mut total_work = 0_u64;
        for name in [
            "conversations.ndjson",
            "participants.ndjson",
            "messages.ndjson",
            "rejections.ndjson",
            "artifacts.ndjson",
            "cached-moments.ndjson",
            "cached-moment-interactions.ndjson",
        ] {
            let path = root.join(name);
            ensure_private_regular_file(&path)?;
            let byte_count = path.metadata()?.len();
            let work_count = byte_count.max(1);
            total_bytes = total_bytes.saturating_add(byte_count);
            total_work = total_work.saturating_add(work_count);
            ledgers.push(AuditLedgerPlan { path, byte_count });
        }
        let mut event = ProgressEvent::new(
            ProgressPhase::ArchiveAudit,
            ProgressState::Started,
            "auditArchive",
            ProgressUnit::Bytes,
            0,
            total_bytes,
            0,
            total_work,
        );
        event.file_count = Some(ledgers.len());
        observer.observe(event);
        Ok(Self {
            observer,
            ledgers,
            ledger_index: 0,
            total_bytes,
            completed_work: 0,
            total_work,
            started_at: Instant::now(),
        })
    }

    fn begin_ledger(&self, path: &Path) -> Result<(usize, u64, Instant), RestoreError> {
        let plan = self
            .ledgers
            .get(self.ledger_index)
            .ok_or_else(|| integrity("archive audit encountered an unplanned extra ledger"))?;
        if plan.path != path {
            return Err(integrity(
                "archive audit ledger order differs from its progress plan",
            ));
        }
        let started = Instant::now();
        self.observe_ledger(
            ProgressState::Started,
            path,
            self.ledger_index,
            plan.byte_count,
            0,
            0,
            None,
        );
        Ok((self.ledger_index, plan.byte_count, started))
    }

    fn advance_ledger(
        &self,
        path: &Path,
        index: usize,
        byte_count: u64,
        completed: u64,
        records: u64,
    ) {
        self.observe_ledger(
            ProgressState::Advanced,
            path,
            index,
            byte_count,
            completed,
            records,
            None,
        );
    }

    fn complete_ledger(
        &mut self,
        path: &Path,
        index: usize,
        byte_count: u64,
        records: u64,
        started: Instant,
    ) -> Result<(), RestoreError> {
        if index != self.ledger_index {
            return Err(integrity("archive audit ledger progress is out of order"));
        }
        self.observe_ledger(
            ProgressState::Completed,
            path,
            index,
            byte_count,
            byte_count,
            records,
            Some(elapsed_milliseconds(started)),
        );
        self.completed_work = self.completed_work.saturating_add(byte_count.max(1));
        self.ledger_index = self.ledger_index.saturating_add(1);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn observe_ledger(
        &self,
        state: ProgressState,
        path: &Path,
        index: usize,
        byte_count: u64,
        completed: u64,
        records: u64,
        elapsed_milliseconds: Option<u64>,
    ) {
        let mut event = ProgressEvent::new(
            ProgressPhase::ArchiveAudit,
            state,
            "auditArchiveLedger",
            ProgressUnit::Bytes,
            completed.min(byte_count),
            byte_count,
            self.completed_work.saturating_add(
                if byte_count == 0 && state == ProgressState::Completed {
                    1
                } else {
                    completed.min(byte_count)
                },
            ),
            self.total_work,
        );
        event.file_index = Some(index.saturating_add(1));
        event.file_count = Some(self.ledgers.len());
        event.file_byte_count = Some(byte_count);
        event.logical_path = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string);
        event.source_record_count = Some(records);
        event.elapsed_milliseconds = elapsed_milliseconds;
        self.observer.observe(event);
    }

    fn finish(&self, report: &ArchiveAuditReport) {
        let mut event = ProgressEvent::new(
            ProgressPhase::ArchiveAudit,
            ProgressState::Completed,
            "auditArchive",
            ProgressUnit::Bytes,
            self.total_bytes,
            self.total_bytes,
            self.total_work,
            self.total_work,
        );
        event.file_count = Some(self.ledgers.len());
        event.restored_record_count = Some(report.restored_record_count());
        event.rejected_record_count = Some(report.rejection_count);
        event.source_record_count = Some(
            report
                .message_count
                .saturating_add(report.rejection_count)
                .saturating_add(report.artifact_count)
                .saturating_add(report.conversation_count)
                .saturating_add(report.participant_count)
                .saturating_add(report.cached_moment_count)
                .saturating_add(report.cached_moment_interaction_count),
        );
        event.elapsed_milliseconds = Some(elapsed_milliseconds(self.started_at));
        self.observer.observe(event);
    }
}

fn read_unique_ndjson<T, K, F>(
    path: &Path,
    key: F,
    kind: &str,
    progress: &mut ArchiveAuditProgress<'_>,
) -> Result<HashMap<K, T>, RestoreError>
where
    T: DeserializeOwned,
    K: Eq + std::hash::Hash,
    F: Fn(&T) -> K,
{
    let mut values = HashMap::new();
    read_ndjson(path, progress, |value: T| {
        if values.insert(key(&value), value).is_some() {
            return Err(integrity(format!(
                "{kind} ledger contains a duplicate identity"
            )));
        }
        Ok(())
    })?;
    Ok(values)
}

fn read_ndjson<T, F>(
    path: &Path,
    progress: &mut ArchiveAuditProgress<'_>,
    mut consume: F,
) -> Result<(), RestoreError>
where
    T: DeserializeOwned,
    F: FnMut(T) -> Result<(), RestoreError>,
{
    let file = open_private_readonly(path)?;
    let before = file.metadata()?;
    let (ledger_index, planned_bytes, ledger_started) = progress.begin_ledger(path)?;
    if planned_bytes != before.len() {
        return Err(integrity(
            "archive ledger size changed after audit progress planning",
        ));
    }
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut completed_bytes = 0_u64;
    let mut record_count = 0_u64;
    let report_increment = (planned_bytes / 100).max(4 * 1024 * 1024).max(1);
    let mut next_report = report_increment;
    let mut last_report = Instant::now();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            return Err(integrity("NDJSON ledger contains an empty record"));
        }
        consume(serde_json::from_slice(&line)?)?;
        completed_bytes = completed_bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        record_count = record_count.saturating_add(1);
        if completed_bytes >= next_report || last_report.elapsed() >= Duration::from_secs(2) {
            progress.advance_ledger(
                path,
                ledger_index,
                planned_bytes,
                completed_bytes,
                record_count,
            );
            next_report = completed_bytes.saturating_add(report_increment);
            last_report = Instant::now();
        }
    }
    let after = reader.get_ref().metadata()?;
    if !same_file_version(&before, &after) || completed_bytes != before.len() {
        return Err(integrity("archive ledger changed while it was audited"));
    }
    progress.complete_ledger(
        path,
        ledger_index,
        planned_bytes,
        record_count,
        ledger_started,
    )?;
    Ok(())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, RestoreError> {
    let mut file = open_private_readonly(path)?;
    let before = file.metadata()?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    let after = file.metadata()?;
    if !same_file_version(&before, &after) || data.len() as u64 != before.len() {
        return Err(integrity("archive JSON changed while it was audited"));
    }
    Ok(serde_json::from_slice(&data)?)
}

fn open_private_readonly(path: &Path) -> Result<File, RestoreError> {
    ensure_private_regular_file(path)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 || metadata.nlink() != 1 {
        return Err(integrity(
            "private archive file changed identity or permissions before audit",
        ));
    }
    Ok(file)
}

fn validate_raw_columns(values: &BTreeMap<String, RawSQLiteValue>) -> Result<(), RestoreError> {
    for value in values.values() {
        validate_raw_value(value)?;
    }
    Ok(())
}

fn validate_raw_value(value: &RawSQLiteValue) -> Result<(), RestoreError> {
    match value {
        RawSQLiteValue::TextBase64(value) | RawSQLiteValue::BlobBase64(value) => {
            validate_base64(value)
        }
        RawSQLiteValue::Null | RawSQLiteValue::Integer(_) | RawSQLiteValue::Real(_) => Ok(()),
    }
}

fn validate_scoped_identifier(
    account_id: &str,
    source_base64: Option<&str>,
    identifier: Option<&str>,
    kind: &str,
) -> Result<(), RestoreError> {
    match (source_base64, identifier) {
        (None, None) => Ok(()),
        (Some(source), Some(identifier)) => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(source)
                .map_err(|_| {
                    integrity("archive contains a malformed source-preserving base64 field")
                })?;
            if bytes.is_empty() {
                return Err(integrity(format!("{kind} source identifier is empty")));
            }
            let mut hasher = Sha256::new();
            hasher.update(account_id.as_bytes());
            hasher.update([0]);
            hasher.update(bytes);
            if hex::encode(hasher.finalize()) == identifier {
                Ok(())
            } else {
                Err(integrity(format!(
                    "{kind} identity is not account-scoped and source-deterministic"
                )))
            }
        }
        _ => Err(integrity(format!(
            "{kind} identity and source evidence are incomplete"
        ))),
    }
}

fn validate_optional_base64(value: Option<&str>) -> Result<(), RestoreError> {
    if let Some(value) = value {
        validate_base64(value)?;
    }
    Ok(())
}

fn validate_base64(value: &str) -> Result<(), RestoreError> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map(|_| ())
        .map_err(|_| integrity("archive contains a malformed source-preserving base64 field"))
}

fn is_lower_hex(value: &str, expected_length: usize) -> bool {
    value.len() == expected_length
        && value
            .bytes()
            .all(|value| value.is_ascii_digit() || matches!(value, b'a'..=b'f'))
}

fn same_file_version(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
}

fn elapsed_milliseconds(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn integrity(message: impl Into<String>) -> RestoreError {
    RestoreError::Integrity(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_evidence_keeps_machine_checks_and_external_attestations_distinct() {
        let completion = RestorationCompletion {
            row_equation_holds: true,
            zero_rejected_rows: true,
            canonical_identities_unique: true,
            semantic_message_coverage_complete: true,
            directions_complete: true,
            entity_coverage_complete: true,
            relationship_coverage_complete: true,
            artifact_verification_complete: true,
            artifact_decoding_complete: true,
            full_restoration_achieved: true,
        };
        let evidence = audited_completion_evidence(
            &completion,
            RestorationArchiveScope::Authoritative,
            RestorationMediaPhase::Resolved,
            true,
            10,
            3,
            2,
        );

        assert!(evidence.row_accounting_complete);
        assert!(evidence.observed_message_type_coverage_complete);
        assert!(evidence.technical_restoration_complete);
        assert!(evidence.non_empty_message_corpus_observed);
        assert!(evidence.media_reference_corpus_observed);
        assert!(evidence.verified_local_media_observed);
        assert!(evidence.external_authorization_attestation_required);
        assert!(evidence.disposable_scenario_attestation_required);
        assert!(evidence.observed_corpus_scope_only);

        let fragment = audited_completion_evidence(
            &RestorationCompletion {
                full_restoration_achieved: false,
                ..completion
            },
            RestorationArchiveScope::IncrementalFragment,
            RestorationMediaPhase::Deferred,
            false,
            0,
            0,
            0,
        );
        assert!(!fragment.source_scope_authoritative);
        assert!(!fragment.media_phase_resolved);
        assert!(!fragment.client_build_production_compatible);
        assert!(!fragment.technical_restoration_complete);
        assert!(!fragment.non_empty_message_corpus_observed);
        assert!(!fragment.media_reference_corpus_observed);
        assert!(!fragment.verified_local_media_observed);
    }

    #[test]
    fn message_artifact_roles_accept_every_recorded_role() {
        // Artifact identity is content-based, so one artifact can serve
        // several roles across messages; every recorded role must verify.
        let mut messages = MessageAudit {
            count: 1,
            ..MessageAudit::default()
        };
        messages.artifact_ids.insert("artifact-a".to_string());
        messages.artifact_references = vec![
            ("artifact-a".to_string(), crate::ArtifactRole::Original),
            (
                "artifact-a".to_string(),
                crate::ArtifactRole::StickerPayload,
            ),
            ("artifact-a".to_string(), crate::ArtifactRole::FilePayload),
        ];
        let mut artifacts = ArtifactAudit::default();
        artifacts.identifiers.insert("artifact-a".to_string());
        artifacts.roles.insert(
            "artifact-a".to_string(),
            BTreeSet::from([
                crate::ArtifactRole::Original,
                crate::ArtifactRole::StickerPayload,
                crate::ArtifactRole::FilePayload,
            ]),
        );

        assert!(verify_message_artifacts(&messages, &artifacts).is_ok());
    }

    #[test]
    fn message_artifact_roles_reject_unrecorded_role() {
        let mut messages = MessageAudit {
            count: 1,
            ..MessageAudit::default()
        };
        messages.artifact_ids.insert("artifact-a".to_string());
        messages.artifact_references =
            vec![("artifact-a".to_string(), crate::ArtifactRole::VideoPoster)];
        let mut artifacts = ArtifactAudit::default();
        artifacts.identifiers.insert("artifact-a".to_string());
        artifacts.roles.insert(
            "artifact-a".to_string(),
            BTreeSet::from([crate::ArtifactRole::Original]),
        );

        assert!(verify_message_artifacts(&messages, &artifacts).is_err());
    }

    #[test]
    fn message_artifact_roles_reject_unlinked_identifier() {
        let mut messages = MessageAudit {
            count: 1,
            ..MessageAudit::default()
        };
        messages.artifact_ids.insert("artifact-a".to_string());
        messages.artifact_references =
            vec![("artifact-a".to_string(), crate::ArtifactRole::Original)];
        let mut artifacts = ArtifactAudit::default();
        artifacts.identifiers.insert("artifact-a".to_string());
        artifacts.roles.insert(
            "artifact-a".to_string(),
            BTreeSet::from([crate::ArtifactRole::Original]),
        );
        messages.artifact_ids.clear();

        assert!(verify_message_artifacts(&messages, &artifacts).is_err());
    }

    #[test]
    fn artifact_role_sets_fall_back_to_the_single_role_field() {
        // Archives written before per-artifact role sets must still audit.
        let legacy = r#"{
            "artifactId": "artifact-a",
            "kind": "image",
            "role": "stickerPayload",
            "availability": "notDownloaded",
            "decodeState": "notRequired"
        }"#;
        let artifact: crate::model::CanonicalArtifact = serde_json::from_str(legacy).unwrap();
        assert!(artifact.roles.is_empty());
        let mut roles = artifact.roles;
        roles.insert(artifact.role);
        assert!(roles.contains(&crate::ArtifactRole::StickerPayload));

        let current = r#"{
            "artifactId": "artifact-a",
            "kind": "image",
            "role": "stickerPayload",
            "roles": ["stickerPayload", "filePayload"],
            "availability": "notDownloaded",
            "decodeState": "notRequired"
        }"#;
        let artifact: crate::model::CanonicalArtifact = serde_json::from_str(current).unwrap();
        assert_eq!(artifact.roles.len(), 2);
    }
}
