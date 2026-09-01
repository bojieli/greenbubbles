use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{Datelike, TimeZone};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::archive::{ensure_private_directory, ensure_private_regular_file};
use crate::live_query::{
    ContactKind, CorpusConversation, CorpusHydratedMessage, CorpusMessageLocation,
    CorpusMessageMetadata, LiveCorpusReader, LiveQueryError, LiveQuerySource, QuerySourceMode,
};
use crate::{ConversationKind, RestoreError};

pub const PERSONAL_MEMORY_POLICY_SCHEMA: &str = "greenbubbles.personal-memory-selection-policy.v1";
pub const PERSONAL_MEMORY_CORPUS_SCHEMA: &str = "greenbubbles.personal-memory-corpus.v1";
pub const PERSONAL_MEMORY_BATCH_SCHEMA: &str = "greenbubbles.personal-memory-batch.v1";
pub const PERSONAL_MEMORY_PAGE_SCHEMA: &str = "greenbubbles.personal-memory-page.v1";
pub const PERSONAL_MEMORY_STATE_SCHEMA: &str = "greenbubbles.personal-memory-state.v1";
pub const PERSONAL_MEMORY_FORMAT_VERSION: u32 = 1;
pub const PERSONAL_MEMORY_CURRENT_SELECTOR: &str = "current";

const MAXIMUM_POLICY_BYTES: u64 = 1024 * 1024;
const MAXIMUM_CONTROL_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAXIMUM_WIKI_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAXIMUM_WIKI_ENTRIES: usize = 100_000;
const MAXIMUM_WIKI_CITATIONS_PER_PROSE_LINE: usize = 8;
const MINIMUM_NEXT_TEXT_BYTES: usize = 16 * 1024;
const MAXIMUM_NEXT_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAXIMUM_BATCH_MESSAGES: usize = 5_000;
/// Pi's built-in read and shell tools truncate at 50 KiB. Every serialized page,
/// including its envelope and trailing newline, stays below that boundary.
pub const MAXIMUM_MEMORY_PAGE_OUTPUT_BYTES: usize = 48 * 1024;

fn default_timezone() -> String {
    "Asia/Singapore".to_string()
}

const fn default_minimum_self_messages() -> usize {
    1
}

const fn default_recent_lookback_months() -> usize {
    12
}

const fn default_direct_session_gap_minutes() -> i64 {
    12 * 60
}

const fn default_group_session_gap_minutes() -> i64 {
    60
}

const fn default_direct_context_before() -> usize {
    24
}

const fn default_direct_context_after() -> usize {
    24
}

const fn default_group_context_before() -> usize {
    12
}

const fn default_group_context_after() -> usize {
    16
}

const fn default_maximum_message_text_bytes() -> usize {
    4 * 1024
}

const fn default_maximum_unit_messages() -> usize {
    160
}

const fn default_maximum_unit_text_bytes() -> usize {
    48 * 1024
}

fn default_true() -> bool {
    true
}

const fn default_delivery_order() -> MemoryDeliveryOrder {
    MemoryDeliveryOrder::AccountHolderRelevance
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MemoryDeliveryOrder {
    /// Compatibility order for corpora prepared before relevance scheduling.
    #[default]
    Chronological,
    /// Deterministic weighted coverage of account-holder-active conversations.
    AccountHolderRelevance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PersonalMemorySelectionPolicy {
    pub schema: String,
    pub format_version: u32,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default)]
    pub reference_unix: Option<i64>,
    #[serde(default)]
    pub not_before_unix: Option<i64>,
    #[serde(default)]
    pub not_after_unix: Option<i64>,
    #[serde(default = "default_minimum_self_messages")]
    pub minimum_self_messages_per_active_month: usize,
    #[serde(default = "default_recent_lookback_months")]
    pub recent_lookback_months: usize,
    #[serde(default)]
    pub minimum_self_active_months_in_lookback: usize,
    #[serde(default = "default_direct_session_gap_minutes")]
    pub direct_session_gap_minutes: i64,
    #[serde(default = "default_group_session_gap_minutes")]
    pub group_session_gap_minutes: i64,
    #[serde(default = "default_direct_context_before")]
    pub direct_context_before: usize,
    #[serde(default = "default_direct_context_after")]
    pub direct_context_after: usize,
    #[serde(default = "default_group_context_before")]
    pub group_context_before: usize,
    #[serde(default = "default_group_context_after")]
    pub group_context_after: usize,
    #[serde(default = "default_maximum_message_text_bytes")]
    pub maximum_message_text_bytes: usize,
    #[serde(default = "default_maximum_unit_messages")]
    pub maximum_unit_messages: usize,
    #[serde(default = "default_maximum_unit_text_bytes")]
    pub maximum_unit_text_bytes: usize,
    #[serde(default = "default_delivery_order")]
    pub delivery_order: MemoryDeliveryOrder,
    #[serde(default = "default_true")]
    pub include_direct_conversations: bool,
    #[serde(default = "default_true")]
    pub include_group_conversations: bool,
    #[serde(default)]
    pub include_official_accounts: bool,
    #[serde(default)]
    pub include_service_accounts: bool,
}

impl PersonalMemorySelectionPolicy {
    fn validate(&self) -> Result<Tz, RestoreError> {
        if self.schema != PERSONAL_MEMORY_POLICY_SCHEMA
            || self.format_version != PERSONAL_MEMORY_FORMAT_VERSION
        {
            return Err(RestoreError::Integrity(
                "personal-memory selection policy schema or format version is unsupported".into(),
            ));
        }
        let timezone = self.timezone.parse::<Tz>().map_err(|_| {
            RestoreError::Integrity(
                "selection policy timezone must be a valid IANA timezone name".into(),
            )
        })?;
        if self
            .not_before_unix
            .zip(self.not_after_unix)
            .is_some_and(|(start, end)| start > end)
        {
            return Err(RestoreError::Integrity(
                "selection policy time range is inverted".into(),
            ));
        }
        if !(1..=10_000).contains(&self.minimum_self_messages_per_active_month) {
            return Err(RestoreError::Integrity(
                "minimumSelfMessagesPerActiveMonth must be between 1 and 10000".into(),
            ));
        }
        if self.recent_lookback_months > 1_200
            || self.minimum_self_active_months_in_lookback > self.recent_lookback_months
        {
            return Err(RestoreError::Integrity(
                "recent lookback settings are inconsistent or exceed 1200 months".into(),
            ));
        }
        if !(1..=30 * 24 * 60).contains(&self.direct_session_gap_minutes)
            || !(1..=30 * 24 * 60).contains(&self.group_session_gap_minutes)
        {
            return Err(RestoreError::Integrity(
                "session gaps must be between 1 minute and 30 days".into(),
            ));
        }
        for (label, value) in [
            ("directContextBefore", self.direct_context_before),
            ("directContextAfter", self.direct_context_after),
            ("groupContextBefore", self.group_context_before),
            ("groupContextAfter", self.group_context_after),
        ] {
            if value > 10_000 {
                return Err(RestoreError::Integrity(format!(
                    "{label} must not exceed 10000 messages"
                )));
            }
        }
        if !(1..=16 * 1024).contains(&self.maximum_message_text_bytes) {
            return Err(RestoreError::Integrity(
                "maximumMessageTextBytes must be between 1 and 16384".into(),
            ));
        }
        if !(1..=1_000).contains(&self.maximum_unit_messages)
            || !(16 * 1024..=512 * 1024).contains(&self.maximum_unit_text_bytes)
            || self.maximum_unit_text_bytes < self.maximum_message_text_bytes
        {
            return Err(RestoreError::Integrity(
                "unit bounds require 1..1000 messages and 16384..524288 text bytes, with the unit bound at least the message bound"
                    .into(),
            ));
        }
        Ok(timezone)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CorpusFileRecord {
    pub relative_path: String,
    pub byte_count: u64,
    #[serde(rename = "sha256")]
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalMemoryCorpusManifest {
    pub schema: String,
    pub format_version: u32,
    pub generated_at_unix_milliseconds: u64,
    pub source_mode: QuerySourceMode,
    pub source_identity: String,
    #[serde(rename = "selectionPolicySHA256")]
    pub selection_policy_sha256: String,
    pub timezone: String,
    /// Missing in legacy format-1 corpora, which were always chronological.
    #[serde(default)]
    pub delivery_order: MemoryDeliveryOrder,
    pub reference_unix: i64,
    pub account_holder_attribution_bound: bool,
    pub content_trust: String,
    pub immutable_index: bool,
    pub source_coverage_complete: bool,
    pub content_complete: bool,
    pub contact_count: usize,
    pub conversation_count: usize,
    pub scanned_message_count: u64,
    pub selected_message_count: u64,
    pub evidence_count: u64,
    pub unit_count: usize,
    pub largest_unit_text_bytes: usize,
    pub unmatched_message_table_count: usize,
    pub files: Vec<CorpusFileRecord>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalMemoryCoverage {
    pub scanned_message_count: u64,
    pub eligible_message_count: u64,
    pub selected_message_count: u64,
    pub self_message_count: u64,
    pub other_message_count: u64,
    pub unknown_actor_message_count: u64,
    pub omitted_outside_time_range: u64,
    pub omitted_inactive_month: u64,
    pub omitted_silent_session: u64,
    pub omitted_context_bound: u64,
    pub omitted_filtered_conversation: u64,
    pub metadata_decode_failure_count: u64,
    pub content_decode_failure_count: u64,
    pub activity_month_count: u64,
    pub active_month_count: u64,
    pub episode_count: u64,
    pub unit_count: u64,
    pub source_coverage_complete: bool,
    pub content_complete: bool,
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContactSidecarRecord {
    alias: Option<String>,
    source_id: String,
    display_name: String,
    kind: ContactKind,
    is_account_holder: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivityRecord {
    conversation: String,
    conversation_id: String,
    label: String,
    kind: ConversationKind,
    month: String,
    message_count: usize,
    self_message_count: usize,
    selected_message_count: usize,
    active: bool,
    recent_conversation_eligible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EvidenceRecord {
    alias: String,
    canonical_id: String,
    conversation: String,
    conversation_id: String,
    sender: Option<String>,
    sender_id: Option<String>,
    actor: String,
    created_at_unix: i64,
    message_type: u32,
    message_subtype: u32,
    payload_kind: String,
    text: String,
    text_truncated: bool,
    content_decode_failed: bool,
    #[serde(rename = "contentSHA256")]
    content_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompactMessage {
    e: String,
    a: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    p: Option<String>,
    t: i64,
    k: String,
    x: String,
    #[serde(default, skip_serializing_if = "is_false")]
    tr: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PreparedUnitFile {
    schema: String,
    id: String,
    c: String,
    label: String,
    kind: ConversationKind,
    month: String,
    from: i64,
    to: i64,
    m: Vec<CompactMessage>,
}

#[derive(Debug, Clone, Serialize)]
struct DeliveryEpisode {
    /// Stable prepared-unit alias. A unit may continue on the next delivery page.
    u: String,
    c: String,
    label: String,
    kind: ConversationKind,
    month: String,
    from: i64,
    to: i64,
    /// Zero-based message offset within the immutable prepared unit.
    o: usize,
    /// Total message count in the immutable prepared unit.
    n: usize,
    m: Vec<CompactMessage>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeliveryPagePosition {
    number: usize,
    page_count: usize,
    message_count: usize,
    text_byte_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeliveryPagePayload {
    schema: &'static str,
    format_version: u32,
    batch_id: String,
    content_trust: &'static str,
    page: DeliveryPagePosition,
    target_pages: Vec<String>,
    people: BTreeMap<String, String>,
    episodes: Vec<DeliveryEpisode>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeliveryPageOutput {
    #[serde(flatten)]
    payload: DeliveryPagePayload,
    page_token: String,
    #[serde(rename = "pageSHA256")]
    page_sha256: String,
}

#[derive(Debug, Clone)]
struct RenderedDeliveryPage {
    output: DeliveryPageOutput,
    serialized: Vec<u8>,
    evidence_aliases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnitIndexEntry {
    id: String,
    relative_path: String,
    #[serde(rename = "sha256")]
    sha256: String,
    byte_count: u64,
    text_byte_count: usize,
    message_count: usize,
    target_pages: Vec<String>,
    evidence_aliases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnitIndex {
    schema: String,
    format_version: u32,
    units: Vec<UnitIndexEntry>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MonthKey {
    ordinal: i32,
    label: String,
}

#[derive(Debug, Clone)]
struct EpisodeDraft {
    conversation: CorpusConversation,
    month: String,
    locations: Vec<CorpusMessageLocation>,
}

#[derive(Debug, Clone)]
struct HydratedEpisode {
    conversation: CorpusConversation,
    month: String,
    messages: Vec<CorpusHydratedMessage>,
}

#[derive(Debug, Clone)]
struct UnitDraft {
    conversation_alias: String,
    conversation_source_id: String,
    conversation_label: String,
    conversation_kind: ConversationKind,
    month: String,
    messages: Vec<CorpusHydratedMessage>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalMemoryProgress {
    pub phase: &'static str,
    pub completed_items: usize,
    pub total_items: usize,
    pub scanned_message_count: u64,
    pub selected_message_count: u64,
    pub hydrated_message_count: u64,
}

pub fn prepare_personal_memory_corpus(
    source: &LiveQuerySource<'_>,
    selection_policy_path: &Path,
    output_directory: &Path,
) -> Result<PersonalMemoryCorpusManifest, RestoreError> {
    prepare_personal_memory_corpus_with_progress(
        source,
        selection_policy_path,
        output_directory,
        &mut |_| {},
    )
}

pub fn prepare_personal_memory_corpus_with_progress(
    source: &LiveQuerySource<'_>,
    selection_policy_path: &Path,
    output_directory: &Path,
    progress: &mut dyn FnMut(&PersonalMemoryProgress),
) -> Result<PersonalMemoryCorpusManifest, RestoreError> {
    if source.account_holder_source_id().is_none() {
        return Err(RestoreError::Integrity(
            "personal-memory preparation requires an authenticated live account-holder binding"
                .into(),
        ));
    }
    validate_new_corpus_output(output_directory, source.root())?;
    let policy_bytes = read_regular_file_limited(selection_policy_path, MAXIMUM_POLICY_BYTES)?;
    let policy: PersonalMemorySelectionPolicy = serde_json::from_slice(&policy_bytes)?;
    let timezone = policy.validate()?;
    let policy_sha256 = sha256_bytes(&policy_bytes);
    let reference_unix = policy.reference_unix.unwrap_or_else(now_unix_seconds);
    let reference_month = month_key(timezone, reference_unix).ok_or_else(|| {
        RestoreError::Integrity(
            "selection reference time is outside supported calendar bounds".into(),
        )
    })?;
    let recent_start_ordinal = if policy.recent_lookback_months == 0 {
        None
    } else {
        Some(
            reference_month
                .ordinal
                .saturating_sub(policy.recent_lookback_months.saturating_sub(1) as i32),
        )
    };

    let reader = LiveCorpusReader::open(source).map_err(corpus_query_error)?;
    let inventory = reader.inventory().clone();
    let mut coverage = PersonalMemoryCoverage {
        source_coverage_complete: inventory.coverage_complete,
        content_complete: true,
        ..Default::default()
    };
    let mut limitation_codes = inventory
        .warnings
        .iter()
        .map(|warning| warning.code.to_string())
        .collect::<BTreeSet<_>>();
    let mut activity = BTreeMap::<(String, String), ActivityRecord>::new();
    let mut episode_drafts = Vec::<EpisodeDraft>::new();
    let mut last_progress = Instant::now();
    progress(&PersonalMemoryProgress {
        phase: "metadataSelection",
        completed_items: 0,
        total_items: inventory.conversations.len(),
        scanned_message_count: 0,
        selected_message_count: 0,
        hydrated_message_count: 0,
    });

    for (conversation_index, conversation) in inventory.conversations.iter().enumerate() {
        let scan = reader
            .scan_metadata(conversation)
            .map_err(corpus_query_error)?;
        coverage.scanned_message_count = coverage
            .scanned_message_count
            .saturating_add(scan.messages.len() as u64);
        if !scan.coverage_complete {
            coverage.source_coverage_complete = false;
        }
        for warning in &scan.warnings {
            limitation_codes.insert(warning.code.to_string());
            if matches!(warning.code, "corpusMetadataRowFailed") {
                coverage.metadata_decode_failure_count = coverage
                    .metadata_decode_failure_count
                    .saturating_add(warning.count.unwrap_or(1) as u64);
            }
        }

        if !conversation_enabled(conversation, &policy) {
            coverage.omitted_filtered_conversation = coverage
                .omitted_filtered_conversation
                .saturating_add(scan.messages.len() as u64);
            if conversation_index.saturating_add(1) == inventory.conversations.len()
                || last_progress.elapsed() >= Duration::from_secs(2)
            {
                progress(&PersonalMemoryProgress {
                    phase: "metadataSelection",
                    completed_items: conversation_index.saturating_add(1),
                    total_items: inventory.conversations.len(),
                    scanned_message_count: coverage.scanned_message_count,
                    selected_message_count: coverage.selected_message_count,
                    hydrated_message_count: 0,
                });
                last_progress = Instant::now();
            }
            continue;
        }

        let mut messages = scan.messages;
        messages.sort_by(|left, right| {
            chronological_metadata_key(left).cmp(&chronological_metadata_key(right))
        });
        let mut month_indices = BTreeMap::<MonthKey, Vec<usize>>::new();
        for (index, message) in messages.iter().enumerate() {
            let _message_type_metadata = wx_db::split_local_type(message.local_type);
            if message.sender.is_some() != message.is_account_holder.is_some() {
                coverage.metadata_decode_failure_count =
                    coverage.metadata_decode_failure_count.saturating_add(1);
                coverage.source_coverage_complete = false;
                limitation_codes.insert("accountHolderMetadataInconsistent".into());
            }
            let timestamp = message.location.create_time;
            if policy
                .not_before_unix
                .is_some_and(|not_before| timestamp < not_before)
                || policy
                    .not_after_unix
                    .is_some_and(|not_after| timestamp > not_after)
            {
                coverage.omitted_outside_time_range =
                    coverage.omitted_outside_time_range.saturating_add(1);
                continue;
            }
            let Some(month) = month_key(timezone, timestamp) else {
                coverage.metadata_decode_failure_count =
                    coverage.metadata_decode_failure_count.saturating_add(1);
                coverage.source_coverage_complete = false;
                limitation_codes.insert("timestampOutOfCalendarRange".into());
                continue;
            };
            coverage.eligible_message_count = coverage.eligible_message_count.saturating_add(1);
            match message.is_account_holder {
                Some(true) => {
                    coverage.self_message_count = coverage.self_message_count.saturating_add(1)
                }
                Some(false) => {
                    coverage.other_message_count = coverage.other_message_count.saturating_add(1)
                }
                None => {
                    coverage.unknown_actor_message_count =
                        coverage.unknown_actor_message_count.saturating_add(1)
                }
            }
            month_indices.entry(month).or_default().push(index);
        }

        let active_months = month_indices
            .iter()
            .filter_map(|(month, indices)| {
                let self_count = indices
                    .iter()
                    .filter(|index| messages[**index].is_account_holder == Some(true))
                    .count();
                (self_count >= policy.minimum_self_messages_per_active_month)
                    .then_some(month.clone())
            })
            .collect::<BTreeSet<_>>();
        let recent_active_count = active_months
            .iter()
            .filter(|month| {
                recent_start_ordinal.is_some_and(|start| {
                    month.ordinal >= start && month.ordinal <= reference_month.ordinal
                })
            })
            .count();
        let recent_conversation_eligible = policy.minimum_self_active_months_in_lookback == 0
            || recent_active_count >= policy.minimum_self_active_months_in_lookback;

        for (month, indices) in &month_indices {
            let self_count = indices
                .iter()
                .filter(|index| messages[**index].is_account_holder == Some(true))
                .count();
            let active = active_months.contains(month) && recent_conversation_eligible;
            coverage.activity_month_count = coverage.activity_month_count.saturating_add(1);
            if active {
                coverage.active_month_count = coverage.active_month_count.saturating_add(1);
            } else {
                coverage.omitted_inactive_month = coverage
                    .omitted_inactive_month
                    .saturating_add(indices.len() as u64);
            }
            activity.insert(
                (conversation.source_id.clone(), month.label.clone()),
                ActivityRecord {
                    conversation: String::new(),
                    conversation_id: conversation.source_id.clone(),
                    label: conversation.display_name.clone(),
                    kind: conversation.kind,
                    month: month.label.clone(),
                    message_count: indices.len(),
                    self_message_count: self_count,
                    selected_message_count: 0,
                    active,
                    recent_conversation_eligible,
                },
            );
            if !active {
                continue;
            }
            let (gap_seconds, context_before, context_after) =
                if conversation.kind == ConversationKind::Group {
                    (
                        policy.group_session_gap_minutes.saturating_mul(60),
                        policy.group_context_before,
                        policy.group_context_after,
                    )
                } else {
                    (
                        policy.direct_session_gap_minutes.saturating_mul(60),
                        policy.direct_context_before,
                        policy.direct_context_after,
                    )
                };
            let sessions = split_sessions(indices, &messages, gap_seconds);
            let mut selected_in_month = BTreeSet::<usize>::new();
            for session in sessions {
                let anchors = session
                    .iter()
                    .enumerate()
                    .filter_map(|(position, index)| {
                        (messages[*index].is_account_holder == Some(true)).then_some(position)
                    })
                    .collect::<Vec<_>>();
                if anchors.is_empty() {
                    coverage.omitted_silent_session = coverage
                        .omitted_silent_session
                        .saturating_add(session.len() as u64);
                    continue;
                }
                for (start, end) in
                    merged_context_windows(&anchors, session.len(), context_before, context_after)
                {
                    let locations = session[start..=end]
                        .iter()
                        .map(|index| {
                            selected_in_month.insert(*index);
                            messages[*index].location.clone()
                        })
                        .collect::<Vec<_>>();
                    episode_drafts.push(EpisodeDraft {
                        conversation: conversation.clone(),
                        month: month.label.clone(),
                        locations,
                    });
                }
                let selected_in_session = session
                    .iter()
                    .filter(|index| selected_in_month.contains(index))
                    .count();
                coverage.omitted_context_bound = coverage
                    .omitted_context_bound
                    .saturating_add(session.len().saturating_sub(selected_in_session) as u64);
            }
            if let Some(record) =
                activity.get_mut(&(conversation.source_id.clone(), month.label.clone()))
            {
                record.selected_message_count = selected_in_month.len();
            }
            coverage.selected_message_count = coverage
                .selected_message_count
                .saturating_add(selected_in_month.len() as u64);
        }
        if conversation_index.saturating_add(1) == inventory.conversations.len()
            || last_progress.elapsed() >= Duration::from_secs(2)
        {
            progress(&PersonalMemoryProgress {
                phase: "metadataSelection",
                completed_items: conversation_index.saturating_add(1),
                total_items: inventory.conversations.len(),
                scanned_message_count: coverage.scanned_message_count,
                selected_message_count: coverage.selected_message_count,
                hydrated_message_count: 0,
            });
            last_progress = Instant::now();
        }
    }

    let mut episode_groups = BTreeMap::<String, Vec<EpisodeDraft>>::new();
    for episode in episode_drafts {
        episode_groups
            .entry(episode.conversation.source_id.clone())
            .or_default()
            .push(episode);
    }
    let mut hydrated_episodes = Vec::<HydratedEpisode>::new();
    let hydration_group_count = episode_groups.len();
    let mut hydrated_message_count = 0_u64;
    last_progress = Instant::now();
    progress(&PersonalMemoryProgress {
        phase: "selectedContentHydration",
        completed_items: 0,
        total_items: hydration_group_count,
        scanned_message_count: coverage.scanned_message_count,
        selected_message_count: coverage.selected_message_count,
        hydrated_message_count: 0,
    });
    for (hydration_index, (conversation_id, episodes)) in episode_groups.into_iter().enumerate() {
        let conversation = episodes
            .first()
            .map(|episode| episode.conversation.clone())
            .ok_or_else(|| RestoreError::Integrity("empty episode group was produced".into()))?;
        let selected = episodes
            .iter()
            .flat_map(|episode| episode.locations.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let hydration = reader
            .hydrate(&conversation, &selected, policy.maximum_message_text_bytes)
            .map_err(corpus_query_error)?;
        if !hydration.coverage_complete {
            coverage.content_complete = false;
        }
        for warning in &hydration.warnings {
            limitation_codes.insert(warning.code.to_string());
        }
        coverage.content_decode_failure_count =
            coverage.content_decode_failure_count.saturating_add(
                hydration
                    .messages
                    .iter()
                    .filter(|message| message.content_decode_failed)
                    .count() as u64,
            );
        if hydration
            .messages
            .iter()
            .any(|message| message.content_decode_failed)
        {
            coverage.content_complete = false;
        }
        let by_location = hydration
            .messages
            .into_iter()
            .map(|message| (message.location.clone(), message))
            .collect::<BTreeMap<_, _>>();
        hydrated_message_count = hydrated_message_count.saturating_add(by_location.len() as u64);
        for episode in episodes {
            let messages = episode
                .locations
                .iter()
                .filter_map(|location| by_location.get(location).cloned())
                .collect::<Vec<_>>();
            if messages.len() != episode.locations.len() {
                coverage.content_complete = false;
                limitation_codes.insert("selectedMessageHydrationIncomplete".into());
            }
            if !messages.is_empty() {
                hydrated_episodes.push(HydratedEpisode {
                    conversation: episode.conversation,
                    month: episode.month,
                    messages,
                });
            }
        }
        let _ = conversation_id;
        if hydration_index.saturating_add(1) == hydration_group_count
            || last_progress.elapsed() >= Duration::from_secs(2)
        {
            progress(&PersonalMemoryProgress {
                phase: "selectedContentHydration",
                completed_items: hydration_index.saturating_add(1),
                total_items: hydration_group_count,
                scanned_message_count: coverage.scanned_message_count,
                selected_message_count: coverage.selected_message_count,
                hydrated_message_count,
            });
            last_progress = Instant::now();
        }
    }
    hydrated_episodes
        .sort_by(|left, right| hydrated_episode_key(left).cmp(&hydrated_episode_key(right)));

    let conversation_aliases = inventory
        .conversations
        .iter()
        .enumerate()
        .map(|(index, conversation)| {
            (
                conversation.source_id.clone(),
                format!("C{:06}", index.saturating_add(1)),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for record in activity.values_mut() {
        record.conversation = conversation_aliases
            .get(&record.conversation_id)
            .cloned()
            .ok_or_else(|| {
                RestoreError::Integrity("activity conversation alias is unavailable".into())
            })?;
    }

    let mut person_names = inventory
        .contacts
        .iter()
        .filter(|contact| !contact.is_account_holder)
        .map(|contact| (contact.source_id.clone(), contact.display_name.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut person_ids = BTreeSet::<String>::new();
    for episode in &hydrated_episodes {
        if episode.conversation.kind == ConversationKind::Direct
            && source.account_holder_source_id() != Some(episode.conversation.source_id.as_str())
        {
            person_ids.insert(episode.conversation.source_id.clone());
        }
        for message in &episode.messages {
            if message.is_account_holder != Some(true) {
                if let Some(sender) = &message.sender {
                    person_ids.insert(sender.clone());
                    if let Some(name) = &message.sender_display_name {
                        person_names.entry(sender.clone()).or_insert(name.clone());
                    }
                }
            }
        }
    }
    let person_aliases = person_ids
        .into_iter()
        .enumerate()
        .map(|(index, source_id)| (source_id, format!("P{:06}", index.saturating_add(1))))
        .collect::<BTreeMap<_, _>>();

    let hydrated_episode_count = hydrated_episodes.len() as u64;
    let mut units = Vec::<UnitDraft>::new();
    for episode in hydrated_episodes {
        let conversation_alias = conversation_aliases
            .get(&episode.conversation.source_id)
            .cloned()
            .ok_or_else(|| {
                RestoreError::Integrity("episode conversation alias is unavailable".into())
            })?;
        let mut current = Vec::new();
        let mut current_text_bytes = 0usize;
        for message in episode.messages {
            let text_bytes = compact_message_text(&message).len();
            let would_overflow = !current.is_empty()
                && (current.len() >= policy.maximum_unit_messages
                    || current_text_bytes.saturating_add(text_bytes)
                        > policy.maximum_unit_text_bytes);
            if would_overflow {
                units.push(UnitDraft {
                    conversation_alias: conversation_alias.clone(),
                    conversation_source_id: episode.conversation.source_id.clone(),
                    conversation_label: model_safe_conversation_label(
                        &episode.conversation,
                        &conversation_alias,
                    ),
                    conversation_kind: episode.conversation.kind,
                    month: episode.month.clone(),
                    messages: std::mem::take(&mut current),
                });
                current_text_bytes = 0;
            }
            current_text_bytes = current_text_bytes.saturating_add(text_bytes);
            current.push(message);
        }
        if !current.is_empty() {
            units.push(UnitDraft {
                conversation_label: model_safe_conversation_label(
                    &episode.conversation,
                    &conversation_alias,
                ),
                conversation_alias,
                conversation_source_id: episode.conversation.source_id,
                conversation_kind: episode.conversation.kind,
                month: episode.month,
                messages: current,
            });
        }
    }
    units = order_unit_drafts(units, policy.delivery_order);
    coverage.episode_count = hydrated_episode_count;

    let parent = output_directory.parent().unwrap_or_else(|| Path::new("."));
    progress(&PersonalMemoryProgress {
        phase: "atomicPublication",
        completed_items: 0,
        total_items: units.len(),
        scanned_message_count: coverage.scanned_message_count,
        selected_message_count: coverage.selected_message_count,
        hydrated_message_count,
    });
    let staging = tempfile::Builder::new()
        .prefix(".greenbubbles-personal-memory-")
        .tempdir_in(parent)?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))?;
    let batches_directory = staging.path().join("batches");
    fs::create_dir(&batches_directory)?;
    fs::set_permissions(&batches_directory, fs::Permissions::from_mode(0o700))?;

    let mut files = Vec::<CorpusFileRecord>::new();
    let contacts_path = staging.path().join("contacts.jsonl");
    let mut contact_records = inventory
        .contacts
        .iter()
        .map(|contact| ContactSidecarRecord {
            alias: person_aliases.get(&contact.source_id).cloned(),
            source_id: contact.source_id.clone(),
            display_name: contact.display_name.clone(),
            kind: contact.kind,
            is_account_holder: contact.is_account_holder,
        })
        .collect::<Vec<_>>();
    for (source_id, alias) in &person_aliases {
        if !contact_records
            .iter()
            .any(|record| &record.source_id == source_id)
        {
            contact_records.push(ContactSidecarRecord {
                alias: Some(alias.clone()),
                source_id: source_id.clone(),
                display_name: person_names
                    .get(source_id)
                    .cloned()
                    .unwrap_or_else(|| source_id.clone()),
                kind: ContactKind::Unknown,
                is_account_holder: false,
            });
        }
    }
    contact_records.sort_by(|left, right| left.source_id.cmp(&right.source_id));
    files.push(write_json_lines(
        &contacts_path,
        "contacts.jsonl",
        contact_records.iter(),
    )?);

    let activity_path = staging.path().join("activity.jsonl");
    files.push(write_json_lines(
        &activity_path,
        "activity.jsonl",
        activity.values(),
    )?);

    let evidence_path = staging.path().join("evidence.jsonl");
    let mut evidence_writer = owner_only_writer(&evidence_path)?;
    let mut evidence_hasher = Sha256::new();
    let mut evidence_byte_count = 0_u64;
    let mut evidence_count = 0_u64;
    let mut unit_index_entries = Vec::<UnitIndexEntry>::new();
    let mut largest_unit_text_bytes = 0usize;
    let publication_unit_count = units.len();
    last_progress = Instant::now();
    for (unit_index, unit) in units.into_iter().enumerate() {
        let unit_id = format!("U{:06}", unit_index.saturating_add(1));
        let mut compact_messages = Vec::with_capacity(unit.messages.len());
        let mut evidence_aliases = Vec::with_capacity(unit.messages.len());
        let mut target_pages = BTreeSet::from(["index.md".to_string(), "me.md".to_string()]);
        if let Some(person_alias) = person_aliases.get(&unit.conversation_source_id) {
            target_pages.insert(format!("people/{person_alias}.md"));
        }
        let mut text_byte_count = 0usize;
        for message in unit.messages {
            evidence_count = evidence_count.saturating_add(1);
            let evidence_alias = format!("E{evidence_count:09}");
            let actor = match message.is_account_holder {
                Some(true) => "self",
                Some(false) => "other",
                None => "unknown",
            }
            .to_string();
            let person_alias = message
                .sender
                .as_ref()
                .and_then(|sender| person_aliases.get(sender).cloned());
            if let Some(person_alias) = &person_alias {
                target_pages.insert(format!("people/{person_alias}.md"));
            }
            let text = compact_message_text(&message);
            text_byte_count = text_byte_count.saturating_add(text.len());
            let evidence = EvidenceRecord {
                alias: evidence_alias.clone(),
                canonical_id: message.canonical_id,
                conversation: unit.conversation_alias.clone(),
                conversation_id: unit.conversation_source_id.clone(),
                sender: person_alias.clone(),
                sender_id: message.sender,
                actor: actor.clone(),
                created_at_unix: message.location.create_time,
                message_type: message.message_type,
                message_subtype: message.message_subtype,
                payload_kind: message.payload_kind.clone(),
                text: text.clone(),
                text_truncated: message.text_truncated,
                content_decode_failed: message.content_decode_failed,
                content_sha256: sha256_bytes(text.as_bytes()),
            };
            write_hashed_json_line(
                &mut evidence_writer,
                &mut evidence_hasher,
                &mut evidence_byte_count,
                &evidence,
            )?;
            compact_messages.push(CompactMessage {
                e: evidence_alias.clone(),
                a: actor,
                p: person_alias,
                t: message.location.create_time,
                k: message.payload_kind,
                x: text,
                tr: message.text_truncated,
            });
            evidence_aliases.push(evidence_alias);
        }
        let from = compact_messages
            .first()
            .map(|message| message.t)
            .unwrap_or_default();
        let to = compact_messages
            .last()
            .map(|message| message.t)
            .unwrap_or_default();
        let prepared = PreparedUnitFile {
            schema: PERSONAL_MEMORY_BATCH_SCHEMA.to_string(),
            id: unit_id.clone(),
            c: unit.conversation_alias,
            label: unit.conversation_label,
            kind: unit.conversation_kind,
            month: unit.month,
            from,
            to,
            m: compact_messages,
        };
        let relative_path = format!("batches/{unit_id}.json");
        let record = write_json_pretty(
            &staging.path().join(&relative_path),
            &relative_path,
            &prepared,
        )?;
        largest_unit_text_bytes = largest_unit_text_bytes.max(text_byte_count);
        unit_index_entries.push(UnitIndexEntry {
            id: unit_id,
            relative_path,
            sha256: record.sha256.clone(),
            byte_count: record.byte_count,
            text_byte_count,
            message_count: evidence_aliases.len(),
            target_pages: target_pages.into_iter().collect(),
            evidence_aliases,
        });
        if unit_index.saturating_add(1) == publication_unit_count
            || last_progress.elapsed() >= Duration::from_secs(2)
        {
            progress(&PersonalMemoryProgress {
                phase: "atomicPublication",
                completed_items: unit_index.saturating_add(1),
                total_items: publication_unit_count,
                scanned_message_count: coverage.scanned_message_count,
                selected_message_count: coverage.selected_message_count,
                hydrated_message_count,
            });
            last_progress = Instant::now();
        }
    }
    evidence_writer.flush()?;
    evidence_writer.get_ref().sync_all()?;
    files.push(CorpusFileRecord {
        relative_path: "evidence.jsonl".into(),
        byte_count: evidence_byte_count,
        sha256: hex::encode(evidence_hasher.finalize()),
    });

    let unit_index = UnitIndex {
        schema: "greenbubbles.personal-memory-unit-index.v1".into(),
        format_version: PERSONAL_MEMORY_FORMAT_VERSION,
        units: unit_index_entries,
    };
    files.push(write_json_pretty(
        &batches_directory.join("index.json"),
        "batches/index.json",
        &unit_index,
    )?);
    coverage.unit_count = unit_index.units.len() as u64;
    coverage.limitation_codes = limitation_codes.into_iter().collect();
    let coverage_record = write_json_pretty(
        &staging.path().join("coverage.json"),
        "coverage.json",
        &coverage,
    )?;
    files.push(coverage_record);

    let manifest = PersonalMemoryCorpusManifest {
        schema: PERSONAL_MEMORY_CORPUS_SCHEMA.to_string(),
        format_version: PERSONAL_MEMORY_FORMAT_VERSION,
        generated_at_unix_milliseconds: now_unix_milliseconds()?,
        source_mode: source.mode(),
        source_identity: source.identity().to_string(),
        selection_policy_sha256: policy_sha256,
        timezone: policy.timezone,
        delivery_order: policy.delivery_order,
        reference_unix,
        account_holder_attribution_bound: true,
        content_trust: "untrustedChatEvidence".into(),
        immutable_index: true,
        source_coverage_complete: coverage.source_coverage_complete,
        content_complete: coverage.content_complete,
        contact_count: contact_records.len(),
        conversation_count: inventory.conversations.len(),
        scanned_message_count: coverage.scanned_message_count,
        selected_message_count: coverage.selected_message_count,
        evidence_count,
        unit_count: unit_index.units.len(),
        largest_unit_text_bytes,
        unmatched_message_table_count: inventory.unmatched_message_table_count,
        files,
    };
    write_json_pretty(
        &staging.path().join("manifest.json"),
        "manifest.json",
        &manifest,
    )?;
    File::open(&batches_directory)?.sync_all()?;
    File::open(staging.path())?.sync_all()?;
    protect_immutable_corpus_tree(staging.path())?;
    fs::rename(staging.path(), output_directory)?;
    File::open(parent)?.sync_all()?;
    progress(&PersonalMemoryProgress {
        phase: "complete",
        completed_items: manifest.unit_count,
        total_items: manifest.unit_count,
        scanned_message_count: manifest.scanned_message_count,
        selected_message_count: manifest.selected_message_count,
        hydrated_message_count: manifest.evidence_count,
    });
    Ok(manifest)
}

fn conversation_enabled(
    conversation: &CorpusConversation,
    policy: &PersonalMemorySelectionPolicy,
) -> bool {
    match conversation.contact_kind {
        ContactKind::Group => policy.include_group_conversations,
        ContactKind::Official => policy.include_official_accounts,
        ContactKind::Service | ContactKind::AccountHolder => policy.include_service_accounts,
        ContactKind::Person | ContactKind::Unknown => policy.include_direct_conversations,
    }
}

fn model_safe_conversation_label(conversation: &CorpusConversation, alias: &str) -> String {
    let display_name = conversation.display_name.trim();
    if !display_name.is_empty() && display_name != conversation.source_id.trim() {
        return display_name.to_string();
    }

    match conversation.kind {
        ConversationKind::Group => format!("Group {alias}"),
        ConversationKind::Direct => format!("Direct conversation {alias}"),
        ConversationKind::Business => format!("Official account {alias}"),
        ConversationKind::Chatbot => format!("Chatbot {alias}"),
        ConversationKind::System => format!("System conversation {alias}"),
        ConversationKind::Unresolved => format!("Conversation {alias}"),
    }
}

fn model_safe_person_label(record: &ContactSidecarRecord, alias: &str) -> String {
    let display_name = record.display_name.trim();
    if !display_name.is_empty() && display_name != record.source_id.trim() {
        return display_name.to_string();
    }
    format!("Person {alias}")
}

fn chronological_metadata_key(message: &CorpusMessageMetadata) -> (i64, i64, i64, u32, i64) {
    (
        message.location.create_time,
        message.location.sort_sequence,
        message.location.server_id,
        message.location.shard_id,
        message.location.row_id,
    )
}

fn hydrated_episode_key(episode: &HydratedEpisode) -> (i64, &str, &str) {
    (
        episode
            .messages
            .first()
            .map(|message| message.location.create_time)
            .unwrap_or_default(),
        episode.conversation.source_id.as_str(),
        episode.month.as_str(),
    )
}

fn unit_draft_key(unit: &UnitDraft) -> (i64, &str, &str) {
    (
        unit.messages
            .first()
            .map(|message| message.location.create_time)
            .unwrap_or_default(),
        unit.conversation_alias.as_str(),
        unit.month.as_str(),
    )
}

fn conversation_kind_priority(kind: ConversationKind) -> u8 {
    match kind {
        ConversationKind::Direct => 0,
        ConversationKind::Group => 1,
        ConversationKind::Business => 2,
        ConversationKind::Chatbot => 3,
        ConversationKind::System => 4,
        ConversationKind::Unresolved => 5,
    }
}

fn unit_self_message_count(unit: &UnitDraft) -> usize {
    unit.messages
        .iter()
        .filter(|message| message.is_account_holder == Some(true))
        .count()
}

fn unit_last_timestamp(unit: &UnitDraft) -> i64 {
    unit.messages
        .last()
        .map(|message| message.location.create_time)
        .unwrap_or_default()
}

#[derive(Debug)]
struct RelevanceScheduledUnit {
    unit: UnitDraft,
    rank_within_conversation: usize,
    conversation_weight: usize,
    conversation_self_message_count: usize,
    conversation_active_month_count: usize,
    conversation_last_timestamp: i64,
    conversation_kind: ConversationKind,
    conversation_alias: String,
}

fn order_unit_drafts(
    mut units: Vec<UnitDraft>,
    delivery_order: MemoryDeliveryOrder,
) -> Vec<UnitDraft> {
    units.sort_by(|left, right| unit_draft_key(left).cmp(&unit_draft_key(right)));
    if delivery_order == MemoryDeliveryOrder::Chronological {
        return units;
    }

    let mut conversations = BTreeMap::<String, Vec<UnitDraft>>::new();
    for unit in units {
        conversations
            .entry(unit.conversation_alias.clone())
            .or_default()
            .push(unit);
    }

    let mut scheduled = Vec::<RelevanceScheduledUnit>::new();
    for (conversation_alias, conversation_units) in conversations {
        let conversation_self_message_count = conversation_units
            .iter()
            .map(unit_self_message_count)
            .sum::<usize>();
        let conversation_active_month_count = conversation_units
            .iter()
            .map(|unit| unit.month.as_str())
            .collect::<BTreeSet<_>>()
            .len();
        let conversation_last_timestamp = conversation_units
            .iter()
            .map(unit_last_timestamp)
            .max()
            .unwrap_or_default();
        let conversation_kind = conversation_units
            .first()
            .map(|unit| unit.conversation_kind)
            .unwrap_or(ConversationKind::Unresolved);
        // Logarithmic weighting gives a highly self-active conversation several
        // representative units before a one-off conversation, without allowing
        // any single conversation to monopolize the frontier. Every remaining
        // unit still receives a finite virtual finish time and is scheduled.
        let conversation_weight =
            ((usize::BITS - conversation_self_message_count.max(1).leading_zeros()) as usize)
                .clamp(1, 16);
        let mut units_by_month = BTreeMap::<String, Vec<UnitDraft>>::new();
        for unit in conversation_units {
            units_by_month
                .entry(unit.month.clone())
                .or_default()
                .push(unit);
        }
        let mut month_scheduled_units = Vec::<(usize, usize, i64, String, UnitDraft)>::new();
        for (month, mut month_units) in units_by_month {
            let month_self_message_count = month_units
                .iter()
                .map(unit_self_message_count)
                .sum::<usize>();
            let month_last_timestamp = month_units
                .iter()
                .map(unit_last_timestamp)
                .max()
                .unwrap_or_default();
            month_units.sort_by(|left, right| {
                unit_self_message_count(right)
                    .cmp(&unit_self_message_count(left))
                    .then_with(|| right.messages.len().cmp(&left.messages.len()))
                    .then_with(|| unit_last_timestamp(right).cmp(&unit_last_timestamp(left)))
                    .then_with(|| unit_draft_key(left).cmp(&unit_draft_key(right)))
            });
            for (rank_within_month, unit) in month_units.into_iter().enumerate() {
                month_scheduled_units.push((
                    rank_within_month,
                    month_self_message_count,
                    month_last_timestamp,
                    month.clone(),
                    unit,
                ));
            }
        }
        // Cover active periods within a relationship before repeatedly drawing
        // from one dense month. Stronger and more recent months break ties.
        month_scheduled_units.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| right.2.cmp(&left.2))
                .then_with(|| left.3.cmp(&right.3))
                .then_with(|| unit_draft_key(&left.4).cmp(&unit_draft_key(&right.4)))
        });
        for (rank_within_conversation, (_, _, _, _, unit)) in
            month_scheduled_units.into_iter().enumerate()
        {
            scheduled.push(RelevanceScheduledUnit {
                unit,
                rank_within_conversation,
                conversation_weight,
                conversation_self_message_count,
                conversation_active_month_count,
                conversation_last_timestamp,
                conversation_kind,
                conversation_alias: conversation_alias.clone(),
            });
        }
    }

    scheduled.sort_by(|left, right| {
        // Compare (rank + 1) / weight without floating point so the order is
        // byte-for-byte stable across platforms and builds.
        left.rank_within_conversation
            .saturating_add(1)
            .saturating_mul(right.conversation_weight)
            .cmp(
                &right
                    .rank_within_conversation
                    .saturating_add(1)
                    .saturating_mul(left.conversation_weight),
            )
            .then_with(|| {
                right
                    .conversation_self_message_count
                    .cmp(&left.conversation_self_message_count)
            })
            .then_with(|| {
                right
                    .conversation_active_month_count
                    .cmp(&left.conversation_active_month_count)
            })
            .then_with(|| {
                conversation_kind_priority(left.conversation_kind)
                    .cmp(&conversation_kind_priority(right.conversation_kind))
            })
            .then_with(|| {
                right
                    .conversation_last_timestamp
                    .cmp(&left.conversation_last_timestamp)
            })
            .then_with(|| left.conversation_alias.cmp(&right.conversation_alias))
            .then_with(|| {
                left.rank_within_conversation
                    .cmp(&right.rank_within_conversation)
            })
            .then_with(|| unit_draft_key(&left.unit).cmp(&unit_draft_key(&right.unit)))
    });
    scheduled.into_iter().map(|entry| entry.unit).collect()
}

fn month_key(timezone: Tz, timestamp: i64) -> Option<MonthKey> {
    let date = timezone.timestamp_opt(timestamp, 0).single()?;
    let month = date.month() as i32;
    Some(MonthKey {
        ordinal: date.year().saturating_mul(12).saturating_add(month - 1),
        label: format!("{:04}-{month:02}", date.year()),
    })
}

fn split_sessions(
    indices: &[usize],
    messages: &[CorpusMessageMetadata],
    maximum_gap_seconds: i64,
) -> Vec<Vec<usize>> {
    let mut sessions = Vec::<Vec<usize>>::new();
    for index in indices {
        let starts_new = sessions
            .last()
            .and_then(|session| session.last())
            .is_some_and(|previous| {
                messages[*index]
                    .location
                    .create_time
                    .saturating_sub(messages[*previous].location.create_time)
                    > maximum_gap_seconds
            });
        if starts_new || sessions.is_empty() {
            sessions.push(Vec::new());
        }
        if let Some(session) = sessions.last_mut() {
            session.push(*index);
        }
    }
    sessions
}

fn merged_context_windows(
    anchors: &[usize],
    message_count: usize,
    before: usize,
    after: usize,
) -> Vec<(usize, usize)> {
    if message_count == 0 {
        return Vec::new();
    }
    let mut windows = anchors
        .iter()
        .map(|anchor| {
            (
                anchor.saturating_sub(before),
                anchor.saturating_add(after).min(message_count - 1),
            )
        })
        .collect::<Vec<_>>();
    windows.sort_unstable();
    let mut merged = Vec::<(usize, usize)>::new();
    for (start, end) in windows {
        if let Some(previous) = merged.last_mut() {
            if start <= previous.1.saturating_add(1) {
                previous.1 = previous.1.max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    merged
}

fn compact_message_text(message: &CorpusHydratedMessage) -> String {
    message.text.clone().unwrap_or_else(|| {
        if message.content_decode_failed {
            "[content unavailable]".to_string()
        } else {
            format!("[{}]", message.payload_kind)
        }
    })
}

fn validate_new_corpus_output(output: &Path, source_root: &Path) -> Result<(), RestoreError> {
    if output.try_exists()? {
        return Err(RestoreError::Integrity(
            "personal-memory corpus output directory already exists".into(),
        ));
    }
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    ensure_private_directory(parent)?;
    let canonical_parent = fs::canonicalize(parent)?;
    let name = output.file_name().ok_or_else(|| {
        RestoreError::UnsafePath("personal-memory output has no final path component".into())
    })?;
    let final_output = canonical_parent.join(name);
    let canonical_source = fs::canonicalize(source_root)?;
    if final_output.starts_with(&canonical_source) {
        return Err(RestoreError::UnsafePath(
            "personal-memory corpus output must be outside the selected database source".into(),
        ));
    }
    Ok(())
}

fn read_regular_file_limited(path: &Path, maximum_bytes: u64) -> Result<Vec<u8>, RestoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
    {
        return Err(RestoreError::Integrity(
            "selection policy must be a current-user-owned, non-symlink regular file".into(),
        ));
    }
    if metadata.len() > maximum_bytes {
        return Err(RestoreError::Integrity(format!(
            "input exceeds the fixed {maximum_bytes}-byte safety limit"
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum_bytes {
        return Err(RestoreError::Integrity(format!(
            "input exceeds the fixed {maximum_bytes}-byte safety limit"
        )));
    }
    Ok(bytes)
}

fn owner_only_writer(path: &Path) -> Result<BufWriter<File>, RestoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath("private output has no parent".into()))?;
    ensure_private_directory(parent)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    Ok(BufWriter::new(file))
}

fn write_json_pretty<T: Serialize>(
    path: &Path,
    relative_path: &str,
    value: &T,
) -> Result<CorpusFileRecord, RestoreError> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    let mut writer = owner_only_writer(path)?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(CorpusFileRecord {
        relative_path: relative_path.to_string(),
        byte_count: bytes.len() as u64,
        sha256: sha256_bytes(&bytes),
    })
}

fn write_json_lines<'record, T: Serialize + 'record>(
    path: &Path,
    relative_path: &str,
    values: impl IntoIterator<Item = &'record T>,
) -> Result<CorpusFileRecord, RestoreError> {
    let mut writer = owner_only_writer(path)?;
    let mut hasher = Sha256::new();
    let mut byte_count = 0_u64;
    for value in values {
        write_hashed_json_line(&mut writer, &mut hasher, &mut byte_count, value)?;
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(CorpusFileRecord {
        relative_path: relative_path.to_string(),
        byte_count,
        sha256: hex::encode(hasher.finalize()),
    })
}

fn write_hashed_json_line<T: Serialize>(
    writer: &mut BufWriter<File>,
    hasher: &mut Sha256,
    byte_count: &mut u64,
    value: &T,
) -> Result<(), RestoreError> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    hasher.update(&bytes);
    *byte_count = byte_count.saturating_add(bytes.len() as u64);
    Ok(())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn sha256_file(path: &Path) -> Result<String, RestoreError> {
    ensure_private_regular_file(path)?;
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}

fn now_unix_milliseconds() -> Result<u64, RestoreError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RestoreError::Integrity("system clock predates the Unix epoch".into()))?
        .as_millis()
        .try_into()
        .map_err(|_| RestoreError::Integrity("system time does not fit in u64".into()))
}

fn corpus_query_error(error: LiveQueryError) -> RestoreError {
    RestoreError::Integrity(format!(
        "personal-memory read-only source scan failed safely: {error}"
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WikiFileSnapshot {
    #[serde(rename = "sha256")]
    sha256: String,
    citations: BTreeSet<String>,
    has_prose: bool,
    #[serde(default)]
    uncited_prose_line_count: usize,
    /// Derived validation metadata. Legacy state omitted it; byte-equivalence
    /// checks intentionally compare hashes rather than derived fields.
    #[serde(default)]
    excessive_citation_prose_line_count: usize,
    /// Needed only while validating the current in-memory scan. Omitting this
    /// duplicate derived data keeps persisted run state compact.
    #[serde(skip)]
    prose_line_citations: Vec<BTreeSet<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PageReviewDisposition {
    DurableEvidenceRetained,
    ReviewedNoDurableMemory,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageReviewRecord {
    disposition: PageReviewDisposition,
    retained_evidence_aliases: Vec<String>,
    acknowledged_at_unix_milliseconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OutstandingDeliveryPage {
    number: usize,
    page_token: String,
    #[serde(rename = "pageSHA256")]
    page_sha256: String,
    message_count: usize,
    text_byte_count: usize,
    evidence_aliases: Vec<String>,
    #[serde(default)]
    delivery_count: u64,
    #[serde(default)]
    first_delivered_at_unix_milliseconds: Option<u64>,
    #[serde(default)]
    last_delivered_at_unix_milliseconds: Option<u64>,
    #[serde(default)]
    review: Option<PageReviewRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OutstandingBatch {
    batch_id: String,
    start_unit_index: usize,
    end_unit_index_exclusive: usize,
    text_byte_count: usize,
    message_count: usize,
    target_pages: Vec<String>,
    evidence_aliases: Vec<String>,
    wiki_before: Option<BTreeMap<String, WikiFileSnapshot>>,
    /// Empty only while migrating an outstanding state written by an earlier
    /// build. The next protocol operation deterministically reconstructs it.
    #[serde(default)]
    delivery_pages: Vec<OutstandingDeliveryPage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommittedBatch {
    batch_id: String,
    committed_at_unix_milliseconds: u64,
    #[serde(default)]
    disposition: MemoryCommitDisposition,
    #[serde(default)]
    reviewed_page_count: usize,
    #[serde(default)]
    reviewed_message_count: usize,
    #[serde(default)]
    retained_evidence_count: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MemoryCommitDisposition {
    #[default]
    WikiUpdated,
    ReviewedNoDurableMemory,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MemoryRunState {
    schema: String,
    format_version: u32,
    #[serde(rename = "corpusManifestSHA256")]
    corpus_manifest_sha256: String,
    next_unit_index: usize,
    outstanding: Option<OutstandingBatch>,
    last_committed: Option<CommittedBatch>,
    committed_wiki: Option<BTreeMap<String, WikiFileSnapshot>>,
    created_at_unix_milliseconds: u64,
    updated_at_unix_milliseconds: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchPosition {
    first_unit: usize,
    unit_count: usize,
    total_units: usize,
    message_count: usize,
    text_byte_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryBatchOutput {
    schema: &'static str,
    format_version: u32,
    delivery_order: MemoryDeliveryOrder,
    batch_id: String,
    complete: bool,
    content_trust: &'static str,
    position: BatchPosition,
    delivery: MemoryBatchDelivery,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryBatchDelivery {
    page_count: usize,
    acknowledged_page_count: usize,
    delivered_message_count: usize,
    acknowledged_message_count: usize,
    retained_evidence_count: usize,
    maximum_page_output_bytes: usize,
    review_complete: bool,
    next_page_number: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryPageAcknowledgement {
    pub schema: &'static str,
    pub format_version: u32,
    pub batch_id: String,
    pub page_token: String,
    pub acknowledged: bool,
    pub already_acknowledged: bool,
    pub disposition: PageReviewDisposition,
    pub page_count: usize,
    pub acknowledged_page_count: usize,
    pub acknowledged_message_count: usize,
    pub retained_evidence_count: usize,
    pub review_complete: bool,
    pub next_page_number: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryCommitResult {
    pub schema: &'static str,
    pub format_version: u32,
    pub batch_id: String,
    pub committed: bool,
    pub already_committed: bool,
    pub disposition: MemoryCommitDisposition,
    pub next_unit_index: usize,
    pub total_units: usize,
    pub complete: bool,
    pub changed_pages: Vec<String>,
    pub reviewed_page_count: usize,
    pub reviewed_message_count: usize,
    pub retained_evidence_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryLastCommittedStatus {
    pub batch_id: String,
    pub disposition: MemoryCommitDisposition,
    pub reviewed_page_count: usize,
    pub reviewed_message_count: usize,
    pub retained_evidence_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStatus {
    pub schema: &'static str,
    pub format_version: u32,
    pub corpus_manifest_valid: bool,
    pub state_present: bool,
    pub unit_count: usize,
    pub evidence_count: u64,
    pub scanned_message_count: u64,
    pub selected_message_count: u64,
    pub source_coverage_complete: bool,
    pub content_complete: bool,
    pub unmatched_message_table_count: usize,
    pub limitation_codes: Vec<String>,
    pub delivery_order: MemoryDeliveryOrder,
    pub next_unit_index: usize,
    pub committed_unit_count: usize,
    pub committed_message_count: u64,
    pub outstanding_batch_id: Option<String>,
    pub outstanding_unit_count: usize,
    pub outstanding_page_count: usize,
    pub delivered_page_count: usize,
    pub acknowledged_page_count: usize,
    pub acknowledged_message_count: usize,
    pub retained_evidence_count: usize,
    /// Present only while a batch is outstanding. `None` distinguishes "there
    /// is no review in progress" from an incomplete outstanding review.
    pub review_complete: Option<bool>,
    pub last_committed: Option<MemoryLastCommittedStatus>,
    pub complete: bool,
    pub progress_percent: f64,
}

struct LoadedCorpus {
    root: PathBuf,
    manifest: PersonalMemoryCorpusManifest,
    coverage: PersonalMemoryCoverage,
    manifest_sha256: String,
    unit_index: UnitIndex,
}

struct StateLock {
    file: File,
}

impl Drop for StateLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn person_alias_from_target_page(path: &str) -> Option<&str> {
    path.strip_prefix("people/")?.strip_suffix(".md")
}

fn append_delivery_message(
    episodes: &mut Vec<DeliveryEpisode>,
    unit: &PreparedUnitFile,
    message_offset: usize,
) {
    let message = unit.m[message_offset].clone();
    if let Some(current) = episodes.last_mut().filter(|episode| {
        episode.u == unit.id && episode.o.saturating_add(episode.m.len()) == message_offset
    }) {
        current.to = message.t;
        current.m.push(message);
        return;
    }
    episodes.push(DeliveryEpisode {
        u: unit.id.clone(),
        c: unit.c.clone(),
        label: unit.label.clone(),
        kind: unit.kind,
        month: unit.month.clone(),
        from: message.t,
        to: message.t,
        o: message_offset,
        n: unit.m.len(),
        m: vec![message],
    });
}

fn remove_last_delivery_message(episodes: &mut Vec<DeliveryEpisode>) {
    let Some(last) = episodes.last_mut() else {
        return;
    };
    last.m.pop();
    if let Some(message) = last.m.last() {
        last.to = message.t;
    } else {
        episodes.pop();
    }
}

fn delivery_page_payload(
    batch_id: &str,
    number: usize,
    page_count: usize,
    episodes: Vec<DeliveryEpisode>,
    direct_people_by_unit: &BTreeMap<String, BTreeSet<String>>,
    all_people: &BTreeMap<String, String>,
) -> DeliveryPagePayload {
    let mut aliases = BTreeSet::new();
    for episode in &episodes {
        if episode.kind == ConversationKind::Direct {
            aliases.extend(
                direct_people_by_unit
                    .get(&episode.u)
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
        }
        aliases.extend(episode.m.iter().filter_map(|message| message.p.clone()));
    }
    let mut target_pages = BTreeSet::from(["index.md".to_string(), "me.md".to_string()]);
    let people = aliases
        .into_iter()
        .map(|alias| {
            target_pages.insert(format!("people/{alias}.md"));
            let label = all_people
                .get(&alias)
                .cloned()
                .unwrap_or_else(|| alias.clone());
            (alias, label)
        })
        .collect();
    let message_count = episodes.iter().map(|episode| episode.m.len()).sum();
    let text_byte_count = episodes
        .iter()
        .flat_map(|episode| &episode.m)
        .map(|message| message.x.len())
        .sum();
    DeliveryPagePayload {
        schema: PERSONAL_MEMORY_PAGE_SCHEMA,
        format_version: PERSONAL_MEMORY_FORMAT_VERSION,
        batch_id: batch_id.to_string(),
        content_trust: "untrustedChatEvidence",
        page: DeliveryPagePosition {
            number,
            page_count,
            message_count,
            text_byte_count,
        },
        target_pages: target_pages.into_iter().collect(),
        people,
        episodes,
    }
}

fn render_delivery_page(
    payload: DeliveryPagePayload,
) -> Result<RenderedDeliveryPage, RestoreError> {
    let payload_bytes = serde_json::to_vec(&payload)?;
    let page_sha256 = sha256_bytes(&payload_bytes);
    let page_token = format!("R{:06}-{}", payload.page.number, &page_sha256[..16]);
    let evidence_aliases = payload
        .episodes
        .iter()
        .flat_map(|episode| &episode.m)
        .map(|message| message.e.clone())
        .collect();
    let output = DeliveryPageOutput {
        payload,
        page_token,
        page_sha256,
    };
    let mut serialized = serde_json::to_vec(&output)?;
    serialized.push(b'\n');
    Ok(RenderedDeliveryPage {
        output,
        serialized,
        evidence_aliases,
    })
}

fn build_delivery_pages(
    corpus: &LoadedCorpus,
    batch_id: &str,
    start_unit_index: usize,
    end_unit_index_exclusive: usize,
) -> Result<Vec<RenderedDeliveryPage>, RestoreError> {
    if start_unit_index >= end_unit_index_exclusive
        || end_unit_index_exclusive > corpus.unit_index.units.len()
    {
        return Err(RestoreError::Integrity(
            "outstanding batch contains an invalid delivery range".into(),
        ));
    }
    let entries = &corpus.unit_index.units[start_unit_index..end_unit_index_exclusive];
    let units = entries
        .iter()
        .map(|entry| load_unit(corpus, entry))
        .collect::<Result<Vec<_>, _>>()?;
    let mut all_person_aliases = BTreeSet::new();
    let mut direct_people_by_unit = BTreeMap::<String, BTreeSet<String>>::new();
    for (entry, unit) in entries.iter().zip(&units) {
        let aliases = entry
            .target_pages
            .iter()
            .filter_map(|path| person_alias_from_target_page(path).map(str::to_string))
            .collect::<BTreeSet<_>>();
        all_person_aliases.extend(aliases.iter().cloned());
        if unit.kind == ConversationKind::Direct {
            direct_people_by_unit.insert(unit.id.clone(), aliases);
        }
    }
    let all_people = load_people_labels(corpus, &all_person_aliases)?;

    let mut drafts = Vec::<Vec<DeliveryEpisode>>::new();
    let mut current = Vec::<DeliveryEpisode>::new();
    for unit in &units {
        for message_offset in 0..unit.m.len() {
            append_delivery_message(&mut current, unit, message_offset);
            // Use maximum-width counters while packing. The actual page number and
            // count can only make the final compact JSON smaller.
            let candidate = render_delivery_page(delivery_page_payload(
                batch_id,
                usize::MAX,
                usize::MAX,
                current.clone(),
                &direct_people_by_unit,
                &all_people,
            ))?;
            if candidate.serialized.len() <= MAXIMUM_MEMORY_PAGE_OUTPUT_BYTES {
                continue;
            }
            remove_last_delivery_message(&mut current);
            if current.is_empty() {
                return Err(RestoreError::Integrity(format!(
                    "prepared message {} cannot fit the fixed {}-byte Pi delivery boundary; prepare with a smaller maximumMessageTextBytes",
                    unit.m[message_offset].e, MAXIMUM_MEMORY_PAGE_OUTPUT_BYTES
                )));
            }
            drafts.push(std::mem::take(&mut current));
            append_delivery_message(&mut current, unit, message_offset);
            let single = render_delivery_page(delivery_page_payload(
                batch_id,
                usize::MAX,
                usize::MAX,
                current.clone(),
                &direct_people_by_unit,
                &all_people,
            ))?;
            if single.serialized.len() > MAXIMUM_MEMORY_PAGE_OUTPUT_BYTES {
                return Err(RestoreError::Integrity(format!(
                    "prepared message {} cannot fit the fixed {}-byte Pi delivery boundary; prepare with a smaller maximumMessageTextBytes",
                    unit.m[message_offset].e, MAXIMUM_MEMORY_PAGE_OUTPUT_BYTES
                )));
            }
        }
    }
    if !current.is_empty() {
        drafts.push(current);
    }
    let page_count = drafts.len();
    let mut pages = Vec::with_capacity(page_count);
    for (index, episodes) in drafts.into_iter().enumerate() {
        let page = render_delivery_page(delivery_page_payload(
            batch_id,
            index.saturating_add(1),
            page_count,
            episodes,
            &direct_people_by_unit,
            &all_people,
        ))?;
        if page.serialized.len() > MAXIMUM_MEMORY_PAGE_OUTPUT_BYTES {
            return Err(RestoreError::Integrity(
                "deterministic memory page exceeded the fixed Pi delivery boundary".into(),
            ));
        }
        pages.push(page);
    }
    Ok(pages)
}

fn ensure_outstanding_delivery(
    corpus: &LoadedCorpus,
    outstanding: &mut OutstandingBatch,
) -> Result<Vec<RenderedDeliveryPage>, RestoreError> {
    let rendered = build_delivery_pages(
        corpus,
        &outstanding.batch_id,
        outstanding.start_unit_index,
        outstanding.end_unit_index_exclusive,
    )?;
    if outstanding.delivery_pages.is_empty() {
        outstanding.delivery_pages = rendered
            .iter()
            .map(|page| OutstandingDeliveryPage {
                number: page.output.payload.page.number,
                page_token: page.output.page_token.clone(),
                page_sha256: page.output.page_sha256.clone(),
                message_count: page.output.payload.page.message_count,
                text_byte_count: page.output.payload.page.text_byte_count,
                evidence_aliases: page.evidence_aliases.clone(),
                delivery_count: 0,
                first_delivered_at_unix_milliseconds: None,
                last_delivered_at_unix_milliseconds: None,
                review: None,
            })
            .collect();
    }
    if outstanding.delivery_pages.len() != rendered.len() {
        return Err(RestoreError::Integrity(
            "persisted memory delivery page count is inconsistent".into(),
        ));
    }
    for (descriptor, page) in outstanding.delivery_pages.iter().zip(&rendered) {
        if descriptor.number != page.output.payload.page.number
            || descriptor.page_token != page.output.page_token
            || descriptor.page_sha256 != page.output.page_sha256
            || descriptor.message_count != page.output.payload.page.message_count
            || descriptor.text_byte_count != page.output.payload.page.text_byte_count
            || descriptor.evidence_aliases != page.evidence_aliases
            || descriptor.delivery_count == 0
                && (descriptor.first_delivered_at_unix_milliseconds.is_some()
                    || descriptor.last_delivered_at_unix_milliseconds.is_some())
            || descriptor.delivery_count > 0
                && (descriptor.first_delivered_at_unix_milliseconds.is_none()
                    || descriptor.last_delivered_at_unix_milliseconds.is_none())
            || descriptor.review.is_some() && descriptor.delivery_count == 0
        {
            return Err(RestoreError::Integrity(
                "persisted memory delivery page no longer matches immutable evidence".into(),
            ));
        }
        if let Some(review) = &descriptor.review {
            let retained = review
                .retained_evidence_aliases
                .iter()
                .collect::<BTreeSet<_>>();
            if retained.len() != review.retained_evidence_aliases.len()
                || retained
                    .iter()
                    .any(|alias| !descriptor.evidence_aliases.contains(alias))
                || match review.disposition {
                    PageReviewDisposition::DurableEvidenceRetained => retained.is_empty(),
                    PageReviewDisposition::ReviewedNoDurableMemory => !retained.is_empty(),
                }
            {
                return Err(RestoreError::Integrity(
                    "persisted memory page review is inconsistent with its delivered evidence"
                        .into(),
                ));
            }
        }
    }
    let flattened = rendered
        .iter()
        .flat_map(|page| page.evidence_aliases.iter().cloned())
        .collect::<Vec<_>>();
    if flattened != outstanding.evidence_aliases {
        return Err(RestoreError::Integrity(
            "memory delivery pages do not cover the exact outstanding evidence sequence".into(),
        ));
    }
    Ok(rendered)
}

fn batch_delivery_summary(outstanding: &OutstandingBatch) -> MemoryBatchDelivery {
    let acknowledged = outstanding
        .delivery_pages
        .iter()
        .filter(|page| page.review.is_some())
        .collect::<Vec<_>>();
    let delivered_message_count = outstanding
        .delivery_pages
        .iter()
        .filter(|page| page.delivery_count > 0)
        .map(|page| page.message_count)
        .sum();
    let acknowledged_message_count = acknowledged.iter().map(|page| page.message_count).sum();
    let retained_evidence_count = acknowledged
        .iter()
        .filter_map(|page| page.review.as_ref())
        .map(|review| review.retained_evidence_aliases.len())
        .sum();
    let next_page_number = outstanding
        .delivery_pages
        .iter()
        .find(|page| page.review.is_none())
        .map(|page| page.number);
    MemoryBatchDelivery {
        page_count: outstanding.delivery_pages.len(),
        acknowledged_page_count: acknowledged.len(),
        delivered_message_count,
        acknowledged_message_count,
        retained_evidence_count,
        maximum_page_output_bytes: MAXIMUM_MEMORY_PAGE_OUTPUT_BYTES,
        review_complete: next_page_number.is_none() && !outstanding.delivery_pages.is_empty(),
        next_page_number,
    }
}

pub fn next_personal_memory_batch(
    corpus_directory: &Path,
    state_path: &Path,
    wiki_directory: Option<&Path>,
    maximum_text_bytes: usize,
) -> Result<Value, RestoreError> {
    if !(MINIMUM_NEXT_TEXT_BYTES..=MAXIMUM_NEXT_TEXT_BYTES).contains(&maximum_text_bytes) {
        return Err(RestoreError::Integrity(format!(
            "memory next --max-text-bytes must be between {MINIMUM_NEXT_TEXT_BYTES} and {MAXIMUM_NEXT_TEXT_BYTES}"
        )));
    }
    let corpus = load_corpus(corpus_directory)?;
    let _lock = acquire_state_lock(state_path)?;
    let mut state = load_or_initialize_state(state_path, &corpus)?;
    if state.next_unit_index > corpus.unit_index.units.len() {
        return Err(RestoreError::Integrity(
            "memory state cursor exceeds the immutable corpus unit count".into(),
        ));
    }
    let verified_wiki_before = if state.outstanding.is_none() {
        let current = wiki_directory.map(scan_wiki).transpose()?;
        if let (Some(current), Some(committed)) = (&current, &state.committed_wiki) {
            if !wiki_snapshots_same_bytes(current, committed) {
                return Err(RestoreError::Integrity(
                    "wiki changed outside the committed personal-memory batch protocol".into(),
                ));
            }
        }
        current
    } else {
        None
    };
    if state.outstanding.is_none() && state.next_unit_index == corpus.unit_index.units.len() {
        return Ok(json!({
            "schema": PERSONAL_MEMORY_BATCH_SCHEMA,
            "formatVersion": PERSONAL_MEMORY_FORMAT_VERSION,
            "deliveryOrder": corpus.manifest.delivery_order,
            "complete": true,
            "position": {
                "firstUnit": state.next_unit_index.saturating_add(1),
                "unitCount": 0,
                "totalUnits": corpus.unit_index.units.len(),
                "messageCount": 0,
                "textByteCount": 0
            },
            "delivery": {
                "pageCount": 0,
                "acknowledgedPageCount": 0,
                "deliveredMessageCount": 0,
                "acknowledgedMessageCount": 0,
                "retainedEvidenceCount": 0,
                "maximumPageOutputBytes": MAXIMUM_MEMORY_PAGE_OUTPUT_BYTES,
                "reviewComplete": true,
                "nextPageNumber": null
            }
        }));
    }

    if state.outstanding.is_none() {
        let start = state.next_unit_index;
        let first = corpus.unit_index.units.get(start).ok_or_else(|| {
            RestoreError::Integrity("memory state points to a missing corpus unit".into())
        })?;
        if first.text_byte_count > maximum_text_bytes {
            return Err(RestoreError::Integrity(format!(
                "next prepared unit contains {} text bytes; use --max-text-bytes of at least that value or prepare with a smaller maximumUnitTextBytes",
                first.text_byte_count
            )));
        }
        let mut end = start;
        let mut text_byte_count = 0usize;
        let mut message_count = 0usize;
        let mut target_pages = BTreeSet::new();
        let mut evidence_aliases = Vec::new();
        let mut unit_hashes = Vec::new();
        while let Some(entry) = corpus.unit_index.units.get(end) {
            let next_text = text_byte_count.saturating_add(entry.text_byte_count);
            let next_messages = message_count.saturating_add(entry.message_count);
            if end > start
                && (next_text > maximum_text_bytes || next_messages > MAXIMUM_BATCH_MESSAGES)
            {
                break;
            }
            if next_text > maximum_text_bytes || next_messages > MAXIMUM_BATCH_MESSAGES {
                return Err(RestoreError::Integrity(
                    "one prepared unit exceeds the requested batch safety bound".into(),
                ));
            }
            text_byte_count = next_text;
            message_count = next_messages;
            target_pages.extend(entry.target_pages.iter().cloned());
            evidence_aliases.extend(entry.evidence_aliases.iter().cloned());
            unit_hashes.push(entry.sha256.clone());
            end = end.saturating_add(1);
        }
        let batch_id = batch_id(&corpus.manifest_sha256, start, end, &unit_hashes);
        let wiki_before = verified_wiki_before.or_else(|| state.committed_wiki.clone());
        state.outstanding = Some(OutstandingBatch {
            batch_id,
            start_unit_index: start,
            end_unit_index_exclusive: end,
            text_byte_count,
            message_count,
            target_pages: target_pages.into_iter().collect(),
            evidence_aliases,
            wiki_before,
            delivery_pages: Vec::new(),
        });
    }
    let outstanding = state
        .outstanding
        .as_mut()
        .ok_or_else(|| RestoreError::Integrity("outstanding batch was not persisted".into()))?;
    ensure_outstanding_delivery(&corpus, outstanding)?;
    state.updated_at_unix_milliseconds = now_unix_milliseconds()?;
    write_state_atomic(state_path, &state)?;
    render_outstanding_batch(
        state
            .outstanding
            .as_ref()
            .ok_or_else(|| RestoreError::Integrity("outstanding batch was not persisted".into()))?,
        corpus.unit_index.units.len(),
        corpus.manifest.delivery_order,
    )
}

pub fn next_personal_memory_page(
    corpus_directory: &Path,
    state_path: &Path,
    batch_id: &str,
) -> Result<Value, RestoreError> {
    validate_batch_identifier(batch_id)?;
    let corpus = load_corpus(corpus_directory)?;
    let _lock = acquire_state_lock(state_path)?;
    let mut state = load_existing_state(state_path, &corpus)?;
    let outstanding = state
        .outstanding
        .as_mut()
        .ok_or_else(|| RestoreError::Integrity("memory page has no outstanding batch".into()))?;
    if !memory_selector_matches(batch_id, &outstanding.batch_id) {
        return Err(RestoreError::Integrity(
            "memory page batch identifier does not match the outstanding batch".into(),
        ));
    }
    let resolved_batch_id = outstanding.batch_id.clone();
    let rendered = ensure_outstanding_delivery(&corpus, outstanding)?;
    let Some(index) = outstanding
        .delivery_pages
        .iter()
        .position(|page| page.review.is_none())
    else {
        let summary = batch_delivery_summary(outstanding);
        state.updated_at_unix_milliseconds = now_unix_milliseconds()?;
        write_state_atomic(state_path, &state)?;
        return Ok(json!({
            "schema": PERSONAL_MEMORY_PAGE_SCHEMA,
            "formatVersion": PERSONAL_MEMORY_FORMAT_VERSION,
            "batchId": resolved_batch_id,
            "reviewComplete": true,
            "pageCount": summary.page_count,
            "acknowledgedPageCount": summary.acknowledged_page_count,
            "acknowledgedMessageCount": summary.acknowledged_message_count,
            "retainedEvidenceCount": summary.retained_evidence_count
        }));
    };
    let now = now_unix_milliseconds()?;
    let descriptor = &mut outstanding.delivery_pages[index];
    descriptor.delivery_count = descriptor.delivery_count.saturating_add(1);
    descriptor
        .first_delivered_at_unix_milliseconds
        .get_or_insert(now);
    descriptor.last_delivered_at_unix_milliseconds = Some(now);
    let value = serde_json::to_value(&rendered[index].output)?;
    let mut serialized = serde_json::to_vec(&value)?;
    serialized.push(b'\n');
    if serialized.len() > MAXIMUM_MEMORY_PAGE_OUTPUT_BYTES {
        return Err(RestoreError::Integrity(
            "serialized memory page exceeded the fixed Pi delivery boundary".into(),
        ));
    }
    state.updated_at_unix_milliseconds = now;
    write_state_atomic(state_path, &state)?;
    Ok(value)
}

pub fn acknowledge_personal_memory_page(
    corpus_directory: &Path,
    state_path: &Path,
    batch_id: &str,
    page_token: &str,
    retained_evidence_aliases: &[String],
    reviewed_no_durable_memory: bool,
) -> Result<MemoryPageAcknowledgement, RestoreError> {
    validate_batch_identifier(batch_id)?;
    if page_token.is_empty() || page_token.len() > 128 || page_token.chars().any(char::is_control) {
        return Err(RestoreError::Integrity(
            "memory page token is empty or outside safe limits".into(),
        ));
    }
    let corpus = load_corpus(corpus_directory)?;
    let _lock = acquire_state_lock(state_path)?;
    let mut state = load_existing_state(state_path, &corpus)?;
    let outstanding = state.outstanding.as_mut().ok_or_else(|| {
        RestoreError::Integrity("memory acknowledge has no outstanding batch".into())
    })?;
    if !memory_selector_matches(batch_id, &outstanding.batch_id) {
        return Err(RestoreError::Integrity(
            "memory acknowledge batch identifier does not match the outstanding batch".into(),
        ));
    }
    let resolved_batch_id = outstanding.batch_id.clone();
    ensure_outstanding_delivery(&corpus, outstanding)?;
    let index = if page_token == PERSONAL_MEMORY_CURRENT_SELECTOR {
        outstanding
            .delivery_pages
            .iter()
            .position(|page| page.review.is_none())
    } else {
        outstanding
            .delivery_pages
            .iter()
            .position(|page| page.page_token == page_token)
    }
    .ok_or_else(|| {
        RestoreError::Integrity(
            "memory acknowledge page token does not belong to the outstanding batch".into(),
        )
    })?;
    let first_unreviewed = outstanding
        .delivery_pages
        .iter()
        .position(|page| page.review.is_none());
    if outstanding.delivery_pages[index].review.is_none() && first_unreviewed != Some(index) {
        return Err(RestoreError::Integrity(
            "memory pages must be acknowledged in deterministic order".into(),
        ));
    }
    let descriptor = &mut outstanding.delivery_pages[index];
    let resolved_page_token = descriptor.page_token.clone();
    let disposition = if reviewed_no_durable_memory {
        if !retained_evidence_aliases.is_empty() {
            return Err(RestoreError::Integrity(
                "reviewed-no-durable-memory page acknowledgement may not retain evidence".into(),
            ));
        }
        PageReviewDisposition::ReviewedNoDurableMemory
    } else {
        if retained_evidence_aliases.is_empty() {
            return Err(RestoreError::Integrity(
                "a durable-evidence page acknowledgement must retain at least one exact page evidence alias"
                    .into(),
            ));
        }
        PageReviewDisposition::DurableEvidenceRetained
    };
    let retained = retained_evidence_aliases.iter().collect::<BTreeSet<_>>();
    if retained.len() != retained_evidence_aliases.len()
        || retained
            .iter()
            .any(|alias| !descriptor.evidence_aliases.contains(alias))
    {
        return Err(RestoreError::Integrity(
            "memory page acknowledgement retained a duplicate or non-page evidence alias".into(),
        ));
    }
    if let Some(review) = &descriptor.review {
        if review.disposition != disposition
            || review.retained_evidence_aliases != retained_evidence_aliases
        {
            return Err(RestoreError::Integrity(
                "an acknowledged memory page cannot be reclassified".into(),
            ));
        }
        let summary = batch_delivery_summary(outstanding);
        return Ok(MemoryPageAcknowledgement {
            schema: PERSONAL_MEMORY_PAGE_SCHEMA,
            format_version: PERSONAL_MEMORY_FORMAT_VERSION,
            batch_id: resolved_batch_id,
            page_token: resolved_page_token,
            acknowledged: true,
            already_acknowledged: true,
            disposition,
            page_count: summary.page_count,
            acknowledged_page_count: summary.acknowledged_page_count,
            acknowledged_message_count: summary.acknowledged_message_count,
            retained_evidence_count: summary.retained_evidence_count,
            review_complete: summary.review_complete,
            next_page_number: summary.next_page_number,
        });
    }
    if descriptor.delivery_count == 0 {
        return Err(RestoreError::Integrity(
            "memory page must be delivered before it can be acknowledged".into(),
        ));
    }
    descriptor.review = Some(PageReviewRecord {
        disposition,
        retained_evidence_aliases: retained_evidence_aliases.to_vec(),
        acknowledged_at_unix_milliseconds: now_unix_milliseconds()?,
    });
    let summary = batch_delivery_summary(outstanding);
    state.updated_at_unix_milliseconds = now_unix_milliseconds()?;
    write_state_atomic(state_path, &state)?;
    Ok(MemoryPageAcknowledgement {
        schema: PERSONAL_MEMORY_PAGE_SCHEMA,
        format_version: PERSONAL_MEMORY_FORMAT_VERSION,
        batch_id: resolved_batch_id,
        page_token: resolved_page_token,
        acknowledged: true,
        already_acknowledged: false,
        disposition,
        page_count: summary.page_count,
        acknowledged_page_count: summary.acknowledged_page_count,
        acknowledged_message_count: summary.acknowledged_message_count,
        retained_evidence_count: summary.retained_evidence_count,
        review_complete: summary.review_complete,
        next_page_number: summary.next_page_number,
    })
}

pub fn commit_personal_memory_batch(
    corpus_directory: &Path,
    state_path: &Path,
    batch_id: &str,
    wiki_directory: &Path,
) -> Result<MemoryCommitResult, RestoreError> {
    commit_personal_memory_batch_with_disposition(
        corpus_directory,
        state_path,
        batch_id,
        wiki_directory,
        MemoryCommitDisposition::WikiUpdated,
    )
}

pub fn commit_personal_memory_batch_reviewed_no_durable_memory(
    corpus_directory: &Path,
    state_path: &Path,
    batch_id: &str,
    wiki_directory: &Path,
) -> Result<MemoryCommitResult, RestoreError> {
    commit_personal_memory_batch_with_disposition(
        corpus_directory,
        state_path,
        batch_id,
        wiki_directory,
        MemoryCommitDisposition::ReviewedNoDurableMemory,
    )
}

fn validate_batch_identifier(batch_id: &str) -> Result<(), RestoreError> {
    if batch_id.is_empty() || batch_id.len() > 128 || batch_id.chars().any(char::is_control) {
        return Err(RestoreError::Integrity(
            "memory batch identifier is empty or outside safe limits".into(),
        ));
    }
    Ok(())
}

fn memory_selector_matches(requested: &str, actual: &str) -> bool {
    requested == PERSONAL_MEMORY_CURRENT_SELECTOR || requested == actual
}

fn commit_personal_memory_batch_with_disposition(
    corpus_directory: &Path,
    state_path: &Path,
    batch_id: &str,
    wiki_directory: &Path,
    disposition: MemoryCommitDisposition,
) -> Result<MemoryCommitResult, RestoreError> {
    validate_batch_identifier(batch_id)?;
    let corpus = load_corpus(corpus_directory)?;
    let _lock = acquire_state_lock(state_path)?;
    let mut state = load_existing_state(state_path, &corpus)?;
    let resolved_batch_id = if batch_id == PERSONAL_MEMORY_CURRENT_SELECTOR {
        state
            .outstanding
            .as_ref()
            .map(|outstanding| outstanding.batch_id.clone())
            .or_else(|| {
                state
                    .last_committed
                    .as_ref()
                    .map(|committed| committed.batch_id.clone())
            })
            .ok_or_else(|| {
                RestoreError::Integrity(
                    "memory commit has no current or previously committed batch".into(),
                )
            })?
    } else {
        batch_id.to_string()
    };
    if let Some(committed) = state
        .last_committed
        .as_ref()
        .filter(|committed| committed.batch_id == resolved_batch_id)
    {
        return Ok(MemoryCommitResult {
            schema: PERSONAL_MEMORY_STATE_SCHEMA,
            format_version: PERSONAL_MEMORY_FORMAT_VERSION,
            batch_id: resolved_batch_id,
            committed: true,
            already_committed: true,
            disposition: committed.disposition,
            next_unit_index: state.next_unit_index,
            total_units: corpus.unit_index.units.len(),
            complete: state.next_unit_index == corpus.unit_index.units.len(),
            changed_pages: Vec::new(),
            reviewed_page_count: committed.reviewed_page_count,
            reviewed_message_count: committed.reviewed_message_count,
            retained_evidence_count: committed.retained_evidence_count,
        });
    }
    let mut outstanding = state
        .outstanding
        .clone()
        .ok_or_else(|| RestoreError::Integrity("memory commit has no outstanding batch".into()))?;
    if outstanding.batch_id != resolved_batch_id {
        return Err(RestoreError::Integrity(
            "memory commit batch identifier does not match the outstanding batch".into(),
        ));
    }
    ensure_outstanding_delivery(&corpus, &mut outstanding)?;
    if outstanding.delivery_pages.is_empty()
        || outstanding
            .delivery_pages
            .iter()
            .any(|page| page.delivery_count == 0 || page.review.is_none())
    {
        return Err(RestoreError::Integrity(
            "memory commit requires every deterministic delivery page to be fetched and acknowledged"
                .into(),
        ));
    }
    let retained_evidence_aliases = outstanding
        .delivery_pages
        .iter()
        .filter_map(|page| page.review.as_ref())
        .flat_map(|review| review.retained_evidence_aliases.iter().cloned())
        .collect::<BTreeSet<_>>();
    if disposition == MemoryCommitDisposition::ReviewedNoDurableMemory
        && (!retained_evidence_aliases.is_empty()
            || outstanding.delivery_pages.iter().any(|page| {
                page.review.as_ref().is_some_and(|review| {
                    review.disposition == PageReviewDisposition::DurableEvidenceRetained
                })
            }))
    {
        return Err(RestoreError::Integrity(
            "reviewed-no-durable-memory batch commit conflicts with retained page evidence".into(),
        ));
    }
    let current_wiki = scan_wiki(wiki_directory)?;
    let changed_pages = match disposition {
        MemoryCommitDisposition::WikiUpdated => {
            let changed_pages = validate_wiki_commit(
                &current_wiki,
                outstanding.wiki_before.as_ref(),
                &outstanding.target_pages,
                &retained_evidence_aliases
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>(),
                corpus.manifest.evidence_count,
            )?;
            if changed_pages.iter().any(|path| path == "me.md") {
                let me = current_wiki.get("me.md").ok_or_else(|| {
                    RestoreError::Integrity("changed account-holder wiki page is missing".into())
                })?;
                let self_evidence_aliases = load_self_evidence_aliases(&corpus, &me.citations)?;
                validate_me_self_attribution(me, &self_evidence_aliases)?;
            }
            changed_pages
        }
        MemoryCommitDisposition::ReviewedNoDurableMemory => {
            validate_reviewed_no_durable_memory_commit(
                &current_wiki,
                outstanding.wiki_before.as_ref(),
                corpus.manifest.evidence_count,
            )?;
            Vec::new()
        }
    };
    state.next_unit_index = outstanding.end_unit_index_exclusive;
    state.outstanding = None;
    state.last_committed = Some(CommittedBatch {
        batch_id: resolved_batch_id.clone(),
        committed_at_unix_milliseconds: now_unix_milliseconds()?,
        disposition,
        reviewed_page_count: outstanding.delivery_pages.len(),
        reviewed_message_count: outstanding.message_count,
        retained_evidence_count: retained_evidence_aliases.len(),
    });
    state.committed_wiki = Some(current_wiki);
    state.updated_at_unix_milliseconds = now_unix_milliseconds()?;
    write_state_atomic(state_path, &state)?;
    Ok(MemoryCommitResult {
        schema: PERSONAL_MEMORY_STATE_SCHEMA,
        format_version: PERSONAL_MEMORY_FORMAT_VERSION,
        batch_id: resolved_batch_id,
        committed: true,
        already_committed: false,
        disposition,
        next_unit_index: state.next_unit_index,
        total_units: corpus.unit_index.units.len(),
        complete: state.next_unit_index == corpus.unit_index.units.len(),
        changed_pages,
        reviewed_page_count: outstanding.delivery_pages.len(),
        reviewed_message_count: outstanding.message_count,
        retained_evidence_count: retained_evidence_aliases.len(),
    })
}

pub fn personal_memory_status(
    corpus_directory: &Path,
    state_path: Option<&Path>,
) -> Result<MemoryStatus, RestoreError> {
    let corpus = load_corpus(corpus_directory)?;
    let state = if let Some(path) = state_path {
        if path.try_exists()? {
            Some(load_existing_state(path, &corpus)?)
        } else {
            None
        }
    } else {
        None
    };
    let next_unit_index = state
        .as_ref()
        .map(|state| state.next_unit_index)
        .unwrap_or_default();
    let outstanding_unit_count = state
        .as_ref()
        .and_then(|state| state.outstanding.as_ref())
        .map(|batch| {
            batch
                .end_unit_index_exclusive
                .saturating_sub(batch.start_unit_index)
        })
        .unwrap_or_default();
    let outstanding_pages = state
        .as_ref()
        .and_then(|state| state.outstanding.as_ref())
        .map(|batch| batch.delivery_pages.as_slice())
        .unwrap_or_default();
    let delivered_page_count = outstanding_pages
        .iter()
        .filter(|page| page.delivery_count > 0)
        .count();
    let acknowledged_pages = outstanding_pages
        .iter()
        .filter(|page| page.review.is_some())
        .collect::<Vec<_>>();
    let acknowledged_message_count = acknowledged_pages
        .iter()
        .map(|page| page.message_count)
        .sum();
    let retained_evidence_count = acknowledged_pages
        .iter()
        .filter_map(|page| page.review.as_ref())
        .map(|review| review.retained_evidence_aliases.len())
        .sum();
    let review_complete = state
        .as_ref()
        .and_then(|state| state.outstanding.as_ref())
        .map(|_| {
            !outstanding_pages.is_empty() && acknowledged_pages.len() == outstanding_pages.len()
        });
    let last_committed = state
        .as_ref()
        .and_then(|state| state.last_committed.as_ref())
        .map(|committed| MemoryLastCommittedStatus {
            batch_id: committed.batch_id.clone(),
            disposition: committed.disposition,
            reviewed_page_count: committed.reviewed_page_count,
            reviewed_message_count: committed.reviewed_message_count,
            retained_evidence_count: committed.retained_evidence_count,
        });
    let total = corpus.unit_index.units.len();
    let committed_message_count = corpus.unit_index.units[..next_unit_index]
        .iter()
        .map(|unit| unit.message_count as u64)
        .sum();
    let progress_percent = if total == 0 {
        100.0
    } else {
        next_unit_index as f64 * 100.0 / total as f64
    };
    Ok(MemoryStatus {
        schema: PERSONAL_MEMORY_STATE_SCHEMA,
        format_version: PERSONAL_MEMORY_FORMAT_VERSION,
        corpus_manifest_valid: true,
        state_present: state.is_some(),
        unit_count: total,
        evidence_count: corpus.manifest.evidence_count,
        scanned_message_count: corpus.manifest.scanned_message_count,
        selected_message_count: corpus.manifest.selected_message_count,
        source_coverage_complete: corpus.manifest.source_coverage_complete,
        content_complete: corpus.manifest.content_complete,
        unmatched_message_table_count: corpus.manifest.unmatched_message_table_count,
        limitation_codes: corpus.coverage.limitation_codes.clone(),
        delivery_order: corpus.manifest.delivery_order,
        next_unit_index,
        committed_unit_count: next_unit_index,
        committed_message_count,
        outstanding_batch_id: state
            .as_ref()
            .and_then(|state| state.outstanding.as_ref())
            .map(|batch| batch.batch_id.clone()),
        outstanding_unit_count,
        outstanding_page_count: outstanding_pages.len(),
        delivered_page_count,
        acknowledged_page_count: acknowledged_pages.len(),
        acknowledged_message_count,
        retained_evidence_count,
        review_complete,
        last_committed,
        complete: next_unit_index == total
            && state
                .as_ref()
                .is_none_or(|state| state.outstanding.is_none()),
        progress_percent,
    })
}

fn load_corpus(corpus_directory: &Path) -> Result<LoadedCorpus, RestoreError> {
    ensure_private_directory(corpus_directory)?;
    let root = fs::canonicalize(corpus_directory)?;
    if root != corpus_directory.canonicalize()? {
        return Err(RestoreError::UnsafePath(
            "personal-memory corpus path could not be canonicalized consistently".into(),
        ));
    }
    let manifest_path = root.join("manifest.json");
    let manifest_bytes =
        read_immutable_owner_file_limited(&manifest_path, MAXIMUM_CONTROL_FILE_BYTES)?;
    let manifest_sha256 = sha256_bytes(&manifest_bytes);
    let manifest: PersonalMemoryCorpusManifest = serde_json::from_slice(&manifest_bytes)?;
    if manifest.schema != PERSONAL_MEMORY_CORPUS_SCHEMA
        || manifest.format_version != PERSONAL_MEMORY_FORMAT_VERSION
        || !manifest.immutable_index
        || !manifest.account_holder_attribution_bound
        || manifest.content_trust != "untrustedChatEvidence"
    {
        return Err(RestoreError::Integrity(
            "personal-memory corpus manifest invariants are invalid".into(),
        ));
    }
    for record in &manifest.files {
        if !valid_sha256(&record.sha256) {
            return Err(RestoreError::Integrity(
                "corpus manifest contains an invalid file hash".into(),
            ));
        }
        let path = safe_corpus_path(&root, &record.relative_path)?;
        let metadata = immutable_owner_file_metadata(&path)?;
        if metadata.len() != record.byte_count {
            return Err(RestoreError::Integrity(format!(
                "immutable corpus file {} has an unexpected byte count",
                record.relative_path
            )));
        }
    }
    let coverage_record = manifest
        .files
        .iter()
        .find(|record| record.relative_path == "coverage.json")
        .ok_or_else(|| RestoreError::Integrity("corpus manifest has no coverage report".into()))?;
    let coverage_path = safe_corpus_path(&root, &coverage_record.relative_path)?;
    let coverage_bytes =
        read_immutable_owner_file_limited(&coverage_path, MAXIMUM_CONTROL_FILE_BYTES)?;
    if coverage_bytes.len() as u64 != coverage_record.byte_count
        || sha256_bytes(&coverage_bytes) != coverage_record.sha256
    {
        return Err(RestoreError::Integrity(
            "coverage report does not match the immutable corpus manifest".into(),
        ));
    }
    let coverage: PersonalMemoryCoverage = serde_json::from_slice(&coverage_bytes)?;
    if coverage.scanned_message_count != manifest.scanned_message_count
        || coverage.selected_message_count != manifest.selected_message_count
        || coverage.unit_count != manifest.unit_count as u64
        || coverage.source_coverage_complete != manifest.source_coverage_complete
        || coverage.content_complete != manifest.content_complete
        || coverage.limitation_codes.len() > 128
        || coverage.limitation_codes.iter().any(|code| {
            code.is_empty()
                || code.len() > 128
                || !code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
    {
        return Err(RestoreError::Integrity(
            "coverage report does not match corpus manifest accounting".into(),
        ));
    }
    let unit_index_record = manifest
        .files
        .iter()
        .find(|record| record.relative_path == "batches/index.json")
        .ok_or_else(|| {
            RestoreError::Integrity("corpus manifest has no prepared-unit index".into())
        })?;
    let unit_index_path = safe_corpus_path(&root, &unit_index_record.relative_path)?;
    let unit_index_bytes =
        read_immutable_owner_file_limited(&unit_index_path, MAXIMUM_CONTROL_FILE_BYTES)?;
    if unit_index_bytes.len() as u64 != unit_index_record.byte_count
        || sha256_bytes(&unit_index_bytes) != unit_index_record.sha256
    {
        return Err(RestoreError::Integrity(
            "prepared-unit index does not match the immutable corpus manifest".into(),
        ));
    }
    let unit_index: UnitIndex = serde_json::from_slice(&unit_index_bytes)?;
    if unit_index.schema != "greenbubbles.personal-memory-unit-index.v1"
        || unit_index.format_version != PERSONAL_MEMORY_FORMAT_VERSION
        || unit_index.units.len() != manifest.unit_count
    {
        return Err(RestoreError::Integrity(
            "prepared-unit index schema or count is invalid".into(),
        ));
    }
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut evidence_count = 0_u64;
    let mut largest_unit_text_bytes = 0usize;
    for (index, unit) in unit_index.units.iter().enumerate() {
        let expected_id = format!("U{:06}", index.saturating_add(1));
        if unit.id != expected_id
            || !ids.insert(unit.id.clone())
            || !paths.insert(unit.relative_path.clone())
            || unit.relative_path != format!("batches/{}.json", unit.id)
            || !valid_sha256(&unit.sha256)
            || unit.message_count != unit.evidence_aliases.len()
            || unit.message_count > 1_000
            || unit.text_byte_count > 512 * 1024
        {
            return Err(RestoreError::Integrity(
                "prepared-unit index contains an invalid or duplicate entry".into(),
            ));
        }
        safe_corpus_path(&root, &unit.relative_path)?;
        evidence_count = evidence_count.saturating_add(unit.evidence_aliases.len() as u64);
        largest_unit_text_bytes = largest_unit_text_bytes.max(unit.text_byte_count);
        for alias in &unit.evidence_aliases {
            if !valid_evidence_alias(alias, manifest.evidence_count) {
                return Err(RestoreError::Integrity(
                    "prepared-unit index contains an invalid evidence alias".into(),
                ));
            }
        }
        for page in &unit.target_pages {
            validate_wiki_relative_path(page)?;
        }
    }
    if evidence_count != manifest.evidence_count
        || manifest.evidence_count > manifest.selected_message_count
        || (manifest.content_complete && manifest.selected_message_count != manifest.evidence_count)
        || largest_unit_text_bytes != manifest.largest_unit_text_bytes
    {
        return Err(RestoreError::Integrity(
            "corpus evidence or unit accounting does not match its manifest".into(),
        ));
    }
    Ok(LoadedCorpus {
        root,
        manifest,
        coverage,
        manifest_sha256,
        unit_index,
    })
}

fn render_outstanding_batch(
    outstanding: &OutstandingBatch,
    total_units: usize,
    delivery_order: MemoryDeliveryOrder,
) -> Result<Value, RestoreError> {
    if outstanding.start_unit_index >= outstanding.end_unit_index_exclusive {
        return Err(RestoreError::Integrity(
            "outstanding batch contains an invalid unit range".into(),
        ));
    }
    let output = MemoryBatchOutput {
        schema: PERSONAL_MEMORY_BATCH_SCHEMA,
        format_version: PERSONAL_MEMORY_FORMAT_VERSION,
        delivery_order,
        batch_id: outstanding.batch_id.clone(),
        complete: false,
        content_trust: "untrustedChatEvidence",
        position: BatchPosition {
            first_unit: outstanding.start_unit_index.saturating_add(1),
            unit_count: outstanding
                .end_unit_index_exclusive
                .saturating_sub(outstanding.start_unit_index),
            total_units,
            message_count: outstanding.message_count,
            text_byte_count: outstanding.text_byte_count,
        },
        delivery: batch_delivery_summary(outstanding),
    };
    Ok(serde_json::to_value(output)?)
}

fn load_unit(
    corpus: &LoadedCorpus,
    entry: &UnitIndexEntry,
) -> Result<PreparedUnitFile, RestoreError> {
    let path = safe_corpus_path(&corpus.root, &entry.relative_path)?;
    let bytes = read_immutable_owner_file_limited(&path, entry.byte_count.max(1))?;
    if bytes.len() as u64 != entry.byte_count || sha256_bytes(&bytes) != entry.sha256 {
        return Err(RestoreError::Integrity(format!(
            "prepared unit {} does not match its immutable index",
            entry.id
        )));
    }
    let unit: PreparedUnitFile = serde_json::from_slice(&bytes)?;
    if unit.schema != PERSONAL_MEMORY_BATCH_SCHEMA
        || unit.id != entry.id
        || unit.m.len() != entry.message_count
        || unit.m.iter().map(|message| message.x.len()).sum::<usize>() != entry.text_byte_count
        || unit
            .m
            .iter()
            .map(|message| message.e.as_str())
            .ne(entry.evidence_aliases.iter().map(String::as_str))
    {
        return Err(RestoreError::Integrity(format!(
            "prepared unit {} has invalid internal accounting",
            entry.id
        )));
    }
    Ok(unit)
}

fn load_people_labels(
    corpus: &LoadedCorpus,
    requested: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, RestoreError> {
    if requested.is_empty() {
        return Ok(BTreeMap::new());
    }
    let record = corpus
        .manifest
        .files
        .iter()
        .find(|record| record.relative_path == "contacts.jsonl")
        .ok_or_else(|| RestoreError::Integrity("corpus contacts sidecar is missing".into()))?;
    let path = safe_corpus_path(&corpus.root, &record.relative_path)?;
    let metadata = immutable_owner_file_metadata(&path)?;
    if metadata.len() != record.byte_count || sha256_file(&path)? != record.sha256 {
        return Err(RestoreError::Integrity(
            "corpus contacts sidecar no longer matches the immutable manifest".into(),
        ));
    }
    let mut labels = BTreeMap::new();
    let reader = BufReader::new(File::open(path)?);
    for line in reader.lines() {
        let record: ContactSidecarRecord = serde_json::from_str(&line?)?;
        if let Some(alias) = record.alias.as_deref() {
            if requested.contains(alias) {
                labels.insert(alias.to_string(), model_safe_person_label(&record, alias));
            }
        }
    }
    for alias in requested {
        labels.entry(alias.clone()).or_insert_with(|| alias.clone());
    }
    Ok(labels)
}

fn load_self_evidence_aliases(
    corpus: &LoadedCorpus,
    requested: &BTreeSet<String>,
) -> Result<BTreeSet<String>, RestoreError> {
    if requested.is_empty() {
        return Ok(BTreeSet::new());
    }
    let record = corpus
        .manifest
        .files
        .iter()
        .find(|record| record.relative_path == "evidence.jsonl")
        .ok_or_else(|| RestoreError::Integrity("corpus evidence sidecar is missing".into()))?;
    let path = safe_corpus_path(&corpus.root, &record.relative_path)?;
    let metadata = immutable_owner_file_metadata(&path)?;
    if metadata.len() != record.byte_count {
        return Err(RestoreError::Integrity(
            "corpus evidence sidecar has an unexpected byte count".into(),
        ));
    }
    let mut reader = BufReader::new(File::open(path)?);
    let mut line = Vec::new();
    let mut hasher = Sha256::new();
    let mut byte_count = 0_u64;
    let mut found = BTreeSet::new();
    let mut self_aliases = BTreeSet::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        byte_count = byte_count.saturating_add(read as u64);
        hasher.update(&line);
        let evidence: EvidenceRecord = serde_json::from_slice(&line)?;
        if requested.contains(&evidence.alias) {
            if !found.insert(evidence.alias.clone()) {
                return Err(RestoreError::Integrity(
                    "corpus evidence sidecar contains a repeated requested alias".into(),
                ));
            }
            match evidence.actor.as_str() {
                "self" => {
                    self_aliases.insert(evidence.alias);
                }
                "other" | "unknown" => {}
                _ => {
                    return Err(RestoreError::Integrity(
                        "corpus evidence sidecar contains an invalid actor".into(),
                    ));
                }
            }
        }
    }
    if byte_count != record.byte_count
        || hex::encode(hasher.finalize()) != record.sha256
        || found != *requested
    {
        return Err(RestoreError::Integrity(
            "corpus evidence sidecar no longer matches its immutable manifest or cited aliases"
                .into(),
        ));
    }
    Ok(self_aliases)
}

fn batch_id(manifest_sha256: &str, start: usize, end: usize, unit_hashes: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"greenbubbles-memory-batch-v1\0");
    hasher.update(manifest_sha256.as_bytes());
    hasher.update((start as u64).to_le_bytes());
    hasher.update((end as u64).to_le_bytes());
    for hash in unit_hashes {
        hasher.update(hash.as_bytes());
    }
    let digest = hex::encode(hasher.finalize());
    format!(
        "B{:06}-{:06}-{}",
        start.saturating_add(1),
        end,
        &digest[..16]
    )
}

fn safe_corpus_path(root: &Path, relative: &str) -> Result<PathBuf, RestoreError> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path.as_os_str().is_empty()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(RestoreError::UnsafePath(
            "corpus manifest contains an unsafe relative path".into(),
        ));
    }
    let path = root.join(relative_path);
    let parent = path
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath("corpus file has no parent".into()))?;
    let canonical_parent = fs::canonicalize(parent)?;
    if !canonical_parent.starts_with(root) {
        return Err(RestoreError::UnsafePath(
            "corpus file parent escapes the immutable index".into(),
        ));
    }
    Ok(path)
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn acquire_state_lock(state_path: &Path) -> Result<StateLock, RestoreError> {
    let parent = state_path.parent().unwrap_or_else(|| Path::new("."));
    ensure_private_directory(parent)?;
    let file_name = state_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| RestoreError::UnsafePath("memory state path has no UTF-8 name".into()))?;
    if file_name.is_empty() || file_name.chars().any(char::is_control) {
        return Err(RestoreError::UnsafePath(
            "memory state path has an unsafe final component".into(),
        ));
    }
    let lock_path = parent.join(format!(".{file_name}.lock"));
    if lock_path.try_exists()? {
        ensure_private_regular_file(&lock_path)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(lock_path)?;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result != 0 {
        return Err(RestoreError::Integrity(
            "memory state is already locked by another GreenBubbles process".into(),
        ));
    }
    Ok(StateLock { file })
}

fn load_or_initialize_state(
    state_path: &Path,
    corpus: &LoadedCorpus,
) -> Result<MemoryRunState, RestoreError> {
    if state_path.try_exists()? {
        return load_existing_state(state_path, corpus);
    }
    let now = now_unix_milliseconds()?;
    let state = MemoryRunState {
        schema: PERSONAL_MEMORY_STATE_SCHEMA.into(),
        format_version: PERSONAL_MEMORY_FORMAT_VERSION,
        corpus_manifest_sha256: corpus.manifest_sha256.clone(),
        next_unit_index: 0,
        outstanding: None,
        last_committed: None,
        committed_wiki: None,
        created_at_unix_milliseconds: now,
        updated_at_unix_milliseconds: now,
    };
    write_state_atomic(state_path, &state)?;
    Ok(state)
}

fn load_existing_state(
    state_path: &Path,
    corpus: &LoadedCorpus,
) -> Result<MemoryRunState, RestoreError> {
    let bytes = read_owner_file_limited(state_path, MAXIMUM_CONTROL_FILE_BYTES)?;
    let state: MemoryRunState = serde_json::from_slice(&bytes)?;
    if state.schema != PERSONAL_MEMORY_STATE_SCHEMA
        || state.format_version != PERSONAL_MEMORY_FORMAT_VERSION
        || state.corpus_manifest_sha256 != corpus.manifest_sha256
        || state.next_unit_index > corpus.unit_index.units.len()
        || state.created_at_unix_milliseconds > state.updated_at_unix_milliseconds
    {
        return Err(RestoreError::Integrity(
            "memory state does not belong to this immutable corpus or is inconsistent".into(),
        ));
    }
    if let Some(outstanding) = &state.outstanding {
        if outstanding.start_unit_index != state.next_unit_index
            || outstanding.start_unit_index >= outstanding.end_unit_index_exclusive
            || outstanding.end_unit_index_exclusive > corpus.unit_index.units.len()
            || outstanding.batch_id.is_empty()
            || outstanding.batch_id.len() > 128
        {
            return Err(RestoreError::Integrity(
                "memory state contains an invalid outstanding batch".into(),
            ));
        }
        for page in &outstanding.target_pages {
            validate_wiki_relative_path(page)?;
        }
        for alias in &outstanding.evidence_aliases {
            if !valid_evidence_alias(alias, corpus.manifest.evidence_count) {
                return Err(RestoreError::Integrity(
                    "memory state contains an invalid evidence alias".into(),
                ));
            }
        }
        for (index, page) in outstanding.delivery_pages.iter().enumerate() {
            if page.number != index.saturating_add(1)
                || page.page_token.is_empty()
                || page.page_token.len() > 128
                || !valid_sha256(&page.page_sha256)
                || page.message_count == 0
                || page.evidence_aliases.len() != page.message_count
                || page
                    .evidence_aliases
                    .iter()
                    .any(|alias| !valid_evidence_alias(alias, corpus.manifest.evidence_count))
            {
                return Err(RestoreError::Integrity(
                    "memory state contains an invalid deterministic delivery page".into(),
                ));
            }
        }
    }
    validate_snapshot_paths(state.committed_wiki.as_ref())?;
    if let Some(outstanding) = &state.outstanding {
        validate_snapshot_paths(outstanding.wiki_before.as_ref())?;
    }
    Ok(state)
}

fn validate_snapshot_paths(
    snapshot: Option<&BTreeMap<String, WikiFileSnapshot>>,
) -> Result<(), RestoreError> {
    if let Some(snapshot) = snapshot {
        if snapshot.len() > MAXIMUM_WIKI_ENTRIES {
            return Err(RestoreError::Integrity(
                "memory state wiki inventory exceeds the fixed safety limit".into(),
            ));
        }
        for (path, record) in snapshot {
            validate_wiki_relative_path(path)?;
            if !valid_sha256(&record.sha256) {
                return Err(RestoreError::Integrity(
                    "memory state contains an invalid wiki file hash".into(),
                ));
            }
        }
    }
    Ok(())
}

fn write_state_atomic(path: &Path, state: &MemoryRunState) -> Result<(), RestoreError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    ensure_private_directory(parent)?;
    if path.try_exists()? {
        ensure_private_regular_file(path)?;
    }
    let mut bytes = serde_json::to_vec_pretty(state)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAXIMUM_CONTROL_FILE_BYTES {
        return Err(RestoreError::Integrity(
            "memory state exceeds the fixed control-file safety limit".into(),
        ));
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RestoreError::Integrity("system clock predates the Unix epoch".into()))?
        .as_nanos();
    let temporary = parent.join(format!(
        ".greenbubbles-memory-state-{}-{nonce}.tmp",
        std::process::id()
    ));
    let result = (|| -> Result<(), RestoreError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() && temporary.try_exists().unwrap_or(false) {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn read_owner_file_limited(path: &Path, maximum_bytes: u64) -> Result<Vec<u8>, RestoreError> {
    ensure_private_regular_file(path)?;
    let metadata = fs::metadata(path)?;
    if metadata.len() > maximum_bytes {
        return Err(RestoreError::Integrity(format!(
            "private control file exceeds the fixed {maximum_bytes}-byte safety limit"
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum_bytes {
        return Err(RestoreError::Integrity(format!(
            "private control file exceeds the fixed {maximum_bytes}-byte safety limit"
        )));
    }
    Ok(bytes)
}

fn read_immutable_owner_file_limited(
    path: &Path,
    maximum_bytes: u64,
) -> Result<Vec<u8>, RestoreError> {
    let metadata = immutable_owner_file_metadata(path)?;
    if metadata.len() > maximum_bytes {
        return Err(RestoreError::Integrity(format!(
            "immutable corpus file exceeds the fixed {maximum_bytes}-byte safety limit"
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum_bytes {
        return Err(RestoreError::Integrity(format!(
            "immutable corpus file exceeds the fixed {maximum_bytes}-byte safety limit"
        )));
    }
    Ok(bytes)
}

fn immutable_owner_file_metadata(path: &Path) -> Result<fs::Metadata, RestoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o777 != 0o400
        || metadata.nlink() != 1
    {
        return Err(RestoreError::Integrity(
            "corpus files must be current-user-owned, immutable 0400 regular files".into(),
        ));
    }
    Ok(metadata)
}

fn protect_immutable_corpus_tree(root: &Path) -> Result<(), RestoreError> {
    for entry in walkdir::WalkDir::new(root)
        .contents_first(true)
        .follow_links(false)
    {
        let entry = entry.map_err(|_| {
            RestoreError::Integrity("prepared corpus tree could not be finalized safely".into())
        })?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(RestoreError::Integrity(
                "prepared corpus tree contains an unsafe entry".into(),
            ));
        }
        if metadata.is_file() {
            if metadata.nlink() != 1 {
                return Err(RestoreError::Integrity(
                    "prepared corpus tree contains a multiply linked file".into(),
                ));
            }
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o400))?;
        } else if metadata.is_dir() {
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o500))?;
        } else {
            return Err(RestoreError::Integrity(
                "prepared corpus tree contains a non-file, non-directory entry".into(),
            ));
        }
    }
    Ok(())
}

fn scan_wiki(wiki_directory: &Path) -> Result<BTreeMap<String, WikiFileSnapshot>, RestoreError> {
    ensure_private_directory(wiki_directory)?;
    let root = fs::canonicalize(wiki_directory)?;
    let mut snapshot = BTreeMap::new();
    let mut entries = 0usize;
    for entry in walkdir::WalkDir::new(&root).follow_links(false) {
        let entry = entry.map_err(|_| {
            RestoreError::Integrity("wiki tree could not be traversed safely".into())
        })?;
        entries = entries.saturating_add(1);
        if entries > MAXIMUM_WIKI_ENTRIES {
            return Err(RestoreError::Integrity(format!(
                "wiki tree exceeds the fixed {MAXIMUM_WIKI_ENTRIES}-entry safety limit"
            )));
        }
        if entry.path() == root {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(RestoreError::Integrity(
                "wiki entries must be current-user-owned and may not be symbolic links".into(),
            ));
        }
        if metadata.is_dir() {
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(RestoreError::Integrity(
                    "wiki directories must be owner-only".into(),
                ));
            }
            continue;
        }
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(RestoreError::Integrity(
                "wiki files must be owner-only, singly linked regular files".into(),
            ));
        }
        let relative = entry
            .path()
            .strip_prefix(&root)
            .map_err(|_| RestoreError::UnsafePath("wiki entry escaped its selected root".into()))?;
        let relative = relative
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        validate_wiki_relative_path(&relative)?;
        if entry.path().extension().and_then(|value| value.to_str()) != Some("md") {
            return Err(RestoreError::Integrity(
                "wiki may contain only owner-only Markdown files and directories".into(),
            ));
        }
        if metadata.len() > MAXIMUM_WIKI_FILE_BYTES {
            return Err(RestoreError::Integrity(format!(
                "wiki Markdown file exceeds the fixed {MAXIMUM_WIKI_FILE_BYTES}-byte safety limit"
            )));
        }
        let bytes = read_owner_file_limited(entry.path(), MAXIMUM_WIKI_FILE_BYTES)?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| RestoreError::Integrity("wiki Markdown must be valid UTF-8".into()))?;
        snapshot.insert(
            relative,
            WikiFileSnapshot {
                sha256: sha256_bytes(&bytes),
                citations: extract_evidence_aliases(text),
                has_prose: markdown_has_prose(text),
                uncited_prose_line_count: markdown_uncited_prose_line_count(text),
                excessive_citation_prose_line_count: markdown_excessive_citation_prose_line_count(
                    text,
                ),
                prose_line_citations: markdown_prose_line_citations(text),
            },
        );
    }
    Ok(snapshot)
}

fn validate_wiki_commit(
    current: &BTreeMap<String, WikiFileSnapshot>,
    before: Option<&BTreeMap<String, WikiFileSnapshot>>,
    target_pages: &[String],
    batch_aliases: &[String],
    evidence_count: u64,
) -> Result<Vec<String>, RestoreError> {
    let target_pages = target_pages.iter().cloned().collect::<BTreeSet<_>>();
    let batch_aliases = batch_aliases.iter().cloned().collect::<BTreeSet<_>>();
    let mut changed = Vec::new();
    let mut changed_factual_page_count = 0usize;
    let mut current_batch_cited = false;
    if let Some(before) = before {
        for path in before.keys() {
            if !current.contains_key(path) {
                return Err(RestoreError::Integrity(format!(
                    "memory commit may not delete wiki page {path}"
                )));
            }
        }
    }
    for (path, record) in current {
        let previous_record = before.and_then(|before| before.get(path));
        let changed_file = before
            .and_then(|before| before.get(path))
            .is_none_or(|previous| previous.sha256 != record.sha256);
        if changed_file {
            if !target_pages.contains(path) {
                return Err(RestoreError::Integrity(format!(
                    "memory commit changed non-target wiki page {path}"
                )));
            }
            changed.push(path.clone());
            if path != "index.md" {
                if !record.has_prose {
                    return Err(RestoreError::Integrity(format!(
                        "changed factual wiki page {path} has no prose; empty or heading-only placeholders cannot advance memory"
                    )));
                }
                if record.citations.is_empty() {
                    return Err(RestoreError::Integrity(format!(
                        "changed factual wiki page {path} has no evidence alias"
                    )));
                }
                if record.uncited_prose_line_count > 0 {
                    return Err(RestoreError::Integrity(format!(
                        "changed factual wiki page {path} contains {} uncited prose line(s)",
                        record.uncited_prose_line_count
                    )));
                }
                if record.excessive_citation_prose_line_count > 0 {
                    return Err(RestoreError::Integrity(format!(
                        "changed factual wiki page {path} contains {} prose line(s) with more than {MAXIMUM_WIKI_CITATIONS_PER_PROSE_LINE} citations; retain a representative evidence set instead of citation dumping",
                        record.excessive_citation_prose_line_count
                    )));
                }
                changed_factual_page_count = changed_factual_page_count.saturating_add(1);
            }
        }
        for alias in &record.citations {
            if !valid_evidence_alias(alias, evidence_count) {
                return Err(RestoreError::Integrity(format!(
                    "wiki page {path} cites an unknown evidence alias"
                )));
            }
            if changed_file
                && !batch_aliases.contains(alias)
                && previous_record.is_none_or(|previous| !previous.citations.contains(alias))
            {
                return Err(RestoreError::Integrity(format!(
                    "wiki page {path} introduced evidence not present in this batch or that same prior page"
                )));
            }
            if changed_file && path != "index.md" && batch_aliases.contains(alias) {
                current_batch_cited = true;
            }
        }
    }
    let cited_aliases = current
        .values()
        .flat_map(|record| record.citations.iter().cloned())
        .collect::<BTreeSet<_>>();
    let retained_but_uncited_count = batch_aliases.difference(&cited_aliases).count();
    if retained_but_uncited_count > 0 {
        return Err(RestoreError::Integrity(format!(
            "memory commit has {retained_but_uncited_count} retained evidence alias(es) that are not cited in the durable wiki"
        )));
    }
    if changed_factual_page_count == 0 || !current_batch_cited {
        return Err(RestoreError::Integrity(
            "memory commit requires at least one changed non-index page with prose and an evidence alias from the outstanding batch"
                .into(),
        ));
    }
    changed.sort();
    Ok(changed)
}

fn validate_me_self_attribution(
    me: &WikiFileSnapshot,
    self_evidence_aliases: &BTreeSet<String>,
) -> Result<(), RestoreError> {
    let incoming_only_line_count = me
        .prose_line_citations
        .iter()
        .filter(|citations| citations.is_disjoint(self_evidence_aliases))
        .count();
    if incoming_only_line_count > 0 {
        return Err(RestoreError::Integrity(format!(
            "changed account-holder wiki page contains {incoming_only_line_count} factual prose line(s) without a self-authored citation; incoming-only facts belong on a person page or must be explicitly supported by account-holder evidence"
        )));
    }
    Ok(())
}

fn validate_reviewed_no_durable_memory_commit(
    current: &BTreeMap<String, WikiFileSnapshot>,
    before: Option<&BTreeMap<String, WikiFileSnapshot>>,
    evidence_count: u64,
) -> Result<(), RestoreError> {
    let before = before.ok_or_else(|| {
        RestoreError::Integrity(
            "reviewed-no-durable-memory commit requires the wiki snapshot captured by memory next"
                .into(),
        )
    })?;
    if !wiki_snapshots_same_bytes(current, before) {
        return Err(RestoreError::Integrity(
            "reviewed-no-durable-memory commit requires a byte-for-byte unchanged wiki".into(),
        ));
    }
    for (path, record) in current {
        for alias in &record.citations {
            if !valid_evidence_alias(alias, evidence_count) {
                return Err(RestoreError::Integrity(format!(
                    "wiki page {path} cites an unknown evidence alias"
                )));
            }
        }
    }
    Ok(())
}

fn wiki_snapshots_same_bytes(
    left: &BTreeMap<String, WikiFileSnapshot>,
    right: &BTreeMap<String, WikiFileSnapshot>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(path, record)| {
            right
                .get(path)
                .is_some_and(|other| record.sha256 == other.sha256)
        })
}

fn extract_evidence_aliases(text: &str) -> BTreeSet<String> {
    let bytes = text.as_bytes();
    let mut aliases = BTreeSet::new();
    let mut index = 0usize;
    while index.saturating_add(12) <= bytes.len() {
        if bytes[index] != b'[' || bytes[index.saturating_add(1)] != b'E' {
            index = index.saturating_add(1);
            continue;
        }
        let alias_start = index.saturating_add(1);
        let alias_end = alias_start.saturating_add(10);
        if bytes[alias_start.saturating_add(1)..alias_end]
            .iter()
            .all(u8::is_ascii_digit)
            && bytes[alias_end] == b']'
        {
            aliases.insert(text[alias_start..alias_end].to_string());
            index = alias_end.saturating_add(1);
        } else {
            index = index.saturating_add(1);
        }
    }
    aliases
}

fn markdown_has_prose(text: &str) -> bool {
    text.lines().any(markdown_line_is_prose)
}

fn markdown_uncited_prose_line_count(text: &str) -> usize {
    text.lines()
        .filter(|line| markdown_line_is_prose(line))
        .filter(|line| extract_evidence_aliases(line).is_empty())
        .count()
}

fn markdown_excessive_citation_prose_line_count(text: &str) -> usize {
    text.lines()
        .filter(|line| markdown_line_is_prose(line))
        .filter(|line| extract_evidence_aliases(line).len() > MAXIMUM_WIKI_CITATIONS_PER_PROSE_LINE)
        .count()
}

fn markdown_prose_line_citations(text: &str) -> Vec<BTreeSet<String>> {
    text.lines()
        .filter(|line| markdown_line_is_prose(line))
        .map(extract_evidence_aliases)
        .collect()
}

fn markdown_line_is_prose(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty()
        && !line.starts_with('#')
        && line != "---"
        && !line.starts_with("<!--")
        && !line.starts_with("[//]:")
}

fn valid_evidence_alias(alias: &str, evidence_count: u64) -> bool {
    alias.len() == 10
        && alias.starts_with('E')
        && alias[1..].bytes().all(|byte| byte.is_ascii_digit())
        && alias[1..]
            .parse::<u64>()
            .is_ok_and(|number| number > 0 && number <= evidence_count)
}

fn validate_wiki_relative_path(value: &str) -> Result<(), RestoreError> {
    let path = Path::new(value);
    if path.is_absolute()
        || path.as_os_str().is_empty()
        || path.extension().and_then(|extension| extension.to_str()) != Some("md")
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(RestoreError::UnsafePath(
            "wiki target path must be a safe relative Markdown path".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod personal_memory_tests {
    use super::{
        extract_evidence_aliases, order_unit_drafts, validate_me_self_attribution,
        validate_reviewed_no_durable_memory_commit, validate_wiki_commit, MemoryDeliveryOrder,
        UnitDraft, WikiFileSnapshot,
    };
    use crate::live_query::{CorpusHydratedMessage, CorpusMessageLocation};
    use crate::ConversationKind;
    use std::collections::{BTreeMap, BTreeSet};

    fn snapshot(hash: &str, aliases: &[&str], has_prose: bool) -> WikiFileSnapshot {
        WikiFileSnapshot {
            sha256: hash.to_string(),
            citations: aliases.iter().map(|alias| (*alias).to_string()).collect(),
            has_prose,
            uncited_prose_line_count: 0,
            excessive_citation_prose_line_count: 0,
            prose_line_citations: Vec::new(),
        }
    }

    fn unit(conversation: &str, month: &str, timestamp: i64) -> UnitDraft {
        UnitDraft {
            conversation_alias: conversation.to_string(),
            conversation_source_id: format!("source-{conversation}"),
            conversation_label: format!("Conversation {conversation}"),
            conversation_kind: ConversationKind::Direct,
            month: month.to_string(),
            messages: vec![CorpusHydratedMessage {
                location: CorpusMessageLocation {
                    sort_sequence: timestamp,
                    create_time: timestamp,
                    server_id: timestamp,
                    shard_id: 1,
                    row_id: timestamp,
                },
                canonical_id: format!("message-{conversation}-{timestamp}"),
                sender: None,
                sender_display_name: None,
                is_account_holder: Some(true),
                message_type: 1,
                message_subtype: 0,
                payload_kind: "Text".to_string(),
                text: Some("self-authored evidence".to_string()),
                text_truncated: false,
                content_decode_failed: false,
            }],
        }
    }

    #[test]
    fn account_holder_relevance_order_is_weighted_broad_and_complete() {
        let mut units = vec![unit("B", "2023-01", 50), unit("C", "2025-01", 900)];
        for index in 1..=8 {
            units.push(unit(
                "A",
                &format!("2024-{index:02}"),
                i64::from(index) * 100,
            ));
        }
        let chronological = order_unit_drafts(units.clone(), MemoryDeliveryOrder::Chronological);
        assert_eq!(chronological[0].conversation_alias, "B");

        let ordered = order_unit_drafts(units, MemoryDeliveryOrder::AccountHolderRelevance);
        assert_eq!(ordered.len(), 10);
        assert!(ordered[..4]
            .iter()
            .all(|unit| unit.conversation_alias == "A"));
        assert_eq!(
            ordered[..4]
                .iter()
                .map(|unit| unit.month.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            4
        );
        assert_eq!(ordered[4].conversation_alias, "C");
        assert_eq!(ordered[5].conversation_alias, "B");
        assert_eq!(
            ordered
                .iter()
                .map(|unit| unit.messages[0].canonical_id.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            ordered.len()
        );
    }

    #[test]
    fn evidence_extraction_requires_exact_bracketed_citations() {
        let aliases = extract_evidence_aliases(
            "kept [E000000001], also [E000000002]. Ignore E000000003, [E0000000040], and [E000000005",
        );
        assert_eq!(
            aliases,
            BTreeSet::from(["E000000001".to_string(), "E000000002".to_string()])
        );
    }

    #[test]
    fn changed_pages_cannot_launder_another_pages_prior_citation() {
        let before = BTreeMap::from([
            (
                "me.md".to_string(),
                snapshot("me-before", &["E000000001"], true),
            ),
            (
                "people/P000001.md".to_string(),
                snapshot("person-before", &["E000000002"], true),
            ),
        ]);
        let current = BTreeMap::from([
            (
                "me.md".to_string(),
                snapshot("me-before", &["E000000001"], true),
            ),
            (
                "people/P000001.md".to_string(),
                snapshot("person-after", &["E000000001", "E000000003"], true),
            ),
        ]);
        assert!(validate_wiki_commit(
            &current,
            Some(&before),
            &["people/P000001.md".to_string()],
            &["E000000003".to_string()],
            3,
        )
        .is_err());

        let valid_current = BTreeMap::from([
            (
                "me.md".to_string(),
                snapshot("me-before", &["E000000001"], true),
            ),
            (
                "people/P000001.md".to_string(),
                snapshot("person-after", &["E000000002", "E000000003"], true),
            ),
        ]);
        assert_eq!(
            validate_wiki_commit(
                &valid_current,
                Some(&before),
                &["people/P000001.md".to_string()],
                &["E000000003".to_string()],
                3,
            )
            .unwrap(),
            vec!["people/P000001.md"]
        );
    }

    #[test]
    fn index_only_new_evidence_cannot_justify_a_factual_page_change() {
        let before = BTreeMap::from([
            ("index.md".to_string(), snapshot("index-before", &[], false)),
            (
                "me.md".to_string(),
                snapshot("me-before", &["E000000001"], true),
            ),
        ]);
        let current = BTreeMap::from([
            (
                "index.md".to_string(),
                snapshot("index-after", &["E000000002"], true),
            ),
            (
                "me.md".to_string(),
                snapshot("me-after", &["E000000001"], true),
            ),
        ]);
        assert!(validate_wiki_commit(
            &current,
            Some(&before),
            &["index.md".to_string(), "me.md".to_string()],
            &["E000000002".to_string()],
            2,
        )
        .is_err());
    }

    #[test]
    fn every_retained_alias_must_reach_the_durable_wiki() {
        let current = BTreeMap::from([(
            "me.md".to_string(),
            snapshot("me-after", &["E000000001"], true),
        )]);
        assert!(validate_wiki_commit(
            &current,
            None,
            &["me.md".to_string()],
            &["E000000001".to_string(), "E000000002".to_string()],
            2,
        )
        .is_err());
    }

    #[test]
    fn changed_prose_rejects_citation_dumping() {
        let aliases = (1..=9)
            .map(|number| format!("E{number:09}"))
            .collect::<Vec<_>>();
        let current = BTreeMap::from([(
            "me.md".to_string(),
            WikiFileSnapshot {
                sha256: "me-after".to_string(),
                citations: aliases.iter().cloned().collect(),
                has_prose: true,
                uncited_prose_line_count: 0,
                excessive_citation_prose_line_count: 1,
                prose_line_citations: vec![aliases.iter().cloned().collect()],
            },
        )]);
        assert!(
            validate_wiki_commit(&current, None, &["me.md".to_string()], &aliases, 9,).is_err()
        );
    }

    #[test]
    fn account_holder_prose_requires_self_authored_support() {
        let mut me = snapshot("me-after", &["E000000001"], true);
        me.prose_line_citations = vec![BTreeSet::from(["E000000001".to_string()])];
        assert!(validate_me_self_attribution(&me, &BTreeSet::new()).is_err());
        assert!(
            validate_me_self_attribution(&me, &BTreeSet::from(["E000000001".to_string()]),).is_ok()
        );

        me.prose_line_citations = vec![BTreeSet::from([
            "E000000001".to_string(),
            "E000000002".to_string(),
        ])];
        assert!(
            validate_me_self_attribution(&me, &BTreeSet::from(["E000000002".to_string()]),).is_ok()
        );
    }

    #[test]
    fn reviewed_no_memory_still_rejects_unknown_existing_citations() {
        let wiki = BTreeMap::from([("me.md".to_string(), snapshot("same", &["E999999999"], true))]);
        assert!(validate_reviewed_no_durable_memory_commit(&wiki, Some(&wiki), 3).is_err());
    }
}
