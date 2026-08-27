use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalMessage {
    pub canonical_id: String,
    pub account_id: String,
    pub source_set_id: String,
    pub source_logical_path: String,
    pub source_table_id: String,
    pub source_table_name: String,
    pub source_row_id: i64,
    pub conversation_id: String,
    pub conversation_source_identifier_base64: String,
    pub sender_id: Option<String>,
    pub sender_source_identifier_base64: Option<String>,
    pub local_id: Option<i64>,
    pub server_id: Option<i64>,
    pub sort_sequence: Option<i64>,
    pub created_at_unix: Option<i64>,
    pub conversation_ordinal: u64,
    pub ordering_basis: MessageOrderingBasis,
    pub raw_type: Option<i64>,
    pub logical_type: Option<u32>,
    pub sub_type: Option<u32>,
    pub status: Option<i64>,
    pub direction: MessageDirection,
    pub direction_evidence: DirectionEvidence,
    pub content_base64: Option<String>,
    pub packed_info_base64: Option<String>,
    pub compression_type: Option<i64>,
    pub raw_columns: BTreeMap<String, RawSQLiteValue>,
    pub typed_payload: TypedPayload,
    pub semantic_decode_state: SemanticDecodeState,
    pub semantic_gap_reason: Option<String>,
    pub relationships: Vec<MessageRelationship>,
    pub artifact_references: Vec<MessageArtifactReference>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageDirection {
    Incoming,
    Outgoing,
    #[default]
    Unknown,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DirectionEvidence {
    ExplicitSourceColumn,
    SenderMatchesConversation,
    SenderDiffersFromConversation,
    SenderMatchesAccount,
    SenderDiffersFromAccount,
    #[default]
    Unresolved,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageOrderingBasis {
    SortSequence,
    ServerId,
    CreatedAt,
    LocalId,
    #[default]
    HybridSourceFallback,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRelationship {
    pub kind: MessageRelationshipKind,
    pub target_canonical_id: Option<String>,
    pub target_server_id: Option<i64>,
    pub target_local_id: Option<i64>,
    pub resolved: bool,
    pub resolution_state: RelationshipResolutionState,
    pub raw_reference_base64: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RelationshipResolutionState {
    Pending,
    Resolved,
    TargetNotPresentLocally,
    ReferenceIdentifierMissing,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageRelationshipKind {
    Quote,
    Reply,
    Recall,
    Edit,
    Reaction,
    MergedChild,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageArtifactReference {
    pub artifact_id: String,
    pub role: ArtifactRole,
    pub preferred: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArtifactKind {
    Image,
    AnimatedImage,
    Voice,
    Video,
    Document,
    Thumbnail,
    RichMedia,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArtifactRole {
    Original,
    HighResolution,
    Thumbnail,
    VoicePayload,
    VideoPayload,
    VideoPoster,
    FilePayload,
    StickerPayload,
    Auxiliary,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArtifactAvailability {
    Downloaded,
    MaterializedFromDatabase,
    NotDownloaded,
    RemoteOnly,
    Expired,
    Deleted,
    Corrupt,
    Ambiguous,
    MetadataMissing,
    UnsafePath,
    AccountRootUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArtifactDecodeState {
    NotRequired,
    Decoded,
    KeyUnavailable,
    Unsupported,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalArtifact {
    pub artifact_id: String,
    pub kind: ArtifactKind,
    pub role: ArtifactRole,
    pub availability: ArtifactAvailability,
    pub source_md5: Option<String>,
    pub source_local_path: Option<String>,
    pub account_relative_path: Option<String>,
    pub source_byte_count: Option<u64>,
    pub source_device_id: Option<u64>,
    pub source_file_id: Option<u64>,
    pub source_modified_seconds: Option<i64>,
    pub source_modified_nanoseconds: Option<i64>,
    pub source_sha256: Option<String>,
    pub detected_format: Option<String>,
    pub materialized_local_path: Option<String>,
    pub decoded_local_path: Option<String>,
    pub decoded_byte_count: Option<u64>,
    pub decoded_sha256: Option<String>,
    pub decoded_format: Option<String>,
    pub decode_state: ArtifactDecodeState,
    pub verification_detail: Option<String>,
    pub source_resource_set_id: Option<String>,
    pub source_resource_row_id: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConversationKind {
    Direct,
    Group,
    Business,
    Chatbot,
    System,
    Unresolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntityDecodeState {
    Complete,
    RawOnly,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LocalProfileState {
    Hydrated,
    MissingLocalRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntitySourceRecord {
    pub source_set_id: String,
    pub source_logical_path: String,
    pub source_table_id: String,
    pub source_table_name: String,
    pub source_row_id: i64,
    pub raw_columns: BTreeMap<String, RawSQLiteValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalParticipant {
    pub participant_id: String,
    pub account_id: String,
    pub source_identifier_base64: String,
    pub alias_base64: Option<String>,
    pub remark_base64: Option<String>,
    pub nickname_base64: Option<String>,
    pub display_name_base64: Option<String>,
    pub local_profile_state: LocalProfileState,
    pub conversation_ids: Vec<String>,
    pub source_records: Vec<EntitySourceRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConversationMembershipRole {
    DirectPeer,
    Owner,
    Member,
    ObservedSender,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMembership {
    pub participant_id: String,
    pub role: ConversationMembershipRole,
    pub display_name_base64: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalConversation {
    pub conversation_id: String,
    pub account_id: String,
    pub source_identifier_base64: String,
    pub kind: ConversationKind,
    pub participant_ids: Vec<String>,
    pub memberships: Vec<ConversationMembership>,
    pub owner_participant_id: Option<String>,
    pub entity_decode_state: EntityDecodeState,
    pub source_records: Vec<EntitySourceRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "storageClass", content = "value", rename_all = "camelCase")]
pub enum RawSQLiteValue {
    Null,
    Integer(i64),
    Real(f64),
    TextBase64(String),
    BlobBase64(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum TypedPayload {
    Decoded(serde_json::Value),
    Unknown { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SemanticDecodeState {
    Complete,
    Partial,
    UnknownType,
    Failed,
    MissingType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedRow {
    pub source_set_id: String,
    pub source_table_id: String,
    pub source_row_id: Option<i64>,
    pub reason: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorationIntegrity {
    pub database_count: u64,
    pub message_table_count: u64,
    pub message_candidate_gap_count: u64,
    pub source_row_count: u64,
    pub restored_row_count: u64,
    pub rejected_row_count: u64,
    pub duplicate_canonical_id_count: u64,
    pub unknown_payload_count: u64,
    pub unknown_payload_reason_counts: BTreeMap<String, u64>,
    pub semantic_gap_count: u64,
    pub semantic_gap_reason_counts: BTreeMap<String, u64>,
    pub logical_type_counts: BTreeMap<String, u64>,
    pub logical_sub_type_counts: BTreeMap<String, u64>,
    pub message_schema_counts: BTreeMap<String, u64>,
    pub artifact_reference_count: u64,
    pub unique_artifact_count: u64,
    pub downloaded_artifact_count: u64,
    pub materialized_artifact_count: u64,
    pub missing_artifact_count: u64,
    pub ambiguous_artifact_count: u64,
    pub corrupt_artifact_count: u64,
    pub unsafe_artifact_count: u64,
    pub decoded_artifact_count: u64,
    pub artifact_decode_gap_count: u64,
    pub account_root_unavailable_artifact_count: u64,
    pub relationship_reference_count: u64,
    pub resolved_relationship_count: u64,
    pub unresolved_relationship_count: u64,
    pub absent_relationship_target_count: u64,
    pub missing_relationship_identifier_count: u64,
    pub ambiguous_relationship_count: u64,
    pub ordering_basis_counts: BTreeMap<String, u64>,
    pub direction_counts: BTreeMap<String, u64>,
    pub conversation_count: u64,
    pub participant_count: u64,
    pub group_member_count: u64,
    pub entity_source_row_count: u64,
    pub entity_decode_gap_count: u64,
    pub missing_local_profile_count: u64,
    pub unresolved_conversation_count: u64,
}

impl RestorationIntegrity {
    pub fn row_equation_holds(&self) -> bool {
        self.source_row_count == self.restored_row_count + self.rejected_row_count
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorationReport {
    pub format_version: u32,
    pub account_id: String,
    pub source_fingerprint: String,
    #[serde(default)]
    pub client_build_compatibility: crate::ClientBuildCompatibilityEvidence,
    #[serde(default)]
    pub acquisition: Option<crate::SnapshotAcquisitionEvidence>,
    #[serde(default)]
    pub archive_scope: RestorationArchiveScope,
    #[serde(default)]
    pub media_phase: RestorationMediaPhase,
    pub messages_path: String,
    pub rejections_path: String,
    pub artifacts_path: String,
    pub conversations_path: String,
    pub participants_path: String,
    pub coverage_path: String,
    pub report_path: String,
    pub integrity: RestorationIntegrity,
    pub completion: RestorationCompletion,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RestorationMediaPhase {
    #[default]
    Resolved,
    Deferred,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RestorationArchiveScope {
    #[default]
    Authoritative,
    IncrementalFragment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorationCompletion {
    pub row_equation_holds: bool,
    pub zero_rejected_rows: bool,
    pub canonical_identities_unique: bool,
    pub semantic_message_coverage_complete: bool,
    pub directions_complete: bool,
    pub entity_coverage_complete: bool,
    pub relationship_coverage_complete: bool,
    pub artifact_verification_complete: bool,
    pub artifact_decoding_complete: bool,
    pub full_restoration_achieved: bool,
}

impl RestorationCompletion {
    pub fn evaluate(integrity: &RestorationIntegrity) -> Self {
        let row_equation_holds = integrity.row_equation_holds();
        let zero_rejected_rows = integrity.rejected_row_count == 0;
        let canonical_identities_unique = integrity.duplicate_canonical_id_count == 0;
        let semantic_message_coverage_complete =
            integrity.semantic_gap_count == 0 && integrity.message_candidate_gap_count == 0;
        let directions_complete = integrity
            .direction_counts
            .get("unknown")
            .copied()
            .unwrap_or_default()
            == 0;
        let entity_coverage_complete =
            integrity.entity_decode_gap_count == 0 && integrity.unresolved_conversation_count == 0;
        let relationship_coverage_complete = integrity.missing_relationship_identifier_count == 0
            && integrity.ambiguous_relationship_count == 0;
        let artifact_verification_complete = integrity.ambiguous_artifact_count == 0
            && integrity.corrupt_artifact_count == 0
            && integrity.unsafe_artifact_count == 0
            && integrity.account_root_unavailable_artifact_count == 0;
        let artifact_decoding_complete = integrity.artifact_decode_gap_count == 0;
        let full_restoration_achieved = row_equation_holds
            && zero_rejected_rows
            && canonical_identities_unique
            && semantic_message_coverage_complete
            && directions_complete
            && entity_coverage_complete
            && relationship_coverage_complete
            && artifact_verification_complete
            && artifact_decoding_complete;
        Self {
            row_equation_holds,
            zero_rejected_rows,
            canonical_identities_unique,
            semantic_message_coverage_complete,
            directions_complete,
            entity_coverage_complete,
            relationship_coverage_complete,
            artifact_verification_complete,
            artifact_decoding_complete,
            full_restoration_achieved,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorationCoverage {
    pub format_version: u32,
    pub decoder_name: String,
    pub decoder_version: String,
    pub snapshot_manifest_format_version: u32,
    pub message_tables: Vec<MessageTableCoverage>,
    pub all_tables: Vec<TableSchemaCoverage>,
    pub logical_type_counts: BTreeMap<String, u64>,
    pub logical_sub_type_counts: BTreeMap<String, u64>,
    pub unknown_payload_reason_counts: BTreeMap<String, u64>,
    pub semantic_gap_reason_counts: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TableCoverageRole {
    Message,
    KnownAuxiliary,
    Other,
    UnhandledMessageCandidate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableSchemaCoverage {
    pub source_set_id: String,
    pub source_logical_path: String,
    pub source_table_id: String,
    pub source_table_name: String,
    pub columns: Vec<String>,
    pub role: TableCoverageRole,
    pub classification_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageTableCoverage {
    pub source_set_id: String,
    pub source_logical_path: String,
    pub source_table_id: String,
    pub source_table_name: String,
    pub source_row_count: u64,
    pub columns: Vec<String>,
}
