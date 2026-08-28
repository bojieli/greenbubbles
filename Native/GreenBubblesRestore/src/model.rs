use std::collections::{BTreeMap, BTreeSet};

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
    /// Legacy direct-chat heuristic retained for archive decoding only.
    SenderMatchesConversation,
    /// Legacy direct-chat heuristic retained for archive decoding only.
    SenderDiffersFromConversation,
    SenderMatchesAccount,
    SenderDiffersFromAccount,
    SenderAccountConflictWithExplicitSourceColumn,
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
    /// Every role this artifact serves across all referencing messages.
    /// Artifact identity is content-based, so one physical file can
    /// legitimately appear as an original image in one message, a sticker
    /// payload in another, and so on. The single `role` field remains the
    /// primary (first-recorded) role for consumers. Archives written before
    /// this field existed deserialize with an empty set, which consumers must
    /// treat as exactly `role`.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub roles: BTreeSet<ArtifactRole>,
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
    pub source_resource_logical_path: Option<String>,
    pub source_resource_table_id: Option<String>,
    pub source_resource_table_name: Option<String>,
    pub source_resource_row_id: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CachedSurfaceCompleteness {
    PartialLocalCache,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalCachedMoment {
    pub canonical_id: String,
    pub account_id: String,
    pub source_set_id: String,
    pub source_logical_path: String,
    pub source_table_id: String,
    pub source_table_name: String,
    pub source_row_id: i64,
    pub timeline_id: RawSQLiteValue,
    pub author_id: Option<String>,
    pub author_source_identifier_base64: Option<String>,
    pub created_at_unix: Option<i64>,
    pub content_type: Option<i64>,
    pub content_description_base64: Option<String>,
    pub title_base64: Option<String>,
    pub description_base64: Option<String>,
    pub content_url_base64: Option<String>,
    pub media_count: u64,
    pub like_count: u64,
    pub comment_count: u64,
    pub raw_content_base64: Option<String>,
    pub raw_pack_info_base64: Option<String>,
    pub raw_columns: BTreeMap<String, RawSQLiteValue>,
    pub semantic_decode_state: SemanticDecodeState,
    pub semantic_gap_reason: Option<String>,
    pub cache_completeness: CachedSurfaceCompleteness,
    pub observed_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CachedMomentInteractionKind {
    Comment,
    Like,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalCachedMomentInteraction {
    pub canonical_id: String,
    pub account_id: String,
    pub source_set_id: String,
    pub source_logical_path: String,
    pub source_table_id: String,
    pub source_table_name: String,
    pub source_row_id: i64,
    pub local_id: Option<i64>,
    pub feed_id: RawSQLiteValue,
    pub created_at_unix: Option<i64>,
    pub kind: CachedMomentInteractionKind,
    pub raw_type: Option<i64>,
    pub from_participant_id: Option<String>,
    pub from_source_identifier_base64: Option<String>,
    pub from_nickname_base64: Option<String>,
    pub to_participant_id: Option<String>,
    pub to_source_identifier_base64: Option<String>,
    pub to_nickname_base64: Option<String>,
    pub content_base64: Option<String>,
    pub raw_columns: BTreeMap<String, RawSQLiteValue>,
    pub cache_completeness: CachedSurfaceCompleteness,
    pub observed_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CachedSurfaceTableRole {
    MomentTimeline,
    MomentInteraction,
    UnsupportedCandidate,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedSurfaceTableCoverage {
    pub source_set_id: String,
    pub source_logical_path: String,
    pub source_table_id: String,
    pub source_table_name: String,
    pub columns: Vec<String>,
    #[serde(default)]
    pub schema_fingerprint: Option<String>,
    pub source_row_count: u64,
    pub restored_row_count: u64,
    pub role: CachedSurfaceTableRole,
    pub classification_reason: String,
    /// Readability of this optional source table. A partial or unavailable
    /// cached table must never prevent healthy chat or cached-surface records
    /// from being published.
    #[serde(default)]
    pub availability: TableCoverageAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limitation_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedSurfaceCoverage {
    pub format_version: u32,
    #[serde(default)]
    pub schema_profile_fingerprint: Option<String>,
    pub observed_at: String,
    pub cache_completeness: CachedSurfaceCompleteness,
    pub source_database_present: bool,
    pub moment_count: u64,
    pub interaction_count: u64,
    pub semantic_gap_count: u64,
    /// Source rows known during planning but omitted because an optional
    /// cached-surface table became unreadable.
    #[serde(default)]
    pub omitted_row_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitation_codes: Vec<String>,
    pub tables: Vec<CachedSurfaceTableCoverage>,
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
    #[serde(default)]
    pub observed_table_row_count: u64,
    #[serde(default)]
    pub table_role_counts: BTreeMap<String, u64>,
    #[serde(default)]
    pub table_classification_reason_counts: BTreeMap<String, u64>,
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
    #[serde(default)]
    pub direction_conflict_count: u64,
    pub conversation_count: u64,
    pub participant_count: u64,
    pub group_member_count: u64,
    pub entity_source_row_count: u64,
    pub entity_decode_gap_count: u64,
    pub missing_local_profile_count: u64,
    pub unresolved_conversation_count: u64,
    pub cached_moment_count: u64,
    pub cached_moment_interaction_count: u64,
    pub cached_surface_semantic_gap_count: u64,
    #[serde(default)]
    pub cached_surface_omitted_row_count: u64,
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
    #[serde(default)]
    pub self_participant_id: Option<String>,
    #[serde(default)]
    pub account_binding_evidence: Option<AccountHolderBindingEvidence>,
    #[serde(default)]
    pub storage: Option<RestorationStorageEvidence>,
    pub source_fingerprint: String,
    #[serde(default)]
    pub client_build_compatibility: crate::ClientBuildCompatibilityEvidence,
    #[serde(default)]
    pub acquisition: Option<crate::SnapshotAcquisitionEvidence>,
    #[serde(default)]
    pub archive_scope: RestorationArchiveScope,
    #[serde(default)]
    pub database_coverage: Option<RestorationDatabaseCoverage>,
    #[serde(default)]
    pub media_phase: RestorationMediaPhase,
    pub messages_path: String,
    pub rejections_path: String,
    pub artifacts_path: String,
    pub conversations_path: String,
    pub participants_path: String,
    #[serde(default)]
    pub cached_moments_path: Option<String>,
    #[serde(default)]
    pub cached_moment_interactions_path: Option<String>,
    #[serde(default)]
    pub cached_surfaces_path: Option<String>,
    pub coverage_path: String,
    pub report_path: String,
    pub integrity: RestorationIntegrity,
    pub completion: RestorationCompletion,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorationStorageEvidence {
    pub format_version: u32,
    pub source_byte_count: u64,
    pub message_record_count: u64,
    pub observed_table_record_count: u64,
    pub estimated_archive_byte_count: u64,
    pub estimated_staging_byte_count: u64,
    pub estimated_peak_byte_count: u64,
    pub required_free_byte_count: u64,
    pub available_free_byte_count_at_start: u64,
    pub peak_staging_file_byte_count: u64,
    pub staged_uncompressed_byte_count: u64,
    pub staged_compressed_byte_count: u64,
    pub actual_archive_byte_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccountHolderBindingEvidence {
    SnapshotManifest,
    LegacyAccountRoot,
}

impl RestorationReport {
    /// Whether an independently audited archive is safe to apply to a replica.
    /// Partial archives are eligible only when every database in the snapshot
    /// inventory is accounted for as either fresh or explicitly unavailable.
    pub fn replica_mutation_eligible(&self) -> bool {
        match self.archive_scope {
            RestorationArchiveScope::Authoritative => {
                self.database_coverage.as_ref().is_none_or(|coverage| {
                    coverage.is_valid() && coverage.authoritative_database_coverage
                })
            }
            RestorationArchiveScope::PartialDatabaseCoverage => {
                self.format_version >= 5
                    && self.database_coverage.as_ref().is_some_and(|coverage| {
                        coverage.is_valid()
                            && !coverage.authoritative_database_coverage
                            && coverage.attempted_source_set_ids == coverage.snapshot_source_set_ids
                    })
            }
            RestorationArchiveScope::IncrementalFragment
            | RestorationArchiveScope::DiagnosticSubset => false,
        }
    }

    /// Whether an independently audited archive is safe to bootstrap a
    /// read-serving replica. A full-inventory diagnostic restoration may be
    /// useful for search/export even though it is not safe as a later
    /// synchronization authority: every source database must still be
    /// explicitly accounted for as fresh or unavailable.
    pub fn replica_serving_eligible(&self) -> bool {
        self.replica_mutation_eligible()
            || (self.archive_scope == RestorationArchiveScope::DiagnosticSubset
                && self.format_version >= 5
                && self.database_coverage.as_ref().is_some_and(|coverage| {
                    coverage.is_valid()
                        && coverage.attempted_source_set_ids == coverage.snapshot_source_set_ids
                }))
    }
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
    PartialDatabaseCoverage,
    IncrementalFragment,
    DiagnosticSubset,
}

/// Database-level freshness evidence for fault-tolerant restoration. A partial
/// archive may include records preserved from an earlier generation, but it
/// must never present those records as freshly restored from the current
/// snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorationDatabaseCoverage {
    pub format_version: u32,
    pub total_database_count: usize,
    pub attempted_database_count: usize,
    pub restored_database_count: usize,
    pub unavailable_database_count: usize,
    pub preserved_stale_database_count: usize,
    pub authoritative_database_coverage: bool,
    #[serde(rename = "snapshotSourceSetIDs")]
    pub snapshot_source_set_ids: Vec<String>,
    #[serde(rename = "attemptedSourceSetIDs")]
    pub attempted_source_set_ids: Vec<String>,
    #[serde(rename = "freshSourceSetIDs")]
    pub fresh_source_set_ids: Vec<String>,
    #[serde(rename = "unavailableSourceSetIDs")]
    pub unavailable_source_set_ids: Vec<String>,
    #[serde(rename = "preservedStaleSourceSetIDs")]
    pub preserved_stale_source_set_ids: Vec<String>,
    #[serde(default)]
    pub unavailable_databases: Vec<RestorationUnavailableDatabase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorationUnavailableDatabase {
    #[serde(rename = "sourceSetID")]
    pub source_set_id: String,
    pub logical_path: String,
    pub storage_family: String,
    pub database_byte_count: u64,
    pub write_ahead_log_byte_count: u64,
    pub reason: String,
}

impl RestorationDatabaseCoverage {
    pub fn included_source_set_ids(&self) -> BTreeSet<&str> {
        self.fresh_source_set_ids
            .iter()
            .chain(&self.preserved_stale_source_set_ids)
            .map(String::as_str)
            .collect()
    }

    pub fn is_valid(&self) -> bool {
        let sorted_unique = |values: &[String]| {
            values.iter().all(|value| !value.is_empty())
                && values.windows(2).all(|pair| pair[0] < pair[1])
        };
        if self.format_version != 1
            || !sorted_unique(&self.snapshot_source_set_ids)
            || !sorted_unique(&self.attempted_source_set_ids)
            || !sorted_unique(&self.fresh_source_set_ids)
            || !sorted_unique(&self.unavailable_source_set_ids)
            || !sorted_unique(&self.preserved_stale_source_set_ids)
        {
            return false;
        }
        let snapshot = self
            .snapshot_source_set_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let attempted = self
            .attempted_source_set_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let fresh = self
            .fresh_source_set_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let unavailable = self
            .unavailable_source_set_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let stale = self
            .preserved_stale_source_set_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let unavailable_details = self
            .unavailable_databases
            .iter()
            .map(|database| database.source_set_id.as_str())
            .collect::<BTreeSet<_>>();
        self.total_database_count == snapshot.len()
            && self.attempted_database_count == attempted.len()
            && self.restored_database_count == fresh.len()
            && self.unavailable_database_count == unavailable.len()
            && self.preserved_stale_database_count == stale.len()
            && attempted.is_subset(&snapshot)
            && fresh.is_subset(&attempted)
            && unavailable.is_subset(&attempted)
            && stale.is_subset(&unavailable)
            && self.unavailable_databases.len() == unavailable_details.len()
            && self
                .unavailable_databases
                .windows(2)
                .all(|pair| pair[0].source_set_id < pair[1].source_set_id)
            && unavailable_details == unavailable
            && self.unavailable_databases.iter().all(|database| {
                !database.logical_path.is_empty()
                    && !database.storage_family.is_empty()
                    && !database.reason.is_empty()
            })
            && fresh.is_disjoint(&unavailable)
            && fresh.len() + unavailable.len() == attempted.len()
            && self.authoritative_database_coverage
                == (attempted == snapshot && unavailable.is_empty())
    }
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
            == 0
            && integrity.direction_conflict_count == 0;
        let entity_coverage_complete =
            integrity.entity_decode_gap_count == 0 && integrity.unresolved_conversation_count == 0;
        let relationship_coverage_complete = integrity.missing_relationship_identifier_count == 0
            && integrity.ambiguous_relationship_count == 0
            && integrity.unresolved_relationship_count
                == integrity.absent_relationship_target_count;
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
            && artifact_decoding_complete
            && integrity.cached_surface_omitted_row_count == 0;
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

    /// Recompute completion in the context of the published archive. Row and
    /// semantic evidence alone cannot make deferred media, partial database
    /// coverage, a bounded fragment, or an unsupported client build a full
    /// restoration.
    pub fn evaluate_report(report: &RestorationReport) -> Self {
        let mut completion = Self::evaluate(&report.integrity);
        // Format 4 introduced these report-level conditions into the stored
        // completion contract. Older replica backups must continue to verify
        // against the completion semantics they actually persisted; their
        // separate scope/build gates remain fail-closed at mutation time.
        if report.format_version >= 4
            && (report.media_phase == RestorationMediaPhase::Deferred
                || report.archive_scope != RestorationArchiveScope::Authoritative
                || !report.client_build_compatibility.production_compatible)
        {
            completion.full_restoration_achieved = false;
        }
        completion
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorationCoverage {
    pub format_version: u32,
    pub decoder_name: String,
    pub decoder_version: String,
    pub snapshot_manifest_format_version: u32,
    #[serde(default)]
    pub schema_profile_fingerprint: Option<String>,
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
    #[serde(default)]
    pub source_row_count: Option<u64>,
    #[serde(default)]
    pub schema_fingerprint: Option<String>,
    pub role: TableCoverageRole,
    pub classification_reason: String,
    /// An unavailable table is isolated from restoration. Healthy tables stay
    /// queryable and the gap remains explicit in coverage evidence.
    #[serde(default)]
    pub availability: TableCoverageAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limitation_code: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TableCoverageAvailability {
    #[default]
    Complete,
    Partial,
    Unavailable,
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
    #[serde(default)]
    pub schema_fingerprint: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        RestorationArchiveScope, RestorationCompletion, RestorationDatabaseCoverage,
        RestorationIntegrity, RestorationMediaPhase, RestorationReport,
        RestorationUnavailableDatabase,
    };

    #[test]
    fn pending_relationships_cannot_satisfy_completion() {
        let pending = RestorationIntegrity {
            relationship_reference_count: 1,
            unresolved_relationship_count: 1,
            ..Default::default()
        };
        let pending_completion = RestorationCompletion::evaluate(&pending);
        assert!(!pending_completion.relationship_coverage_complete);
        assert!(!pending_completion.full_restoration_achieved);

        let explicitly_absent = RestorationIntegrity {
            relationship_reference_count: 1,
            unresolved_relationship_count: 1,
            absent_relationship_target_count: 1,
            ..Default::default()
        };
        assert!(RestorationCompletion::evaluate(&explicitly_absent).relationship_coverage_complete);
    }

    #[test]
    fn report_level_completion_gates_apply_from_archive_format_four() {
        let integrity = RestorationIntegrity::default();
        let mut report = RestorationReport {
            format_version: 3,
            account_id: String::new(),
            self_participant_id: None,
            account_binding_evidence: None,
            storage: None,
            source_fingerprint: String::new(),
            client_build_compatibility: Default::default(),
            acquisition: None,
            archive_scope: RestorationArchiveScope::DiagnosticSubset,
            database_coverage: None,
            media_phase: RestorationMediaPhase::Deferred,
            messages_path: String::new(),
            rejections_path: String::new(),
            artifacts_path: String::new(),
            conversations_path: String::new(),
            participants_path: String::new(),
            cached_moments_path: None,
            cached_moment_interactions_path: None,
            cached_surfaces_path: None,
            coverage_path: String::new(),
            report_path: String::new(),
            completion: RestorationCompletion::evaluate(&integrity),
            integrity,
        };
        assert!(RestorationCompletion::evaluate_report(&report).full_restoration_achieved);

        report.format_version = 4;
        assert!(!RestorationCompletion::evaluate_report(&report).full_restoration_achieved);

        report.format_version = 5;
        report.archive_scope = RestorationArchiveScope::PartialDatabaseCoverage;
        report.database_coverage = Some(RestorationDatabaseCoverage {
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
                database_byte_count: 1,
                write_ahead_log_byte_count: 0,
                reason: "syntheticUnavailable".to_string(),
            }],
        });
        assert!(report.replica_mutation_eligible());
        report
            .database_coverage
            .as_mut()
            .unwrap()
            .attempted_source_set_ids = vec!["set-a".to_string()];
        report
            .database_coverage
            .as_mut()
            .unwrap()
            .attempted_database_count = 1;
        assert!(!report.replica_mutation_eligible());

        let coverage = report.database_coverage.as_mut().unwrap();
        coverage.attempted_source_set_ids = coverage.snapshot_source_set_ids.clone();
        coverage.attempted_database_count = coverage.total_database_count;
        report.archive_scope = RestorationArchiveScope::DiagnosticSubset;
        assert!(report.replica_serving_eligible());
        assert!(!report.replica_mutation_eligible());
    }
}
