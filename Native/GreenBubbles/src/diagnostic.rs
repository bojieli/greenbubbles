use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine;
use roxmltree::{Document, ParsingOptions};
use serde::Serialize;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::archive::ensure_private_directory;
use crate::{
    CanonicalMessage, MessageRelationship, MessageRelationshipKind, NoProgress, ProgressEvent,
    ProgressObserver, ProgressPhase, ProgressState, ProgressUnit, RawSQLiteValue,
    RestorationCoverage, RestoreError, SemanticDecodeState, TableCoverageRole, TypedPayload,
};

const MAX_GAP_SHAPES_PER_PROFILE: usize = 16;
const MAX_TAG_TOKENS_PER_SHAPE: usize = 128;
const MAX_DIAGNOSTIC_RECORD_BYTES: usize = 64 * 1024 * 1024;
const MAX_DIAGNOSTIC_COVERAGE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_COLUMN_SET_VARIANTS: usize = 32;

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticSchemaProfileReport {
    pub format_version: u32,
    pub privacy_safe: bool,
    pub table_count: u64,
    pub source_row_count: u64,
    pub role_counts: BTreeMap<String, u64>,
    pub classification_reason_counts: BTreeMap<String, u64>,
    pub other_table_count: u64,
    pub other_source_row_count: u64,
    pub other_families: Vec<DiagnosticSchemaFamilyProfile>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticSchemaFamilyProfile {
    pub logical_database_family: String,
    pub table_family: String,
    pub table_count: u64,
    pub non_empty_table_count: u64,
    pub source_row_count: u64,
    pub schema_variant_count: usize,
    pub column_set_variants_truncated: bool,
    pub column_set_variants: Vec<Vec<String>>,
}

#[derive(Default)]
struct SchemaFamilyAccumulator {
    table_count: u64,
    non_empty_table_count: u64,
    source_row_count: u64,
    schema_fingerprints: BTreeSet<String>,
    column_set_variants: BTreeSet<Vec<String>>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticPayloadProfileReport {
    pub format_version: u32,
    pub privacy_safe: bool,
    pub message_count: u64,
    pub relationship_reference_count: u64,
    pub relationship_identifier_present_count: u64,
    pub relationship_identifier_recoverable_from_decoded_xml_count: u64,
    pub relationship_identifier_missing_from_decoded_xml_count: u64,
    pub relationship_decoded_xml_unavailable_count: u64,
    pub adapter_type_profiles: BTreeMap<String, DiagnosticAdapterTypeProfile>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticAdapterTypeProfile {
    pub source_adapter: String,
    pub logical_type: Option<u32>,
    pub logical_sub_type: Option<u32>,
    pub record_count: u64,
    pub semantic_decode_state_counts: BTreeMap<String, u64>,
    pub semantic_gap_reason_counts: BTreeMap<String, u64>,
    pub canonical_content: DiagnosticValueShapeProfile,
    pub raw_columns: BTreeMap<String, DiagnosticValueShapeProfile>,
    pub semantic_gap_xml_value_count: u64,
    pub semantic_gap_xml_shapes: Vec<DiagnosticXmlGapShape>,
}

/// A bounded description of one structurally distinct XML payload. It omits
/// source row identities, content bytes, text nodes, and attribute values.
/// The digest lets operators correlate repeated shapes inside a private
/// report without exposing the source value.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticXmlGapShape {
    pub byte_count: u64,
    pub sha256: String,
    pub direct_parse_verdict: String,
    pub first_tag_name: Option<String>,
    pub last_tag_name: Option<String>,
    pub tag_token_count: u64,
    pub tag_tokens_truncated: bool,
    pub msg_open_offset: Option<u64>,
    pub msg_close_offset: Option<u64>,
    pub appmsg_open_offset: Option<u64>,
    pub appinfo_open_offset: Option<u64>,
    pub offset_shape: String,
    pub tag_tokens: Vec<DiagnosticXmlTagToken>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticXmlTagToken {
    pub name: String,
    pub kind: DiagnosticXmlTagKind,
    pub byte_offset: u64,
    pub depth: u64,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticXmlTagKind {
    Open,
    Close,
    SelfClosing,
    ProcessingInstruction,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticValueShapeProfile {
    pub null_count: u64,
    pub integer_count: u64,
    pub real_count: u64,
    pub text_count: u64,
    pub blob_count: u64,
    pub present_byte_value_count: u64,
    pub empty_byte_value_count: u64,
    pub total_byte_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum_byte_count: Option<u64>,
    pub utf8_xml_count: u64,
    pub utf8_json_count: u64,
    pub other_utf8_count: u64,
    pub binary_count: u64,
}

pub fn profile_archive_schema(
    archive: &Path,
) -> Result<DiagnosticSchemaProfileReport, RestoreError> {
    profile_archive_schema_with_progress(archive, &NoProgress)
}

pub fn profile_archive_schema_with_progress(
    archive: &Path,
    progress: &dyn ProgressObserver,
) -> Result<DiagnosticSchemaProfileReport, RestoreError> {
    ensure_private_directory(archive)?;
    let coverage_path = archive.join("coverage.json");
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&coverage_path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.len() == 0
        || metadata.len() > MAX_DIAGNOSTIC_COVERAGE_BYTES
    {
        return Err(RestoreError::Integrity(
            "diagnostic coverage input must be a bounded owner-only regular file".to_string(),
        ));
    }
    let total_bytes = metadata.len();
    progress.observe(ProgressEvent::new(
        ProgressPhase::ArchiveAudit,
        ProgressState::Started,
        "profileArchiveSchema",
        ProgressUnit::Bytes,
        0,
        total_bytes,
        0,
        total_bytes,
    ));
    let mut bytes = Vec::with_capacity(usize::try_from(total_bytes).unwrap_or_default());
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut completed = 0_u64;
    let started = Instant::now();
    let mut last_report = Instant::now();
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        completed = completed.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if last_report.elapsed() >= Duration::from_secs(1) || completed == total_bytes {
            progress.observe(ProgressEvent::new(
                ProgressPhase::ArchiveAudit,
                ProgressState::Advanced,
                "profileArchiveSchema",
                ProgressUnit::Bytes,
                completed.min(total_bytes),
                total_bytes,
                completed.min(total_bytes),
                total_bytes,
            ));
            last_report = Instant::now();
        }
    }
    if completed != total_bytes {
        return Err(RestoreError::Integrity(
            "diagnostic coverage input changed while it was read".to_string(),
        ));
    }
    let coverage: RestorationCoverage = serde_json::from_slice(&bytes)?;
    let report = build_schema_profile(&coverage);
    let mut finished = ProgressEvent::new(
        ProgressPhase::ArchiveAudit,
        ProgressState::Completed,
        "profileArchiveSchema",
        ProgressUnit::Bytes,
        total_bytes,
        total_bytes,
        total_bytes,
        total_bytes,
    );
    finished.source_record_count = Some(report.table_count);
    finished.elapsed_milliseconds = Some(elapsed_milliseconds(started));
    progress.observe(finished);
    Ok(report)
}

fn build_schema_profile(coverage: &RestorationCoverage) -> DiagnosticSchemaProfileReport {
    let mut report = DiagnosticSchemaProfileReport {
        format_version: 1,
        privacy_safe: true,
        ..Default::default()
    };
    let mut other_families = BTreeMap::<(String, String), SchemaFamilyAccumulator>::new();
    for table in &coverage.all_tables {
        report.table_count = report.table_count.saturating_add(1);
        let source_rows = table.source_row_count.unwrap_or_default();
        report.source_row_count = report.source_row_count.saturating_add(source_rows);
        *report
            .role_counts
            .entry(table_role_name(table.role).to_string())
            .or_default() += 1;
        *report
            .classification_reason_counts
            .entry(table.classification_reason.clone())
            .or_default() += 1;
        if table.role != TableCoverageRole::Other {
            continue;
        }
        report.other_table_count = report.other_table_count.saturating_add(1);
        report.other_source_row_count = report.other_source_row_count.saturating_add(source_rows);
        let key = (
            diagnostic_identifier_family(&table.source_logical_path, true),
            diagnostic_identifier_family(&table.source_table_name, false),
        );
        let family = other_families.entry(key).or_default();
        family.table_count = family.table_count.saturating_add(1);
        family.source_row_count = family.source_row_count.saturating_add(source_rows);
        if source_rows > 0 {
            family.non_empty_table_count = family.non_empty_table_count.saturating_add(1);
        }
        if let Some(fingerprint) = &table.schema_fingerprint {
            family.schema_fingerprints.insert(fingerprint.clone());
        }
        family.column_set_variants.insert(
            table
                .columns
                .iter()
                .map(|column| diagnostic_identifier_family(column, false))
                .collect(),
        );
    }
    report.other_families = other_families
        .into_iter()
        .map(|((logical_database_family, table_family), family)| {
            let variant_count = family.column_set_variants.len();
            DiagnosticSchemaFamilyProfile {
                logical_database_family,
                table_family,
                table_count: family.table_count,
                non_empty_table_count: family.non_empty_table_count,
                source_row_count: family.source_row_count,
                schema_variant_count: family.schema_fingerprints.len().max(variant_count),
                column_set_variants_truncated: variant_count > MAX_COLUMN_SET_VARIANTS,
                column_set_variants: family
                    .column_set_variants
                    .into_iter()
                    .take(MAX_COLUMN_SET_VARIANTS)
                    .collect(),
            }
        })
        .collect();
    report
}

fn table_role_name(role: TableCoverageRole) -> &'static str {
    match role {
        TableCoverageRole::Message => "message",
        TableCoverageRole::KnownAuxiliary => "knownAuxiliary",
        TableCoverageRole::Other => "other",
        TableCoverageRole::UnhandledMessageCandidate => "unhandledMessageCandidate",
    }
}

fn diagnostic_identifier_family(value: &str, allow_path_separators: bool) -> String {
    let normalized = value.replace('\\', "/").to_ascii_lowercase();
    let allowed = |byte: u8| {
        byte.is_ascii_alphanumeric()
            || matches!(byte, b'_' | b'-' | b'.')
            || (allow_path_separators && byte == b'/')
    };
    if normalized.is_empty()
        || normalized.len() > 256
        || !normalized.bytes().all(allowed)
        || normalized.split('/').any(|component| component.is_empty())
    {
        return if allow_path_separators {
            "opaque/database".to_string()
        } else {
            "opaque_identifier".to_string()
        };
    }
    normalized
        .split('/')
        .map(redact_identifier_suffix)
        .collect::<Vec<_>>()
        .join("/")
}

fn redact_identifier_suffix(value: &str) -> String {
    let Some((prefix, suffix)) = value.rsplit_once('_') else {
        return value.to_string();
    };
    if suffix.len() >= 16 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        format!("{prefix}_{{hash}}")
    } else if !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        format!("{prefix}_{{n}}")
    } else {
        value.to_string()
    }
}

fn elapsed_milliseconds(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub fn profile_archive_payloads(
    archive: &Path,
) -> Result<DiagnosticPayloadProfileReport, RestoreError> {
    profile_archive_payloads_with_progress(archive, &NoProgress)
}

pub fn profile_archive_payloads_with_progress(
    archive: &Path,
    progress: &dyn ProgressObserver,
) -> Result<DiagnosticPayloadProfileReport, RestoreError> {
    let messages = archive.join("messages.ndjson");
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&messages)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.mode() & 0o077 != 0 {
        return Err(RestoreError::Integrity(
            "diagnostic message ledger must be a private regular file".to_string(),
        ));
    }
    let total_bytes = metadata.len();
    progress.observe(ProgressEvent::new(
        ProgressPhase::ArchiveAudit,
        ProgressState::Started,
        "profileArchivePayloads",
        ProgressUnit::Bytes,
        0,
        total_bytes,
        0,
        total_bytes,
    ));

    let mut report = DiagnosticPayloadProfileReport {
        format_version: 2,
        privacy_safe: true,
        ..Default::default()
    };
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut completed_bytes = 0_u64;
    let report_increment = (total_bytes / 100).max(1024 * 1024).max(1);
    let mut next_report = report_increment;
    let mut last_report = Instant::now();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        if read > MAX_DIAGNOSTIC_RECORD_BYTES {
            return Err(RestoreError::Integrity(format!(
                "diagnostic message record exceeds the {MAX_DIAGNOSTIC_RECORD_BYTES}-byte limit"
            )));
        }
        completed_bytes = completed_bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if line.iter().all(u8::is_ascii_whitespace) {
            return Err(RestoreError::Integrity(
                "diagnostic message ledger contains an empty record".to_string(),
            ));
        }
        let message: CanonicalMessage = serde_json::from_slice(&line)?;
        observe_message(&mut report, &message)?;
        if completed_bytes >= next_report || last_report.elapsed() >= Duration::from_secs(2) {
            let mut event = ProgressEvent::new(
                ProgressPhase::ArchiveAudit,
                ProgressState::Advanced,
                "profileArchivePayloads",
                ProgressUnit::Bytes,
                completed_bytes.min(total_bytes),
                total_bytes,
                completed_bytes.min(total_bytes),
                total_bytes,
            );
            event.source_record_count = Some(report.message_count);
            progress.observe(event);
            next_report = completed_bytes.saturating_add(report_increment);
            last_report = Instant::now();
        }
    }
    let mut finished = ProgressEvent::new(
        ProgressPhase::ArchiveAudit,
        ProgressState::Completed,
        "profileArchivePayloads",
        ProgressUnit::Bytes,
        total_bytes,
        total_bytes,
        total_bytes,
        total_bytes,
    );
    finished.source_record_count = Some(report.message_count);
    progress.observe(finished);
    Ok(report)
}

fn observe_message(
    report: &mut DiagnosticPayloadProfileReport,
    message: &CanonicalMessage,
) -> Result<(), RestoreError> {
    report.message_count = report.message_count.saturating_add(1);
    let raw_xml = message_raw_xml(message);
    for relationship in &message.relationships {
        report.relationship_reference_count = report.relationship_reference_count.saturating_add(1);
        match relationship_identifier_evidence(relationship, raw_xml) {
            RelationshipIdentifierEvidence::Present => {
                report.relationship_identifier_present_count = report
                    .relationship_identifier_present_count
                    .saturating_add(1)
            }
            RelationshipIdentifierEvidence::RecoverableFromDecodedXml => {
                report.relationship_identifier_recoverable_from_decoded_xml_count = report
                    .relationship_identifier_recoverable_from_decoded_xml_count
                    .saturating_add(1)
            }
            RelationshipIdentifierEvidence::MissingFromDecodedXml => {
                report.relationship_identifier_missing_from_decoded_xml_count = report
                    .relationship_identifier_missing_from_decoded_xml_count
                    .saturating_add(1)
            }
            RelationshipIdentifierEvidence::DecodedXmlUnavailable => {
                report.relationship_decoded_xml_unavailable_count = report
                    .relationship_decoded_xml_unavailable_count
                    .saturating_add(1)
            }
        }
    }
    let adapter = source_adapter(&message.source_table_name);
    let type_name = message
        .logical_type
        .map_or_else(|| "missing".to_string(), |value| value.to_string());
    let sub_type_name = message
        .sub_type
        .map_or_else(|| "missing".to_string(), |value| value.to_string());
    let key = format!("{adapter}:{type_name}:{sub_type_name}");
    let profile =
        report
            .adapter_type_profiles
            .entry(key)
            .or_insert_with(|| DiagnosticAdapterTypeProfile {
                source_adapter: adapter.to_string(),
                logical_type: message.logical_type,
                logical_sub_type: message.sub_type,
                ..Default::default()
            });
    profile.record_count = profile.record_count.saturating_add(1);
    *profile
        .semantic_decode_state_counts
        .entry(semantic_state_name(message.semantic_decode_state).to_string())
        .or_default() += 1;
    if let Some(reason) = message.semantic_gap_reason.as_ref() {
        *profile
            .semantic_gap_reason_counts
            .entry(reason.clone())
            .or_default() += 1;
        if let Some(raw_xml) = message_raw_xml(message) {
            profile.semantic_gap_xml_value_count =
                profile.semantic_gap_xml_value_count.saturating_add(1);
            let shape = xml_gap_shape(raw_xml);
            if profile.semantic_gap_xml_shapes.len() < MAX_GAP_SHAPES_PER_PROFILE
                && !profile
                    .semantic_gap_xml_shapes
                    .iter()
                    .any(|observed| observed.sha256 == shape.sha256)
            {
                profile.semantic_gap_xml_shapes.push(shape);
            }
        }
    }

    match message.content_base64.as_deref() {
        Some(encoded) => {
            let bytes = decode_base64(encoded)?;
            profile.canonical_content.blob_count =
                profile.canonical_content.blob_count.saturating_add(1);
            profile.canonical_content.observe_bytes(&bytes);
        }
        None => {
            profile.canonical_content.null_count =
                profile.canonical_content.null_count.saturating_add(1);
        }
    }
    for (column, value) in &message.raw_columns {
        let value_profile = profile.raw_columns.entry(column.clone()).or_default();
        match value {
            RawSQLiteValue::Null => {
                value_profile.null_count = value_profile.null_count.saturating_add(1)
            }
            RawSQLiteValue::Integer(_) => {
                value_profile.integer_count = value_profile.integer_count.saturating_add(1)
            }
            RawSQLiteValue::Real(_) => {
                value_profile.real_count = value_profile.real_count.saturating_add(1)
            }
            RawSQLiteValue::TextBase64(encoded) => {
                let bytes = decode_base64(encoded)?;
                value_profile.text_count = value_profile.text_count.saturating_add(1);
                value_profile.observe_bytes(&bytes);
            }
            RawSQLiteValue::BlobBase64(encoded) => {
                let bytes = decode_base64(encoded)?;
                value_profile.blob_count = value_profile.blob_count.saturating_add(1);
                value_profile.observe_bytes(&bytes);
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelationshipIdentifierEvidence {
    Present,
    RecoverableFromDecodedXml,
    MissingFromDecodedXml,
    DecodedXmlUnavailable,
}

fn relationship_identifier_evidence(
    relationship: &MessageRelationship,
    raw_xml: Option<&str>,
) -> RelationshipIdentifierEvidence {
    if relationship.target_server_id.is_some() || relationship.target_local_id.is_some() {
        return RelationshipIdentifierEvidence::Present;
    }
    let Some(raw_xml) = raw_xml else {
        return RelationshipIdentifierEvidence::DecodedXmlUnavailable;
    };
    let server_tags: &[&str] = match relationship.kind {
        MessageRelationshipKind::Recall => &["newmsgid", "svrid", "msgid"],
        _ => &["refermsgsvrid", "svrid", "newmsgid"],
    };
    let local_tags = &["refermsglocalid", "localid", "msglocalid"];
    if crate::restore::extract_tagged_i64(raw_xml.as_bytes(), server_tags).is_some()
        || crate::restore::extract_tagged_i64(raw_xml.as_bytes(), local_tags).is_some()
    {
        RelationshipIdentifierEvidence::RecoverableFromDecodedXml
    } else {
        RelationshipIdentifierEvidence::MissingFromDecodedXml
    }
}

fn decode_base64(encoded: &str) -> Result<Zeroizing<Vec<u8>>, RestoreError> {
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map(Zeroizing::new)
        .map_err(|_| {
            RestoreError::Integrity(
                "diagnostic message ledger contains malformed base64".to_string(),
            )
        })
}

impl DiagnosticValueShapeProfile {
    fn observe_bytes(&mut self, bytes: &[u8]) {
        let byte_count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        self.present_byte_value_count = self.present_byte_value_count.saturating_add(1);
        self.total_byte_count = self.total_byte_count.saturating_add(byte_count);
        self.minimum_byte_count = Some(
            self.minimum_byte_count
                .map_or(byte_count, |observed| observed.min(byte_count)),
        );
        self.maximum_byte_count = Some(
            self.maximum_byte_count
                .map_or(byte_count, |observed| observed.max(byte_count)),
        );
        if bytes.is_empty() {
            self.empty_byte_value_count = self.empty_byte_value_count.saturating_add(1);
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            self.binary_count = self.binary_count.saturating_add(1);
            return;
        };
        let trimmed = text.trim();
        let xml_options = ParsingOptions {
            allow_dtd: false,
            ..ParsingOptions::default()
        };
        if !trimmed.is_empty() && Document::parse_with_options(trimmed, xml_options).is_ok() {
            self.utf8_xml_count = self.utf8_xml_count.saturating_add(1);
        } else if !trimmed.is_empty() && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
        {
            self.utf8_json_count = self.utf8_json_count.saturating_add(1);
        } else {
            self.other_utf8_count = self.other_utf8_count.saturating_add(1);
        }
    }
}

fn source_adapter(table: &str) -> &'static str {
    let lower = table.to_ascii_lowercase();
    if lower == "fmessagetable" {
        "friendContactEvent"
    } else if lower == "chatbot_message" {
        "chatbot"
    } else if lower == "revokemessage" {
        "revokeMetadata"
    } else if lower
        .strip_prefix("msg_")
        .or_else(|| lower.strip_prefix("chat_"))
        .is_some_and(|suffix| {
            suffix.len() == 32 && suffix.bytes().all(|value| value.is_ascii_hexdigit())
        })
    {
        "hashedConversation"
    } else {
        "signatureMatched"
    }
}

fn semantic_state_name(state: SemanticDecodeState) -> &'static str {
    match state {
        SemanticDecodeState::Complete => "complete",
        SemanticDecodeState::Partial => "partial",
        SemanticDecodeState::UnknownType => "unknownType",
        SemanticDecodeState::Failed => "failed",
        SemanticDecodeState::MissingType => "missingType",
    }
}

fn message_raw_xml(message: &CanonicalMessage) -> Option<&str> {
    let TypedPayload::Decoded(value) = &message.typed_payload else {
        return None;
    };
    value.as_object()?.values().find_map(|variant| {
        variant
            .as_object()?
            .get("raw_xml")
            .and_then(serde_json::Value::as_str)
    })
}

fn xml_gap_shape(xml: &str) -> DiagnosticXmlGapShape {
    let bytes = xml.as_bytes();
    let mut digest = Sha256::new();
    digest.update(bytes);
    let lower = xml.to_ascii_lowercase();
    let msg_open_offset = tag_offset(&lower, "msg", false);
    let msg_close_offset = tag_offset(&lower, "msg", true);
    let appmsg_open_offset = tag_offset(&lower, "appmsg", false);
    let appinfo_open_offset = tag_offset(&lower, "appinfo", false);
    let (tag_tokens, tag_token_count, tag_tokens_truncated) = structural_tag_tokens(xml);
    let first_tag_name = tag_tokens.first().map(|token| token.name.clone());
    let last_tag_name = tag_tokens.last().map(|token| token.name.clone());
    DiagnosticXmlGapShape {
        byte_count: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        sha256: hex::encode(digest.finalize()),
        direct_parse_verdict: xml_parse_verdict(xml),
        first_tag_name,
        last_tag_name,
        tag_token_count,
        tag_tokens_truncated,
        msg_open_offset: to_u64(msg_open_offset),
        msg_close_offset: to_u64(msg_close_offset),
        appmsg_open_offset: to_u64(appmsg_open_offset),
        appinfo_open_offset: to_u64(appinfo_open_offset),
        offset_shape: offset_shape(
            bytes.len(),
            msg_open_offset,
            msg_close_offset,
            appmsg_open_offset,
            appinfo_open_offset,
        ),
        tag_tokens,
    }
}

fn xml_parse_verdict(xml: &str) -> String {
    let options = ParsingOptions {
        allow_dtd: false,
        ..ParsingOptions::default()
    };
    let Err(error) = Document::parse_with_options(xml, options) else {
        return "valid".to_string();
    };
    let error = error.to_string().to_ascii_lowercase();
    if error.contains("unknown token") {
        "invalidUnknownToken"
    } else if error.contains("multiple root") || error.contains("root node") {
        "invalidRootStructure"
    } else if error.contains("mismatched") || error.contains("close") {
        "invalidMismatchedClose"
    } else if error.contains("expected") || error.contains("end") {
        "invalidTruncatedOrIncomplete"
    } else if error.contains("attribute") {
        "invalidAttribute"
    } else {
        "invalidOther"
    }
    .to_string()
}

fn structural_tag_tokens(xml: &str) -> (Vec<DiagnosticXmlTagToken>, u64, bool) {
    let bytes = xml.as_bytes();
    let lower = xml.to_ascii_lowercase();
    let mut tokens = Vec::new();
    let mut token_count = 0_u64;
    let mut depth = 0_u64;
    let mut cursor = 0_usize;
    while cursor < bytes.len() {
        let Some(relative) = bytes[cursor..].iter().position(|value| *value == b'<') else {
            break;
        };
        let start = cursor.saturating_add(relative);
        if lower[start..].starts_with("<!--") {
            cursor = lower[start + 4..]
                .find("-->")
                .map_or(bytes.len(), |end| start + 4 + end + 3);
            continue;
        }
        if lower[start..].starts_with("<![cdata[") {
            cursor = lower[start + 9..]
                .find("]]>")
                .map_or(bytes.len(), |end| start + 9 + end + 3);
            continue;
        }
        let Some(relative_end) = find_tag_end_bytes(&bytes[start..]) else {
            break;
        };
        let end = start.saturating_add(relative_end);
        let body = xml[start + 1..end].trim();
        let (kind, name_source) = if let Some(value) = body.strip_prefix('/') {
            depth = depth.saturating_sub(1);
            (DiagnosticXmlTagKind::Close, value.trim_start())
        } else if let Some(value) = body.strip_prefix('?') {
            (
                DiagnosticXmlTagKind::ProcessingInstruction,
                value.trim_start(),
            )
        } else if body.starts_with('!') {
            cursor = end.saturating_add(1);
            continue;
        } else if body.trim_end().ends_with('/') {
            (DiagnosticXmlTagKind::SelfClosing, body)
        } else {
            (DiagnosticXmlTagKind::Open, body)
        };
        let raw_name = name_source
            .split(|character: char| {
                character.is_ascii_whitespace() || matches!(character, '/' | '?')
            })
            .next()
            .unwrap_or_default();
        if let Some(name) = diagnostic_tag_name(raw_name) {
            token_count = token_count.saturating_add(1);
            if tokens.len() < MAX_TAG_TOKENS_PER_SHAPE {
                tokens.push(DiagnosticXmlTagToken {
                    name,
                    kind,
                    byte_offset: u64::try_from(start).unwrap_or(u64::MAX),
                    depth,
                });
            }
            if matches!(kind, DiagnosticXmlTagKind::Open) {
                depth = depth.saturating_add(1);
            }
        }
        cursor = end.saturating_add(1);
    }
    (
        tokens,
        token_count,
        token_count > MAX_TAG_TOKENS_PER_SHAPE as u64,
    )
}

fn diagnostic_tag_name(value: &str) -> Option<String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':' | b'.'))
    {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

fn tag_offset(lower: &str, name: &str, closing: bool) -> Option<usize> {
    let prefix = if closing {
        format!("</{name}")
    } else {
        format!("<{name}")
    };
    let mut cursor = 0_usize;
    while let Some(relative) = lower[cursor..].find(&prefix) {
        let start = cursor.saturating_add(relative);
        let next = lower.as_bytes().get(start.saturating_add(prefix.len()));
        if next.is_some_and(|byte| matches!(byte, b'>' | b'/' | b' ' | b'\t' | b'\r' | b'\n')) {
            return Some(start);
        }
        cursor = start.saturating_add(prefix.len());
    }
    None
}

fn offset_shape(
    byte_count: usize,
    msg_open: Option<usize>,
    msg_close: Option<usize>,
    appmsg_open: Option<usize>,
    appinfo_open: Option<usize>,
) -> String {
    format!(
        "msgOpen:{};msgClose:{};appmsgOpen:{};appinfoOpen:{}",
        offset_category(byte_count, msg_open),
        offset_category(byte_count, msg_close),
        offset_category(byte_count, appmsg_open),
        offset_category(byte_count, appinfo_open),
    )
}

fn offset_category(byte_count: usize, offset: Option<usize>) -> &'static str {
    let Some(offset) = offset else {
        return "absent";
    };
    if offset == 0 {
        "start"
    } else if offset <= 32 {
        "prefix"
    } else if offset.saturating_add(32) >= byte_count {
        "suffix"
    } else {
        "middle"
    }
}

fn to_u64(value: Option<usize>) -> Option<u64> {
    value.map(|offset| u64::try_from(offset).unwrap_or(u64::MAX))
}

fn find_tag_end_bytes(value: &[u8]) -> Option<usize> {
    let mut quote = None;
    for (index, byte) in value.iter().copied().enumerate() {
        match (quote, byte) {
            (Some(expected), observed) if observed == expected => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => return Some(index),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_shape_profile_never_serializes_observed_values() {
        let mut profile = DiagnosticValueShapeProfile::default();
        profile.observe_bytes(br#"<msg private="secret">body</msg>"#);
        profile.observe_bytes(br#"{"private":"secret"}"#);
        profile.observe_bytes(b"ordinary private text");
        profile.observe_bytes(&[0xff, 0x00, 0x01]);
        let serialized = serde_json::to_string(&profile).unwrap();
        assert_eq!(profile.utf8_xml_count, 1);
        assert_eq!(profile.utf8_json_count, 1);
        assert_eq!(profile.other_utf8_count, 1);
        assert_eq!(profile.binary_count, 1);
        for private_value in ["secret", "body", "ordinary private text", "msg"] {
            assert!(!serialized.contains(private_value));
        }
    }

    #[test]
    fn source_adapter_hides_hashed_conversation_table_names() {
        assert_eq!(
            source_adapter("Msg_29a6db07e8bbdb53f5d54cc3c309f3f1"),
            "hashedConversation"
        );
        assert_eq!(source_adapter("FMessageTable"), "friendContactEvent");
    }

    #[test]
    fn relationship_profile_detects_identifiers_recoverable_from_decoded_xml() {
        let relationship = MessageRelationship {
            kind: MessageRelationshipKind::Quote,
            target_canonical_id: None,
            target_server_id: None,
            target_local_id: None,
            resolved: false,
            resolution_state: crate::RelationshipResolutionState::ReferenceIdentifierMissing,
            raw_reference_base64: None,
        };
        assert_eq!(
            relationship_identifier_evidence(
                &relationship,
                Some("<msg><appmsg><refermsg><svrid>4242</svrid></refermsg></appmsg></msg>")
            ),
            RelationshipIdentifierEvidence::RecoverableFromDecodedXml
        );
        assert_eq!(
            relationship_identifier_evidence(
                &relationship,
                Some(
                    "<msg><appmsg><refermsg><content>synthetic</content></refermsg></appmsg></msg>"
                )
            ),
            RelationshipIdentifierEvidence::MissingFromDecodedXml
        );
        assert_eq!(
            relationship_identifier_evidence(&relationship, None),
            RelationshipIdentifierEvidence::DecodedXmlUnavailable
        );
    }

    #[test]
    fn schema_profile_groups_hashed_tables_without_serializing_the_hash() {
        let private_hash = "29a6db07e8bbdb53f5d54cc3c309f3f1";
        let coverage = RestorationCoverage {
            format_version: 4,
            decoder_name: "test".to_string(),
            decoder_version: "1".to_string(),
            snapshot_manifest_format_version: 2,
            schema_profile_fingerprint: None,
            message_tables: Vec::new(),
            all_tables: vec![crate::TableSchemaCoverage {
                source_set_id: "private-source".to_string(),
                source_logical_path: "message/message_0.db".to_string(),
                source_table_id: "private-table-id".to_string(),
                source_table_name: format!("Aux_{private_hash}"),
                columns: vec!["local_id".to_string(), "content".to_string()],
                source_row_count: Some(3),
                schema_fingerprint: Some("fingerprint".to_string()),
                role: TableCoverageRole::Other,
                classification_reason: "unclassified".to_string(),
                availability: crate::TableCoverageAvailability::Complete,
                limitation_code: None,
            }],
            logical_type_counts: BTreeMap::new(),
            logical_sub_type_counts: BTreeMap::new(),
            unknown_payload_reason_counts: BTreeMap::new(),
            semantic_gap_reason_counts: BTreeMap::new(),
        };
        let report = build_schema_profile(&coverage);
        assert_eq!(report.other_table_count, 1);
        assert_eq!(report.other_source_row_count, 3);
        assert_eq!(report.other_families[0].table_family, "aux_{hash}");
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains(private_hash));
        assert!(!serialized.contains("private-source"));
        assert!(!serialized.contains("private-table-id"));
    }

    #[test]
    fn gap_shape_reports_only_structure_and_omits_text_and_attributes() {
        let xml = r#"transport<appmsg private="secret"><title>private body</title></appmsg><appinfo></appinfo></msg>"#;
        let shape = xml_gap_shape(xml);
        assert_eq!(shape.first_tag_name.as_deref(), Some("appmsg"));
        assert_eq!(shape.last_tag_name.as_deref(), Some("msg"));
        assert!(shape.msg_open_offset.is_none());
        assert!(shape.msg_close_offset.is_some());
        let serialized = serde_json::to_string(&shape).unwrap();
        for private_value in ["secret", "private body", "transport"] {
            assert!(!serialized.contains(private_value));
        }
    }
}
