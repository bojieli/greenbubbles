use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

use chrono::{DateTime, Datelike, SecondsFormat, TimeZone};
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

pub const PERSONAL_MEMORY_POLICY_SCHEMA: &str = "greenbubbles.personal-memory-selection-policy.v2";
pub const LEGACY_PERSONAL_MEMORY_POLICY_SCHEMA: &str =
    "greenbubbles.personal-memory-selection-policy.v1";
pub const PERSONAL_MEMORY_POLICY_FORMAT_VERSION: u32 = 2;
pub const PERSONAL_MEMORY_CORPUS_SCHEMA: &str = "greenbubbles.personal-memory-corpus.v1";
pub const PERSONAL_MEMORY_BATCH_SCHEMA: &str = "greenbubbles.personal-memory-batch.v1";
pub const PERSONAL_MEMORY_PAGE_SCHEMA: &str = "greenbubbles.personal-memory-page.v1";
pub const PERSONAL_MEMORY_STATE_SCHEMA: &str = "greenbubbles.personal-memory-state.v1";
pub const PERSONAL_MEMORY_FORMAT_VERSION: u32 = 1;
pub const PERSONAL_MEMORY_CURRENT_SELECTOR: &str = "current";

const MAXIMUM_POLICY_BYTES: u64 = 1024 * 1024;
const MAXIMUM_SCOPE_SELECTORS: usize = 10_000;
const MAXIMUM_SCOPE_SELECTOR_BYTES: usize = 512;
const MAXIMUM_CONTROL_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// A canonical million-message corpus can legitimately have well over 100,000
/// immutable units. Keep its verified index separate from the much smaller run
/// state/control-file bound. New v2 indexes omit per-message aliases and repeated
/// target paths, but this larger ceiling also keeps existing v1 corpora readable.
const MAXIMUM_UNIT_INDEX_BYTES: u64 = 512 * 1024 * 1024;
const MAXIMUM_WIKI_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAXIMUM_WIKI_ENTRIES: usize = 100_000;
const MAXIMUM_WIKI_CITATIONS_PER_PROSE_LINE: usize = 8;
const MAXIMUM_REPORTED_WIKI_PROBLEMS: usize = 20;
const MAXIMUM_REPORTED_WIKI_LOCATIONS: usize = 12;
const MINIMUM_NEXT_TEXT_BYTES: usize = 16 * 1024;
const MAXIMUM_NEXT_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAXIMUM_BATCH_MESSAGES: usize = 5_000;
/// No non-sticker payload in a million-message corpus comes close to this, so
/// it only bounds the pathological case rather than trimming ordinary chat.
const MAXIMUM_DELIVERED_MESSAGE_TEXT_BYTES: usize = 4096;
const DELIVERED_MARKUP_TEXT_ATTRIBUTES: [&str; 5] =
    ["label", "poiname", "title", "desc", "content"];
/// The corpus stores the account holder as "You"; a third-person wiki needs a
/// stable noun instead, and the raw source id made every agent title the
/// account-holder page with a wxid.
const ACCOUNT_HOLDER_DELIVERY_LABEL: &str = "Me";
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PersonalMemoryCorpusMode {
    /// Legacy v1 behavior: retain account-holder-active episodes and bounded context.
    #[default]
    AccountHolderActiveEpisodes,
    /// Canonical v2 behavior: retain every decodable row from every inventoried message table.
    AllMessages,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PersonalMemorySummarySubjectSelector {
    /// Build the account holder's personal wiki. This is intentionally not a sender filter.
    #[default]
    AccountHolder,
    /// Build memory about one other person selected by source ID or stable corpus alias.
    Person { selector: String },
    /// Build conversation-centric memory without choosing a person as the subject.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PersonalMemoryConversationKindSelector {
    Direct,
    Group,
    Official,
    Service,
}

impl PersonalMemoryConversationKindSelector {
    pub fn parse_cli(value: &str) -> Result<Self, RestoreError> {
        match value {
            "direct" => Ok(Self::Direct),
            "group" => Ok(Self::Group),
            "official" => Ok(Self::Official),
            "service" => Ok(Self::Service),
            _ => Err(RestoreError::Integrity(
                "memory --conversation-kind must be direct, group, official, or service".into(),
            )),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PersonalMemoryScopeOptions {
    /// Empty means every conversation in the canonical corpus.
    pub conversation_selectors: Vec<String>,
    /// Empty means every conversation kind. Values are ORed, then intersected
    /// with explicit conversation selectors and every other evidence filter.
    pub conversation_kinds: Vec<PersonalMemoryConversationKindSelector>,
    /// Inclusive RFC 3339 lower bound with an explicit offset or `Z`.
    pub from: Option<String>,
    /// Inclusive RFC 3339 upper bound with an explicit offset or `Z`.
    pub through: Option<String>,
    /// Empty means messages from every sender. `self` and `accountHolder` are accepted aliases.
    pub sender_selectors: Vec<String>,
    /// Defaults to the authenticated account holder and does not narrow evidence coverage.
    pub summary_subject: PersonalMemorySummarySubjectSelector,
}

impl PersonalMemoryScopeOptions {
    fn validate_shape(&self) -> Result<(Option<i64>, Option<i64>), RestoreError> {
        for (label, selectors) in [
            ("--conversation", &self.conversation_selectors),
            ("--sender", &self.sender_selectors),
        ] {
            if selectors.len() > MAXIMUM_SCOPE_SELECTORS {
                return Err(RestoreError::Integrity(format!(
                    "memory {label} exceeds the fixed {MAXIMUM_SCOPE_SELECTORS}-selector limit"
                )));
            }
            let mut unique = BTreeSet::new();
            if selectors.iter().any(|selector| {
                let trimmed = selector.trim();
                trimmed.is_empty()
                    || trimmed.len() > MAXIMUM_SCOPE_SELECTOR_BYTES
                    || !unique.insert(trimmed)
            }) {
                return Err(RestoreError::Integrity(format!(
                    "memory {label} contains an empty, duplicate, or oversized selector"
                )));
            }
        }
        if self.conversation_kinds.len() > MAXIMUM_SCOPE_SELECTORS
            || self
                .conversation_kinds
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len()
                != self.conversation_kinds.len()
        {
            return Err(RestoreError::Integrity(
                "memory --conversation-kind contains a duplicate or exceeds the fixed selector limit"
                    .into(),
            ));
        }
        if let PersonalMemorySummarySubjectSelector::Person { selector } = &self.summary_subject {
            let selector = selector.trim();
            if selector.is_empty() || selector.len() > MAXIMUM_SCOPE_SELECTOR_BYTES {
                return Err(RestoreError::Integrity(
                    "memory --subject person selector is empty or oversized".into(),
                ));
            }
        }
        let not_before_unix = self
            .from
            .as_deref()
            .map(|value| parse_rfc3339_scope_bound(value, "--from", true))
            .transpose()?;
        let not_after_unix = self
            .through
            .as_deref()
            .map(|value| parse_rfc3339_scope_bound(value, "--through", false))
            .transpose()?;
        if not_before_unix
            .zip(not_after_unix)
            .is_some_and(|(start, end)| start > end)
        {
            return Err(RestoreError::Integrity(
                "memory RFC 3339 time range is inverted or contains no whole source second".into(),
            ));
        }
        Ok((not_before_unix, not_after_unix))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PersonalMemorySelectionPolicy {
    pub schema: String,
    pub format_version: u32,
    /// Required to be `allMessages` for v2 canonical corpora. Legacy v1 policies omit it.
    #[serde(default)]
    pub corpus_mode: PersonalMemoryCorpusMode,
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
    fn validate(&self) -> Result<(Tz, PersonalMemoryCorpusMode), RestoreError> {
        let corpus_mode = if self.schema == LEGACY_PERSONAL_MEMORY_POLICY_SCHEMA
            && self.format_version == PERSONAL_MEMORY_FORMAT_VERSION
        {
            if self.corpus_mode != PersonalMemoryCorpusMode::AccountHolderActiveEpisodes {
                return Err(RestoreError::Integrity(
                    "legacy personal-memory policies cannot request a v2 corpus mode".into(),
                ));
            }
            PersonalMemoryCorpusMode::AccountHolderActiveEpisodes
        } else if self.schema == PERSONAL_MEMORY_POLICY_SCHEMA
            && self.format_version == PERSONAL_MEMORY_POLICY_FORMAT_VERSION
        {
            if self.corpus_mode != PersonalMemoryCorpusMode::AllMessages {
                return Err(RestoreError::Integrity(
                    "v2 personal-memory policies must use corpusMode=allMessages".into(),
                ));
            }
            PersonalMemoryCorpusMode::AllMessages
        } else {
            return Err(RestoreError::Integrity(
                "personal-memory selection policy schema or format version is unsupported".into(),
            ));
        };
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
        if corpus_mode == PersonalMemoryCorpusMode::AllMessages
            && (self.not_before_unix.is_some()
                || self.not_after_unix.is_some()
                || !self.include_direct_conversations
                || !self.include_group_conversations
                || !self.include_official_accounts
                || !self.include_service_accounts)
        {
            return Err(RestoreError::Integrity(
                "v2 canonical corpus preparation may not pre-filter time or conversation kinds; apply evidence filters through a run scope"
                    .into(),
            ));
        }
        Ok((timezone, corpus_mode))
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
    /// Missing in legacy corpora, which retained only account-holder-active episodes.
    #[serde(default)]
    pub corpus_mode: PersonalMemoryCorpusMode,
    pub timezone: String,
    /// Missing in legacy format-1 corpora, which were always chronological.
    #[serde(default)]
    pub delivery_order: MemoryDeliveryOrder,
    pub reference_unix: i64,
    pub account_holder_attribution_bound: bool,
    pub content_trust: String,
    pub immutable_index: bool,
    pub source_coverage_complete: bool,
    /// True when every inventoried hashed message table was scanned, including
    /// tables whose conversation identity could not be reversed.
    #[serde(default)]
    pub row_coverage_complete: bool,
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
    /// Present only on corpora produced by `memory prepare --extend`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<CorpusGenerationLink>,
}

/// Chain-of-custody link written into the manifest of an extended corpus.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CorpusGenerationLink {
    #[serde(rename = "baseManifestSHA256")]
    pub base_manifest_sha256: String,
    pub generation: u32,
    pub first_new_unit_index: usize,
    pub carried_unit_count: usize,
    pub carried_message_count: u64,
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
    /// True when every inventoried hashed message table contributed metadata rows.
    #[serde(default)]
    pub row_coverage_complete: bool,
    pub content_complete: bool,
    pub limitation_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContactSidecarRecord {
    alias: Option<String>,
    source_id: String,
    display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nickname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wechat_alias: Option<String>,
    kind: ContactKind,
    is_account_holder: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConversationSidecarRecord {
    alias: String,
    source_id: String,
    display_name: String,
    kind: ConversationKind,
    contact_kind: ContactKind,
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
struct DeliveryMessage {
    e: String,
    a: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    p: Option<String>,
    /// RFC 3339 in the corpus timezone. Integer seconds remain private to the
    /// immutable corpus and state so the model never has to decode Unix time.
    t: String,
    k: String,
    x: String,
    #[serde(default, skip_serializing_if = "is_false")]
    tr: bool,
}

#[derive(Debug, Clone, Serialize)]
struct DeliveryEpisode {
    /// Stable prepared-unit alias. A unit may continue on the next delivery page.
    u: String,
    c: String,
    month: String,
    from: String,
    to: String,
    /// Zero-based message offset within the immutable prepared unit.
    o: usize,
    /// Total message count in the immutable prepared unit.
    n: usize,
    m: Vec<DeliveryMessage>,
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
struct DeliveryPersonIdentity {
    source_id: String,
    display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    remark: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wechat_alias: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeliveryConversationIdentity {
    source_id: String,
    title: String,
    kind: ConversationKind,
}

struct DeliveryIdentityIndex {
    account_holder: DeliveryPersonIdentity,
    people: BTreeMap<String, DeliveryPersonIdentity>,
    conversations: BTreeMap<String, DeliveryConversationIdentity>,
}

struct DeliveryPageContext<'a> {
    scope: &'a ResolvedPersonalMemoryScope,
    timezone: Tz,
    direct_people_by_unit: &'a BTreeMap<String, BTreeSet<String>>,
    identities: &'a DeliveryIdentityIndex,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeliveryPagePayload {
    schema: &'static str,
    format_version: u32,
    batch_id: String,
    content_trust: &'static str,
    page: DeliveryPagePosition,
    scope: MemoryScopeOutput,
    target_pages: Vec<String>,
    account_holder: DeliveryPersonIdentity,
    people: BTreeMap<String, DeliveryPersonIdentity>,
    conversations: BTreeMap<String, DeliveryConversationIdentity>,
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
    /// Legacy v1 fields. V2 derives evidence aliases from one ordinal and stores
    /// only person aliases rather than repeating full Markdown paths per unit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    target_pages: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    evidence_aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    person_aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sender_aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    has_account_holder_sender: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    has_unknown_sender: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_evidence_ordinal: Option<u64>,
    /// Added for canonical corpora so scopes can be planned without model-facing IDs.
    #[serde(default)]
    conversation: String,
    /// Private source ID used only to resolve an operator's scope selector.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    conversation_id: String,
    #[serde(default)]
    from: i64,
    #[serde(default)]
    to: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnitIndex {
    schema: String,
    format_version: u32,
    units: Vec<UnitIndexEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum ResolvedMemorySummarySubject {
    #[default]
    AccountHolder,
    Person {
        alias: String,
    },
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolvedPersonalMemoryScope {
    conversation_aliases: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    conversation_kinds: BTreeSet<PersonalMemoryConversationKindSelector>,
    not_before_unix: Option<i64>,
    not_after_unix: Option<i64>,
    sender_aliases: BTreeSet<String>,
    include_account_holder_sender: bool,
    include_unknown_sender: bool,
    summary_subject: ResolvedMemorySummarySubject,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScopedUnitSelection {
    corpus_unit_index: usize,
    /// True means every message in the immutable unit is selected and the bitmap stays absent.
    all_messages: bool,
    /// Hex-encoded least-significant-bit-first selection bitmap for a partial unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message_bitmap: Option<String>,
    message_count: usize,
    text_byte_count: usize,
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
    let (timezone, corpus_mode) = policy.validate()?;
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

        if !conversation_enabled(conversation, &policy, corpus_mode) {
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

        let active_months = if corpus_mode == PersonalMemoryCorpusMode::AllMessages {
            month_indices.keys().cloned().collect::<BTreeSet<_>>()
        } else {
            month_indices
                .iter()
                .filter_map(|(month, indices)| {
                    let self_count = indices
                        .iter()
                        .filter(|index| messages[**index].is_account_holder == Some(true))
                        .count();
                    (self_count >= policy.minimum_self_messages_per_active_month)
                        .then_some(month.clone())
                })
                .collect::<BTreeSet<_>>()
        };
        let recent_active_count = active_months
            .iter()
            .filter(|month| {
                recent_start_ordinal.is_some_and(|start| {
                    month.ordinal >= start && month.ordinal <= reference_month.ordinal
                })
            })
            .count();
        let recent_conversation_eligible = corpus_mode == PersonalMemoryCorpusMode::AllMessages
            || policy.minimum_self_active_months_in_lookback == 0
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
                if corpus_mode == PersonalMemoryCorpusMode::AllMessages {
                    let locations = session
                        .iter()
                        .map(|index| {
                            selected_in_month.insert(*index);
                            messages[*index].location.clone()
                        })
                        .collect::<Vec<_>>();
                    if !locations.is_empty() {
                        episode_drafts.push(EpisodeDraft {
                            conversation: conversation.clone(),
                            month: month.label.clone(),
                            locations,
                        });
                    }
                    continue;
                }
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
            remark: contact.remark.clone(),
            nickname: contact.nickname.clone(),
            wechat_alias: contact.alias.clone(),
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
                remark: None,
                nickname: None,
                wechat_alias: None,
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

    let conversations_path = staging.path().join("conversations.jsonl");
    let conversation_records = inventory
        .conversations
        .iter()
        .map(|conversation| {
            let alias = conversation_aliases
                .get(&conversation.source_id)
                .cloned()
                .ok_or_else(|| {
                    RestoreError::Integrity(
                        "canonical corpus conversation alias is unavailable".into(),
                    )
                })?;
            let display_name = model_safe_conversation_label(conversation, &alias);
            Ok(ConversationSidecarRecord {
                alias,
                source_id: conversation.source_id.clone(),
                display_name,
                kind: conversation.kind,
                contact_kind: conversation.contact_kind,
            })
        })
        .collect::<Result<Vec<_>, RestoreError>>()?;
    files.push(write_json_lines(
        &conversations_path,
        "conversations.jsonl",
        conversation_records.iter(),
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
        let first_evidence_ordinal = evidence_count.saturating_add(1);
        let mut unit_person_aliases = BTreeSet::new();
        let mut unit_sender_aliases = BTreeSet::new();
        let mut has_account_holder_sender = false;
        let mut has_unknown_sender = false;
        if let Some(person_alias) = person_aliases.get(&unit.conversation_source_id) {
            unit_person_aliases.insert(person_alias.clone());
        }
        let mut text_byte_count = 0usize;
        for message in unit.messages {
            evidence_count = evidence_count.saturating_add(1);
            let evidence_alias = format!("E{evidence_count:09}");
            let actor = match message.is_account_holder {
                Some(true) => {
                    has_account_holder_sender = true;
                    "self"
                }
                Some(false) => "other",
                None => {
                    has_unknown_sender = true;
                    "unknown"
                }
            }
            .to_string();
            let person_alias = message
                .sender
                .as_ref()
                .and_then(|sender| person_aliases.get(sender).cloned());
            if let Some(person_alias) = &person_alias {
                unit_person_aliases.insert(person_alias.clone());
                unit_sender_aliases.insert(person_alias.clone());
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
        }
        let from = compact_messages
            .first()
            .map(|message| message.t)
            .unwrap_or_default();
        let to = compact_messages
            .last()
            .map(|message| message.t)
            .unwrap_or_default();
        let conversation_alias = unit.conversation_alias;
        let prepared = PreparedUnitFile {
            schema: PERSONAL_MEMORY_BATCH_SCHEMA.to_string(),
            id: unit_id.clone(),
            c: conversation_alias.clone(),
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
            message_count: prepared.m.len(),
            target_pages: Vec::new(),
            evidence_aliases: Vec::new(),
            person_aliases: unit_person_aliases.into_iter().collect(),
            sender_aliases: unit_sender_aliases.into_iter().collect(),
            has_account_holder_sender,
            has_unknown_sender,
            first_evidence_ordinal: Some(first_evidence_ordinal),
            conversation: conversation_alias,
            conversation_id: String::new(),
            from,
            to,
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
        schema: "greenbubbles.personal-memory-unit-index.v2".into(),
        format_version: 2,
        units: unit_index_entries,
    };
    let unit_index_record = write_json_compact(
        &batches_directory.join("index.json"),
        "batches/index.json",
        &unit_index,
    )?;
    if unit_index_record.byte_count > MAXIMUM_UNIT_INDEX_BYTES {
        return Err(RestoreError::Integrity(format!(
            "prepared unit index exceeds the fixed {MAXIMUM_UNIT_INDEX_BYTES}-byte safety limit"
        )));
    }
    files.push(unit_index_record);
    coverage.unit_count = unit_index.units.len() as u64;
    coverage.limitation_codes = limitation_codes.into_iter().collect();
    coverage.row_coverage_complete = coverage.metadata_decode_failure_count == 0
        && !coverage.limitation_codes.iter().any(|code| {
            matches!(
                code.as_str(),
                "messageTableInventoryUnavailable"
                    | "messageTableInventoryIncomplete"
                    | "corpusMetadataQueryFailed"
                    | "corpusMetadataRowFailed"
                    | "unsupportedCorpusMessageSchema"
                    | "shardUnavailable"
                    | "shardSchemaUnavailable"
            )
        });
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
        corpus_mode,
        timezone: policy.timezone,
        delivery_order: policy.delivery_order,
        reference_unix,
        account_holder_attribution_bound: true,
        content_trust: "untrustedChatEvidence".into(),
        immutable_index: true,
        source_coverage_complete: coverage.source_coverage_complete,
        row_coverage_complete: coverage.row_coverage_complete,
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
        extends: None,
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

/// Extend an existing corpus with new messages from the live source.
///
/// The base corpus must be fully hash-verified before any hydration begins.
/// Every base message must still be present in the live source; any gap is
/// a hard error directing the user to re-prepare from scratch.
pub fn prepare_personal_memory_corpus_extend_with_progress(
    base_corpus_directory: &Path,
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
    let (timezone, corpus_mode) = policy.validate()?;
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

    // --- Step 1: Load and fully hash-verify the base corpus. ---
    let base = load_corpus(base_corpus_directory)?;
    let base_generation = base
        .manifest
        .extends
        .as_ref()
        .map(|link| link.generation)
        .unwrap_or(0);

    progress(&PersonalMemoryProgress {
        phase: "baseCorpusVerification",
        completed_items: 0,
        total_items: base.manifest.unit_count,
        scanned_message_count: 0,
        selected_message_count: 0,
        hydrated_message_count: 0,
    });

    // Hash-verify every base corpus file (byte-level, beyond manifest byte counts).
    for record in &base.manifest.files {
        if record.relative_path == "evidence.jsonl" {
            // Verified below while building the canonical-id set.
            continue;
        }
        let path = safe_corpus_path(&base.root, &record.relative_path)?;
        let bytes = read_corpus_owner_file_limited(&path, MAXIMUM_UNIT_INDEX_BYTES)?;
        if sha256_bytes(&bytes) != record.sha256 {
            return Err(RestoreError::Integrity(format!(
                "base corpus file {} failed hash verification; re-prepare from scratch",
                record.relative_path
            )));
        }
    }

    // --- Step 2: Load base alias maps from sidecars. ---
    let mut base_person_aliases: BTreeMap<String, String> = BTreeMap::new();
    let mut base_person_names: BTreeMap<String, String> = BTreeMap::new();
    let mut max_person_number: usize = 0;
    {
        let contacts_record = base
            .manifest
            .files
            .iter()
            .find(|r| r.relative_path == "contacts.jsonl")
            .ok_or_else(|| RestoreError::Integrity("base corpus has no contacts sidecar".into()))?;
        let contacts_path = safe_corpus_path(&base.root, "contacts.jsonl")?;
        let contacts_bytes =
            read_corpus_owner_file_limited(&contacts_path, MAXIMUM_CONTROL_FILE_BYTES)?;
        if sha256_bytes(&contacts_bytes) != contacts_record.sha256 {
            return Err(RestoreError::Integrity(
                "base corpus contacts sidecar failed hash verification; re-prepare from scratch"
                    .into(),
            ));
        }
        for line in contacts_bytes.split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let record: ContactSidecarRecord = serde_json::from_slice(line)?;
            base_person_names
                .entry(record.source_id.clone())
                .or_insert_with(|| record.display_name.clone());
            if let Some(alias) = record.alias {
                if let Some(n) = alias
                    .strip_prefix('P')
                    .and_then(|digits| digits.parse::<usize>().ok())
                {
                    max_person_number = max_person_number.max(n);
                }
                base_person_aliases.insert(record.source_id, alias);
            }
        }
    }

    let mut base_conversation_aliases: BTreeMap<String, String> = BTreeMap::new();
    let mut max_conversation_number: usize = 0;
    {
        let conversations_record = base
            .manifest
            .files
            .iter()
            .find(|r| r.relative_path == "conversations.jsonl")
            .ok_or_else(|| {
                RestoreError::Integrity("base corpus has no conversations sidecar".into())
            })?;
        let conversations_path = safe_corpus_path(&base.root, "conversations.jsonl")?;
        let conversations_bytes =
            read_corpus_owner_file_limited(&conversations_path, MAXIMUM_CONTROL_FILE_BYTES)?;
        if sha256_bytes(&conversations_bytes) != conversations_record.sha256 {
            return Err(RestoreError::Integrity(
                "base corpus conversations sidecar failed hash verification; re-prepare from scratch"
                    .into(),
            ));
        }
        for line in conversations_bytes.split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let record: ConversationSidecarRecord = serde_json::from_slice(line)?;
            if let Some(n) = record
                .alias
                .strip_prefix('C')
                .and_then(|digits| digits.parse::<usize>().ok())
            {
                max_conversation_number = max_conversation_number.max(n);
            }
            base_conversation_aliases.insert(record.source_id, record.alias);
        }
    }

    // --- Step 3: Build base location set from evidence.jsonl (with hash verify). ---
    // We decode each base canonical_id to extract the 5 location fields so we can
    // perform location-based lookup at metadata-scan time (CorpusMessageMetadata has
    // no canonical_id field).
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct BaseCursorFields {
        sort_sequence: i64,
        create_time: i64,
        server_id: i64,
        shard_id: u32,
        row_id: i64,
    }
    let mut base_locations = BTreeSet::<CorpusMessageLocation>::new();
    let carried_message_count;
    {
        let evidence_record = base
            .manifest
            .files
            .iter()
            .find(|r| r.relative_path == "evidence.jsonl")
            .ok_or_else(|| RestoreError::Integrity("base corpus has no evidence sidecar".into()))?;
        let evidence_path = safe_corpus_path(&base.root, "evidence.jsonl")?;
        let metadata = corpus_owner_file_metadata(&evidence_path)?;
        if metadata.len() != evidence_record.byte_count {
            return Err(RestoreError::Integrity(
                "base corpus evidence sidecar has an unexpected byte count; re-prepare from scratch"
                    .into(),
            ));
        }
        let mut reader_buf = BufReader::with_capacity(1 << 20, File::open(&evidence_path)?);
        let mut line = Vec::new();
        let mut hasher = Sha256::new();
        let mut byte_count = 0_u64;
        let mut count = 0_u64;
        loop {
            line.clear();
            let read = reader_buf.read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            byte_count = byte_count.saturating_add(read as u64);
            hasher.update(&line);
            let evidence: EvidenceRecord = serde_json::from_slice(&line)?;
            let decoded = URL_SAFE_NO_PAD
                .decode(evidence.canonical_id.as_bytes())
                .map_err(|_| {
                    RestoreError::Integrity(
                        "base corpus evidence contains an invalid canonical_id encoding; \
                         re-prepare from scratch"
                            .into(),
                    )
                })?;
            let cursor: BaseCursorFields = serde_json::from_slice(&decoded).map_err(|_| {
                RestoreError::Integrity(
                    "base corpus evidence canonical_id does not decode to expected fields; \
                         re-prepare from scratch"
                        .into(),
                )
            })?;
            if !base_locations.insert(CorpusMessageLocation {
                sort_sequence: cursor.sort_sequence,
                create_time: cursor.create_time,
                server_id: cursor.server_id,
                shard_id: cursor.shard_id,
                row_id: cursor.row_id,
            }) {
                return Err(RestoreError::Integrity(
                    "base corpus evidence sidecar contains a duplicate canonical-id; re-prepare from scratch"
                        .into(),
                ));
            }
            count = count.saturating_add(1);
        }
        if byte_count != evidence_record.byte_count
            || hex::encode(hasher.finalize()) != evidence_record.sha256
        {
            return Err(RestoreError::Integrity(
                "base corpus evidence sidecar failed hash verification; re-prepare from scratch"
                    .into(),
            ));
        }
        if count != base.manifest.evidence_count {
            return Err(RestoreError::Integrity(
                "base corpus evidence count does not match its manifest; re-prepare from scratch"
                    .into(),
            ));
        }
        carried_message_count = count;
    }

    progress(&PersonalMemoryProgress {
        phase: "baseCorpusVerification",
        completed_items: base.manifest.unit_count,
        total_items: base.manifest.unit_count,
        scanned_message_count: carried_message_count,
        selected_message_count: carried_message_count,
        hydrated_message_count: carried_message_count,
    });

    // --- Step 4: Full metadata re-scan of the live source. ---
    let live_reader = LiveCorpusReader::open(source).map_err(corpus_query_error)?;
    let inventory = live_reader.inventory().clone();
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
    let mut live_locations = BTreeSet::<CorpusMessageLocation>::new();
    let mut new_episode_drafts = Vec::<EpisodeDraft>::new();
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
        let scan = live_reader
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

        // Collect all live locations (unfiltered) for fail-closed check.
        for msg in &scan.messages {
            live_locations.insert(msg.location.clone());
        }

        if !conversation_enabled(conversation, &policy, corpus_mode) {
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

        let active_months = if corpus_mode == PersonalMemoryCorpusMode::AllMessages {
            month_indices.keys().cloned().collect::<BTreeSet<_>>()
        } else {
            month_indices
                .iter()
                .filter_map(|(month, indices)| {
                    let self_count = indices
                        .iter()
                        .filter(|index| messages[**index].is_account_holder == Some(true))
                        .count();
                    (self_count >= policy.minimum_self_messages_per_active_month)
                        .then_some(month.clone())
                })
                .collect::<BTreeSet<_>>()
        };
        let recent_active_count = active_months
            .iter()
            .filter(|month| {
                recent_start_ordinal.is_some_and(|start| {
                    month.ordinal >= start && month.ordinal <= reference_month.ordinal
                })
            })
            .count();
        let recent_conversation_eligible = corpus_mode == PersonalMemoryCorpusMode::AllMessages
            || policy.minimum_self_active_months_in_lookback == 0
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
                if corpus_mode == PersonalMemoryCorpusMode::AllMessages {
                    let new_locations: Vec<CorpusMessageLocation> = session
                        .iter()
                        .filter_map(|index| {
                            selected_in_month.insert(*index);
                            let msg = &messages[*index];
                            if base_locations.contains(&msg.location) {
                                None
                            } else {
                                Some(msg.location.clone())
                            }
                        })
                        .collect();
                    if !new_locations.is_empty() {
                        new_episode_drafts.push(EpisodeDraft {
                            conversation: conversation.clone(),
                            month: month.label.clone(),
                            locations: new_locations,
                        });
                    }
                    continue;
                }
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
                    let new_locations: Vec<CorpusMessageLocation> = session[start..=end]
                        .iter()
                        .filter_map(|index| {
                            selected_in_month.insert(*index);
                            let msg = &messages[*index];
                            if base_locations.contains(&msg.location) {
                                None
                            } else {
                                Some(msg.location.clone())
                            }
                        })
                        .collect();
                    if !new_locations.is_empty() {
                        new_episode_drafts.push(EpisodeDraft {
                            conversation: conversation.clone(),
                            month: month.label.clone(),
                            locations: new_locations,
                        });
                    }
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

    if corpus_mode == PersonalMemoryCorpusMode::AllMessages {
        coverage.selected_message_count = coverage.eligible_message_count;
    }

    // --- Step 5: Fail-closed check — every base location must be present in the live scan. ---
    let missing_count = base_locations
        .iter()
        .filter(|loc| !live_locations.contains(*loc))
        .count();
    if missing_count > 0 {
        return Err(RestoreError::Integrity(format!(
            "{missing_count} base message(s) are missing or mutated in the live source; \
             the source database may have been modified — re-prepare from scratch"
        )));
    }

    // --- Step 6: Hydrate only the new episodes. ---
    let mut new_episode_groups = BTreeMap::<String, Vec<EpisodeDraft>>::new();
    for draft in new_episode_drafts {
        new_episode_groups
            .entry(draft.conversation.source_id.clone())
            .or_default()
            .push(draft);
    }
    let hydration_group_count = new_episode_groups.len();
    let mut hydrated_episodes = Vec::<HydratedEpisode>::new();
    let mut hydrated_message_count = 0_u64;

    progress(&PersonalMemoryProgress {
        phase: "selectedContentHydration",
        completed_items: 0,
        total_items: hydration_group_count,
        scanned_message_count: coverage.scanned_message_count,
        selected_message_count: coverage.selected_message_count,
        hydrated_message_count: 0,
    });

    for (hydration_index, (_, episodes)) in new_episode_groups.into_iter().enumerate() {
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
        let hydration = live_reader
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

    // --- Step 7: Build combined alias maps. ---
    let mut conversation_aliases: BTreeMap<String, String> = base_conversation_aliases.clone();
    let mut next_conversation_number = max_conversation_number.saturating_add(1);
    for conversation in &inventory.conversations {
        conversation_aliases
            .entry(conversation.source_id.clone())
            .or_insert_with(|| {
                let alias = format!("C{:06}", next_conversation_number);
                next_conversation_number = next_conversation_number.saturating_add(1);
                alias
            });
    }
    for record in activity.values_mut() {
        if let Some(alias) = conversation_aliases.get(&record.conversation_id) {
            record.conversation = alias.clone();
        }
    }

    let mut person_names: BTreeMap<String, String> = base_person_names.clone();
    for contact in &inventory.contacts {
        if !contact.is_account_holder {
            person_names
                .entry(contact.source_id.clone())
                .or_insert_with(|| contact.display_name.clone());
        }
    }
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
                }
            }
        }
    }
    let mut person_aliases: BTreeMap<String, String> = base_person_aliases.clone();
    let mut next_person_number = max_person_number.saturating_add(1);
    for source_id in &person_ids {
        person_aliases.entry(source_id.clone()).or_insert_with(|| {
            let alias = format!("P{:06}", next_person_number);
            next_person_number = next_person_number.saturating_add(1);
            alias
        });
    }

    // --- Step 8: Build new units from hydrated episodes. ---
    let mut new_units = Vec::<UnitDraft>::new();
    for episode in hydrated_episodes {
        let conversation_alias = conversation_aliases
            .get(&episode.conversation.source_id)
            .cloned()
            .ok_or_else(|| {
                RestoreError::Integrity("extend episode conversation alias is unavailable".into())
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
                new_units.push(UnitDraft {
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
            new_units.push(UnitDraft {
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
    new_units = order_unit_drafts(new_units, policy.delivery_order);

    // --- Step 9: Atomic publication. ---
    let parent = output_directory.parent().unwrap_or_else(|| Path::new("."));
    let carried_unit_count = base.manifest.unit_count;
    let first_new_unit_index = carried_unit_count;
    let total_unit_count = carried_unit_count.saturating_add(new_units.len());

    progress(&PersonalMemoryProgress {
        phase: "atomicPublication",
        completed_items: 0,
        total_items: total_unit_count,
        scanned_message_count: coverage.scanned_message_count,
        selected_message_count: coverage.selected_message_count,
        hydrated_message_count,
    });

    let staging = tempfile::Builder::new()
        .prefix(".greenbubbles-personal-memory-extend-")
        .tempdir_in(parent)?;
    fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))?;
    let batches_directory = staging.path().join("batches");
    fs::create_dir(&batches_directory)?;
    fs::set_permissions(&batches_directory, fs::Permissions::from_mode(0o700))?;

    let mut files = Vec::<CorpusFileRecord>::new();

    // Contacts sidecar.
    let contacts_path = staging.path().join("contacts.jsonl");
    let mut contact_records: Vec<ContactSidecarRecord> = inventory
        .contacts
        .iter()
        .map(|contact| ContactSidecarRecord {
            alias: person_aliases.get(&contact.source_id).cloned(),
            source_id: contact.source_id.clone(),
            display_name: contact.display_name.clone(),
            remark: contact.remark.clone(),
            nickname: contact.nickname.clone(),
            wechat_alias: contact.alias.clone(),
            kind: contact.kind,
            is_account_holder: contact.is_account_holder,
        })
        .collect();
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
                remark: None,
                nickname: None,
                wechat_alias: None,
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

    // Conversations sidecar.
    let conversations_path = staging.path().join("conversations.jsonl");
    let mut conversation_records = Vec::<ConversationSidecarRecord>::new();
    for conversation in &inventory.conversations {
        let alias = conversation_aliases
            .get(&conversation.source_id)
            .cloned()
            .ok_or_else(|| {
                RestoreError::Integrity("extend corpus conversation alias is unavailable".into())
            })?;
        let display_name = model_safe_conversation_label(conversation, &alias);
        conversation_records.push(ConversationSidecarRecord {
            alias,
            source_id: conversation.source_id.clone(),
            display_name,
            kind: conversation.kind,
            contact_kind: conversation.contact_kind,
        });
    }
    for (source_id, alias) in &base_conversation_aliases {
        if !conversation_records
            .iter()
            .any(|r| &r.source_id == source_id)
        {
            conversation_records.push(ConversationSidecarRecord {
                alias: alias.clone(),
                source_id: source_id.clone(),
                display_name: alias.clone(),
                kind: ConversationKind::Unresolved,
                contact_kind: ContactKind::Unknown,
            });
        }
    }
    files.push(write_json_lines(
        &conversations_path,
        "conversations.jsonl",
        conversation_records.iter(),
    )?);

    // Activity sidecar.
    let activity_path = staging.path().join("activity.jsonl");
    files.push(write_json_lines(
        &activity_path,
        "activity.jsonl",
        activity.values(),
    )?);

    // Evidence sidecar — copy base bytes, then append new.
    let evidence_path = staging.path().join("evidence.jsonl");
    let base_evidence_path = safe_corpus_path(&base.root, "evidence.jsonl")?;
    let base_evidence_record = base
        .manifest
        .files
        .iter()
        .find(|r| r.relative_path == "evidence.jsonl")
        .ok_or_else(|| RestoreError::Integrity("base corpus has no evidence sidecar".into()))?;

    let mut evidence_writer = owner_only_writer(&evidence_path)?;
    let mut evidence_hasher = Sha256::new();
    let mut evidence_byte_count = 0_u64;
    let mut evidence_count = 0_u64;

    {
        let mut base_reader = BufReader::with_capacity(1 << 20, File::open(&base_evidence_path)?);
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = base_reader.read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            evidence_hasher.update(&line);
            evidence_writer.write_all(&line)?;
            evidence_byte_count = evidence_byte_count.saturating_add(read as u64);
            evidence_count = evidence_count.saturating_add(1);
        }
    }
    if evidence_byte_count != base_evidence_record.byte_count {
        return Err(RestoreError::Integrity(
            "base evidence sidecar changed size during extend; re-prepare from scratch".into(),
        ));
    }

    // Unit index — start with carried base entries.
    let mut unit_index_entries = Vec::<UnitIndexEntry>::new();
    for entry in &base.unit_index.units {
        unit_index_entries.push(entry.clone());
    }

    // Copy base unit files byte-for-byte.
    let mut largest_unit_text_bytes = base.manifest.largest_unit_text_bytes;
    for entry in &base.unit_index.units {
        let src_path = safe_corpus_path(&base.root, &entry.relative_path)?;
        let dst_path = staging.path().join(&entry.relative_path);
        let src_bytes = read_corpus_owner_file_limited(&src_path, MAXIMUM_UNIT_INDEX_BYTES)?;
        if sha256_bytes(&src_bytes) != entry.sha256 {
            return Err(RestoreError::Integrity(format!(
                "base corpus unit file {} failed hash verification; re-prepare from scratch",
                entry.relative_path
            )));
        }
        let mut writer = owner_only_writer(&dst_path)?;
        writer.write_all(&src_bytes)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        files.push(CorpusFileRecord {
            relative_path: entry.relative_path.clone(),
            byte_count: entry.byte_count,
            sha256: entry.sha256.clone(),
        });
        progress(&PersonalMemoryProgress {
            phase: "atomicPublication",
            completed_items: unit_index_entries.len(),
            total_items: total_unit_count,
            scanned_message_count: coverage.scanned_message_count,
            selected_message_count: coverage.selected_message_count,
            hydrated_message_count,
        });
    }

    // Write new units.
    for (new_unit_offset, unit) in new_units.into_iter().enumerate() {
        let unit_number = carried_unit_count
            .saturating_add(new_unit_offset)
            .saturating_add(1);
        let unit_id = format!("U{:06}", unit_number);
        let mut compact_messages = Vec::with_capacity(unit.messages.len());
        let first_evidence_ordinal = evidence_count.saturating_add(1);
        let mut unit_person_aliases = BTreeSet::new();
        let mut unit_sender_aliases = BTreeSet::new();
        let mut has_account_holder_sender = false;
        let mut has_unknown_sender = false;
        if let Some(person_alias) = person_aliases.get(&unit.conversation_source_id) {
            unit_person_aliases.insert(person_alias.clone());
        }
        let mut text_byte_count = 0usize;
        for message in unit.messages {
            evidence_count = evidence_count.saturating_add(1);
            let evidence_alias = format!("E{evidence_count:09}");
            let actor = match message.is_account_holder {
                Some(true) => {
                    has_account_holder_sender = true;
                    "self"
                }
                Some(false) => "other",
                None => {
                    has_unknown_sender = true;
                    "unknown"
                }
            }
            .to_string();
            let person_alias = message
                .sender
                .as_ref()
                .and_then(|sender| person_aliases.get(sender).cloned());
            if let Some(person_alias) = &person_alias {
                unit_person_aliases.insert(person_alias.clone());
                unit_sender_aliases.insert(person_alias.clone());
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
                e: evidence_alias,
                a: actor,
                p: person_alias,
                t: message.location.create_time,
                k: message.payload_kind,
                x: text,
                tr: message.text_truncated,
            });
        }
        let from = compact_messages
            .first()
            .map(|message| message.t)
            .unwrap_or_default();
        let to = compact_messages
            .last()
            .map(|message| message.t)
            .unwrap_or_default();
        let conversation_alias = unit.conversation_alias;
        let prepared = PreparedUnitFile {
            schema: PERSONAL_MEMORY_BATCH_SCHEMA.to_string(),
            id: unit_id.clone(),
            c: conversation_alias.clone(),
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
            relative_path: relative_path.clone(),
            sha256: record.sha256.clone(),
            byte_count: record.byte_count,
            text_byte_count,
            message_count: prepared.m.len(),
            target_pages: Vec::new(),
            evidence_aliases: Vec::new(),
            person_aliases: unit_person_aliases.into_iter().collect(),
            sender_aliases: unit_sender_aliases.into_iter().collect(),
            has_account_holder_sender,
            has_unknown_sender,
            first_evidence_ordinal: Some(first_evidence_ordinal),
            conversation: conversation_alias,
            conversation_id: String::new(),
            from,
            to,
        });
        files.push(record);
        progress(&PersonalMemoryProgress {
            phase: "atomicPublication",
            completed_items: unit_index_entries.len(),
            total_items: total_unit_count,
            scanned_message_count: coverage.scanned_message_count,
            selected_message_count: coverage.selected_message_count,
            hydrated_message_count,
        });
    }

    evidence_writer.flush()?;
    evidence_writer.get_ref().sync_all()?;
    files.push(CorpusFileRecord {
        relative_path: "evidence.jsonl".into(),
        byte_count: evidence_byte_count,
        sha256: hex::encode(evidence_hasher.finalize()),
    });

    let unit_index = UnitIndex {
        schema: "greenbubbles.personal-memory-unit-index.v2".into(),
        format_version: 2,
        units: unit_index_entries,
    };
    let unit_index_record = write_json_compact(
        &batches_directory.join("index.json"),
        "batches/index.json",
        &unit_index,
    )?;
    if unit_index_record.byte_count > MAXIMUM_UNIT_INDEX_BYTES {
        return Err(RestoreError::Integrity(format!(
            "prepared unit index exceeds the fixed {MAXIMUM_UNIT_INDEX_BYTES}-byte safety limit"
        )));
    }
    files.push(unit_index_record);

    coverage.unit_count = unit_index.units.len() as u64;
    coverage.limitation_codes = limitation_codes.into_iter().collect();
    coverage.row_coverage_complete = coverage.metadata_decode_failure_count == 0
        && !coverage.limitation_codes.iter().any(|code| {
            matches!(
                code.as_str(),
                "messageTableInventoryUnavailable"
                    | "messageTableInventoryIncomplete"
                    | "corpusMetadataQueryFailed"
                    | "corpusMetadataRowFailed"
                    | "unsupportedCorpusMessageSchema"
                    | "shardUnavailable"
                    | "shardSchemaUnavailable"
            )
        });
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
        corpus_mode,
        timezone: policy.timezone,
        delivery_order: policy.delivery_order,
        reference_unix,
        account_holder_attribution_bound: true,
        content_trust: "untrustedChatEvidence".into(),
        immutable_index: true,
        source_coverage_complete: coverage.source_coverage_complete,
        row_coverage_complete: coverage.row_coverage_complete,
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
        extends: Some(CorpusGenerationLink {
            base_manifest_sha256: base.manifest_sha256.clone(),
            generation: base_generation.saturating_add(1),
            first_new_unit_index,
            carried_unit_count,
            carried_message_count,
        }),
    };
    write_json_pretty(
        &staging.path().join("manifest.json"),
        "manifest.json",
        &manifest,
    )?;
    File::open(&batches_directory)?.sync_all()?;
    File::open(staging.path())?.sync_all()?;
    protect_extendable_corpus_tree(staging.path())?;
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
    corpus_mode: PersonalMemoryCorpusMode,
) -> bool {
    if corpus_mode == PersonalMemoryCorpusMode::AllMessages {
        return true;
    }
    match conversation.contact_kind {
        ContactKind::Group => policy.include_group_conversations,
        ContactKind::Official => policy.include_official_accounts,
        ContactKind::Service | ContactKind::AccountHolder => policy.include_service_accounts,
        ContactKind::Person | ContactKind::Unknown => policy.include_direct_conversations,
    }
}

fn model_safe_conversation_label(conversation: &CorpusConversation, alias: &str) -> String {
    let display_name = conversation.display_name.trim();
    if !display_name.is_empty() {
        return display_name.to_string();
    }
    let source_id = conversation.source_id.trim();
    if !source_id.is_empty() {
        return source_id.to_string();
    }
    alias.to_string()
}

fn model_safe_person_label(record: &ContactSidecarRecord, alias: &str) -> String {
    let display_name = record.display_name.trim();
    if !display_name.is_empty() {
        return display_name.to_string();
    }
    let source_id = record.source_id.trim();
    if !source_id.is_empty() {
        return source_id.to_string();
    }
    alias.to_string()
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

/// Sticker, location, and system payloads reach the corpus as WeChat markup
/// envelopes whose CDN and geometry attributes mean nothing to a reader while
/// consuming most of a delivery page. Rendering only their human-readable text
/// keeps pages dense without rewriting the immutable prepared corpus.
fn delivery_message_text(kind: &str, text: &str) -> String {
    let rendered = if looks_like_markup_envelope(text) {
        let readable = markup_envelope_text(text);
        if readable.is_empty() {
            format!("[{kind}]")
        } else {
            readable
        }
    } else {
        text.to_string()
    };
    truncate_on_character_boundary(rendered, MAXIMUM_DELIVERED_MESSAGE_TEXT_BYTES)
}

fn looks_like_markup_envelope(text: &str) -> bool {
    let trimmed = text.trim_start();
    (trimmed.starts_with("<?xml") || trimmed.starts_with("<msg") || trimmed.starts_with("<sysmsg"))
        && text.trim_end().ends_with('>')
}

/// Deliberately minimal: this reads text nodes, CDATA sections, and a small
/// allow list of human-readable attributes. Anything else in the envelope is
/// machine plumbing, and every extracted string stays untrusted evidence.
fn markup_envelope_text(text: &str) -> String {
    let mut pieces = Vec::<String>::new();
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        push_markup_text(&mut pieces, &rest[..open]);
        rest = &rest[open..];
        if let Some(body) = rest.strip_prefix("<![CDATA[") {
            let Some(end) = body.find("]]>") else {
                return pieces.join(" ");
            };
            push_markup_text(&mut pieces, &body[..end]);
            rest = &body[end.saturating_add(3)..];
            continue;
        }
        let Some(close) = rest.find('>') else {
            return pieces.join(" ");
        };
        push_markup_attribute_text(&mut pieces, &rest[1..close]);
        rest = &rest[close.saturating_add(1)..];
    }
    push_markup_text(&mut pieces, rest);
    pieces.join(" ")
}

fn push_markup_attribute_text(pieces: &mut Vec<String>, tag: &str) {
    let mut rest = tag;
    while let Some(equals) = rest.find('=') {
        let name = rest[..equals]
            .trim_end()
            .rsplit(|character: char| character.is_whitespace())
            .next()
            .unwrap_or_default();
        let readable = DELIVERED_MARKUP_TEXT_ATTRIBUTES
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(name));
        let after = rest[equals.saturating_add(1)..].trim_start();
        let Some(quote) = after
            .chars()
            .next()
            .filter(|character| *character == '"' || *character == '\'')
        else {
            rest = &rest[equals.saturating_add(1)..];
            continue;
        };
        let body = &after[quote.len_utf8()..];
        let Some(end) = body.find(quote) else {
            return;
        };
        if readable {
            push_markup_text(pieces, &body[..end]);
        }
        rest = &body[end.saturating_add(quote.len_utf8())..];
    }
}

fn push_markup_text(pieces: &mut Vec<String>, candidate: &str) {
    let candidate = candidate.trim();
    if candidate.is_empty() || pieces.iter().any(|existing| existing == candidate) {
        return;
    }
    pieces.push(candidate.to_string());
}

fn truncate_on_character_boundary(mut text: String, maximum_bytes: usize) -> String {
    if text.len() <= maximum_bytes {
        return text;
    }
    let mut end = maximum_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.truncate(end);
    text.push('\u{2026}');
    text
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

fn write_json_compact<T: Serialize>(
    path: &Path,
    relative_path: &str,
    value: &T,
) -> Result<CorpusFileRecord, RestoreError> {
    let mut bytes = serde_json::to_vec(value)?;
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

fn parse_rfc3339_scope_bound(
    value: &str,
    option: &str,
    round_fraction_up: bool,
) -> Result<i64, RestoreError> {
    if value.trim() != value || value.is_empty() {
        return Err(RestoreError::Integrity(format!(
            "memory {option} must be a non-empty RFC 3339 timestamp without surrounding whitespace"
        )));
    }
    let parsed = DateTime::parse_from_rfc3339(value).map_err(|_| {
        RestoreError::Integrity(format!(
            "memory {option} must be an RFC 3339 timestamp with an explicit offset, for example 2023-12-02T11:18:36+08:00"
        ))
    })?;
    let nanos = parsed.timestamp_subsec_nanos();
    if nanos >= 1_000_000_000 {
        return Err(RestoreError::Integrity(format!(
            "memory {option} does not accept an RFC 3339 leap-second value"
        )));
    }
    if round_fraction_up && nanos != 0 {
        parsed.timestamp().checked_add(1).ok_or_else(|| {
            RestoreError::Integrity(format!(
                "memory {option} is outside the supported time range"
            ))
        })
    } else {
        Ok(parsed.timestamp())
    }
}

fn format_unix_seconds_rfc3339(timezone: Tz, timestamp: i64) -> Result<String, RestoreError> {
    timezone
        .timestamp_opt(timestamp, 0)
        .single()
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Secs, false))
        .ok_or_else(|| {
            RestoreError::Integrity(
                "personal-memory timestamp is outside the RFC 3339 presentation range".into(),
            )
        })
}

fn format_unix_milliseconds_rfc3339(timezone: Tz, timestamp: u64) -> Result<String, RestoreError> {
    let timestamp = i64::try_from(timestamp).map_err(|_| {
        RestoreError::Integrity(
            "personal-memory millisecond timestamp is outside the RFC 3339 presentation range"
                .into(),
        )
    })?;
    timezone
        .timestamp_millis_opt(timestamp)
        .single()
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, false))
        .ok_or_else(|| {
            RestoreError::Integrity(
                "personal-memory millisecond timestamp is outside the RFC 3339 presentation range"
                    .into(),
            )
        })
}

fn manifest_timezone(manifest: &PersonalMemoryCorpusManifest) -> Result<Tz, RestoreError> {
    manifest.timezone.parse::<Tz>().map_err(|_| {
        RestoreError::Integrity(
            "personal-memory corpus manifest contains an unsupported IANA timezone".into(),
        )
    })
}

pub fn personal_memory_manifest_output(
    manifest: &PersonalMemoryCorpusManifest,
) -> Result<Value, RestoreError> {
    let timezone = manifest_timezone(manifest)?;
    let mut output = serde_json::to_value(manifest)?;
    let object = output.as_object_mut().ok_or_else(|| {
        RestoreError::Integrity("personal-memory manifest did not serialize as an object".into())
    })?;
    object.remove("generatedAtUnixMilliseconds");
    object.remove("referenceUnix");
    object.insert(
        "generatedAt".into(),
        Value::String(format_unix_milliseconds_rfc3339(
            timezone,
            manifest.generated_at_unix_milliseconds,
        )?),
    );
    object.insert(
        "referenceTime".into(),
        Value::String(format_unix_seconds_rfc3339(
            timezone,
            manifest.reference_unix,
        )?),
    );
    Ok(output)
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
    prose_lines: Vec<ProseLine>,
}

/// One-based source line numbers travel with each prose line so a rejected
/// commit can tell the agent exactly where to look instead of a bare count.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ProseLine {
    number: usize,
    citations: BTreeSet<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompletedMemoryScope {
    #[serde(rename = "scopeSHA256")]
    scope_sha256: String,
    unit_count: usize,
    message_count: u64,
    completed_at_unix_milliseconds: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MemoryCommitDisposition {
    #[default]
    WikiUpdated,
    ReviewedNoDurableMemory,
}

/// Output format accepted by `memory commit` for a run.
///
/// The default is `Wiki` (Markdown files with evidence citations).
/// Set once on the first `memory next` call via `--format`; verified
/// on every subsequent call to the same state file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    #[default]
    Wiki,
    Python,
    Markdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MemoryRunState {
    schema: String,
    format_version: u32,
    #[serde(rename = "corpusManifestSHA256")]
    corpus_manifest_sha256: String,
    /// Present on new scoped states. Missing means a legacy state over every prepared unit.
    #[serde(default)]
    scope: Option<ResolvedPersonalMemoryScope>,
    #[serde(default, rename = "scopeSHA256")]
    scope_sha256: Option<String>,
    #[serde(default)]
    scoped_units: Vec<ScopedUnitSelection>,
    #[serde(default)]
    scoped_message_count: Option<u64>,
    #[serde(default)]
    completed_scopes: Vec<CompletedMemoryScope>,
    #[serde(default)]
    output_format: OutputFormat,
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
    scope: MemoryScopeOutput,
    position: BatchPosition,
    delivery: MemoryBatchDelivery,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryScopeOutput {
    conversation_filter_count: usize,
    conversation_kinds: Vec<PersonalMemoryConversationKindSelector>,
    from: Option<String>,
    through: Option<String>,
    sender_filter_count: usize,
    all_messages: bool,
    summary_subject: ResolvedMemorySummarySubject,
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
    pub eligible_message_count: u64,
    /// Hydrated messages physically present before run-time scope filtering.
    pub corpus_message_count: u64,
    pub selected_message_count: u64,
    pub source_coverage_complete: bool,
    pub row_coverage_complete: bool,
    pub content_complete: bool,
    pub unmatched_message_table_count: usize,
    pub limitation_codes: Vec<String>,
    pub delivery_order: MemoryDeliveryOrder,
    pub scope: MemoryScopeStatus,
    pub completed_scope_count: usize,
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryScopeStatus {
    pub conversation_filter_count: usize,
    pub conversation_kinds: Vec<PersonalMemoryConversationKindSelector>,
    pub from: Option<String>,
    pub through: Option<String>,
    pub sender_filter_count: usize,
    pub includes_account_holder_sender: bool,
    pub includes_unknown_sender: bool,
    pub all_messages: bool,
    pub summary_subject: String,
    pub summary_subject_alias: Option<String>,
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

fn unit_person_aliases(entry: &UnitIndexEntry) -> BTreeSet<String> {
    entry
        .person_aliases
        .iter()
        .cloned()
        .chain(
            entry
                .target_pages
                .iter()
                .filter_map(|path| person_alias_from_target_page(path).map(str::to_string)),
        )
        .collect()
}

fn memory_scope_output(
    scope: &ResolvedPersonalMemoryScope,
    timezone: Tz,
) -> Result<MemoryScopeOutput, RestoreError> {
    Ok(MemoryScopeOutput {
        conversation_filter_count: scope.conversation_aliases.len(),
        conversation_kinds: scope.conversation_kinds.iter().copied().collect(),
        from: scope
            .not_before_unix
            .map(|value| format_unix_seconds_rfc3339(timezone, value))
            .transpose()?,
        through: scope
            .not_after_unix
            .map(|value| format_unix_seconds_rfc3339(timezone, value))
            .transpose()?,
        sender_filter_count: scope.sender_aliases.len()
            + usize::from(scope.include_account_holder_sender)
            + usize::from(scope.include_unknown_sender),
        all_messages: scope_selects_all_evidence(scope),
        summary_subject: scope.summary_subject.clone(),
    })
}

fn memory_scope_status(
    scope: &ResolvedPersonalMemoryScope,
    timezone: Tz,
) -> Result<MemoryScopeStatus, RestoreError> {
    let (summary_subject, summary_subject_alias) = match &scope.summary_subject {
        ResolvedMemorySummarySubject::AccountHolder => ("accountHolder".into(), None),
        ResolvedMemorySummarySubject::Person { alias } => ("person".into(), Some(alias.clone())),
        ResolvedMemorySummarySubject::None => ("none".into(), None),
    };
    Ok(MemoryScopeStatus {
        conversation_filter_count: scope.conversation_aliases.len(),
        conversation_kinds: scope.conversation_kinds.iter().copied().collect(),
        from: scope
            .not_before_unix
            .map(|value| format_unix_seconds_rfc3339(timezone, value))
            .transpose()?,
        through: scope
            .not_after_unix
            .map(|value| format_unix_seconds_rfc3339(timezone, value))
            .transpose()?,
        sender_filter_count: scope.sender_aliases.len()
            + usize::from(scope.include_account_holder_sender)
            + usize::from(scope.include_unknown_sender),
        includes_account_holder_sender: scope.include_account_holder_sender,
        includes_unknown_sender: scope.include_unknown_sender,
        all_messages: scope_selects_all_evidence(scope),
        summary_subject,
        summary_subject_alias,
    })
}

fn append_delivery_message(
    episodes: &mut Vec<DeliveryEpisode>,
    unit: &PreparedUnitFile,
    message_offset: usize,
    timezone: Tz,
) -> Result<(), RestoreError> {
    let compact = &unit.m[message_offset];
    let message = DeliveryMessage {
        e: compact.e.clone(),
        a: compact.a.clone(),
        p: compact.p.clone(),
        t: format_unix_seconds_rfc3339(timezone, compact.t)?,
        k: compact.k.clone(),
        x: delivery_message_text(&compact.k, &compact.x),
        tr: compact.tr,
    };
    if let Some(current) = episodes.last_mut().filter(|episode| {
        episode.u == unit.id && episode.o.saturating_add(episode.m.len()) == message_offset
    }) {
        current.to = message.t.clone();
        current.m.push(message);
        return Ok(());
    }
    episodes.push(DeliveryEpisode {
        u: unit.id.clone(),
        c: unit.c.clone(),
        month: unit.month.clone(),
        from: message.t.clone(),
        to: message.t.clone(),
        o: message_offset,
        n: unit.m.len(),
        m: vec![message],
    });
    Ok(())
}

fn remove_last_delivery_message(episodes: &mut Vec<DeliveryEpisode>) {
    let Some(last) = episodes.last_mut() else {
        return;
    };
    last.m.pop();
    if let Some(message) = last.m.last() {
        last.to = message.t.clone();
    } else {
        episodes.pop();
    }
}

fn delivery_page_payload(
    batch_id: &str,
    number: usize,
    page_count: usize,
    episodes: Vec<DeliveryEpisode>,
    context: &DeliveryPageContext<'_>,
) -> Result<DeliveryPagePayload, RestoreError> {
    let mut aliases = BTreeSet::new();
    if let ResolvedMemorySummarySubject::Person { alias } = &context.scope.summary_subject {
        aliases.insert(alias.clone());
    }
    for episode in &episodes {
        if context.direct_people_by_unit.contains_key(&episode.u) {
            aliases.extend(
                context
                    .direct_people_by_unit
                    .get(&episode.u)
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
        }
        aliases.extend(episode.m.iter().filter_map(|message| message.p.clone()));
    }
    let mut target_pages = BTreeSet::from(["index.md".to_string()]);
    match &context.scope.summary_subject {
        ResolvedMemorySummarySubject::AccountHolder => {
            target_pages.insert("me.md".into());
        }
        ResolvedMemorySummarySubject::Person { alias } => {
            target_pages.insert(format!("people/{alias}.md"));
        }
        ResolvedMemorySummarySubject::None => {}
    }
    let people = aliases
        .into_iter()
        .map(|alias| {
            if matches!(
                &context.scope.summary_subject,
                ResolvedMemorySummarySubject::AccountHolder
            ) {
                target_pages.insert(format!("people/{alias}.md"));
            }
            let identity = context
                .identities
                .people
                .get(&alias)
                .cloned()
                .ok_or_else(|| {
                    RestoreError::Integrity(
                        "memory page cannot resolve a participant's source identity".into(),
                    )
                })?;
            Ok((alias, identity))
        })
        .collect::<Result<BTreeMap<_, _>, RestoreError>>()?;
    let conversations = episodes
        .iter()
        .map(|episode| {
            target_pages.insert(format!("conversations/{}.md", episode.c));
            let identity = context
                .identities
                .conversations
                .get(&episode.c)
                .cloned()
                .ok_or_else(|| {
                    RestoreError::Integrity(
                        "memory page cannot resolve a conversation's source identity".into(),
                    )
                })?;
            Ok((episode.c.clone(), identity))
        })
        .collect::<Result<BTreeMap<_, _>, RestoreError>>()?;
    let message_count = episodes.iter().map(|episode| episode.m.len()).sum();
    let text_byte_count = episodes
        .iter()
        .flat_map(|episode| &episode.m)
        .map(|message| message.x.len())
        .sum();
    Ok(DeliveryPagePayload {
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
        scope: memory_scope_output(context.scope, context.timezone)?,
        target_pages: target_pages.into_iter().collect(),
        account_holder: context.identities.account_holder.clone(),
        people,
        conversations,
        episodes,
    })
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
    scope: &ResolvedPersonalMemoryScope,
    scoped_units: &[ScopedUnitSelection],
    batch_id: &str,
    start_unit_index: usize,
    end_unit_index_exclusive: usize,
) -> Result<Vec<RenderedDeliveryPage>, RestoreError> {
    if start_unit_index >= end_unit_index_exclusive || end_unit_index_exclusive > scoped_units.len()
    {
        return Err(RestoreError::Integrity(
            "outstanding batch contains an invalid delivery range".into(),
        ));
    }
    let selections = &scoped_units[start_unit_index..end_unit_index_exclusive];
    let units = selections
        .iter()
        .map(|selection| load_scoped_unit(corpus, selection))
        .collect::<Result<Vec<_>, _>>()?;
    let mut all_person_aliases = BTreeSet::new();
    if let ResolvedMemorySummarySubject::Person { alias } = &scope.summary_subject {
        all_person_aliases.insert(alias.clone());
    }
    let mut direct_people_by_unit = BTreeMap::<String, BTreeSet<String>>::new();
    let mut all_conversation_aliases = BTreeSet::new();
    for (selection, unit) in selections.iter().zip(&units) {
        let entry = &corpus.unit_index.units[selection.corpus_unit_index];
        let aliases = unit_person_aliases(entry);
        all_person_aliases.extend(aliases.iter().cloned());
        if unit.kind == ConversationKind::Direct {
            direct_people_by_unit.insert(unit.id.clone(), aliases);
        }
        all_conversation_aliases.insert(unit.c.clone());
    }
    let timezone = manifest_timezone(&corpus.manifest)?;
    let identities =
        load_delivery_identities(corpus, &all_person_aliases, &all_conversation_aliases)?;
    let context = DeliveryPageContext {
        scope,
        timezone,
        direct_people_by_unit: &direct_people_by_unit,
        identities: &identities,
    };

    let mut drafts = Vec::<Vec<DeliveryEpisode>>::new();
    let mut current = Vec::<DeliveryEpisode>::new();
    for unit in &units {
        for message_offset in 0..unit.m.len() {
            append_delivery_message(&mut current, unit, message_offset, timezone)?;
            // Use maximum-width counters while packing. The actual page number and
            // count can only make the final compact JSON smaller.
            let candidate = render_delivery_page(delivery_page_payload(
                batch_id,
                usize::MAX,
                usize::MAX,
                current.clone(),
                &context,
            )?)?;
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
            append_delivery_message(&mut current, unit, message_offset, timezone)?;
            let single = render_delivery_page(delivery_page_payload(
                batch_id,
                usize::MAX,
                usize::MAX,
                current.clone(),
                &context,
            )?)?;
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
            &context,
        )?)?;
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
    scope: &ResolvedPersonalMemoryScope,
    scoped_units: &[ScopedUnitSelection],
    outstanding: &mut OutstandingBatch,
) -> Result<Vec<RenderedDeliveryPage>, RestoreError> {
    let rendered = build_delivery_pages(
        corpus,
        scope,
        scoped_units,
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
    next_personal_memory_batch_with_scope(
        corpus_directory,
        state_path,
        wiki_directory,
        maximum_text_bytes,
        None,
    )
}

pub fn next_personal_memory_batch_with_scope(
    corpus_directory: &Path,
    state_path: &Path,
    wiki_directory: Option<&Path>,
    maximum_text_bytes: usize,
    scope_options: Option<&PersonalMemoryScopeOptions>,
) -> Result<Value, RestoreError> {
    next_personal_memory_batch_with_bounds(
        corpus_directory,
        state_path,
        wiki_directory,
        maximum_text_bytes,
        None,
        scope_options,
    )
}

/// `maximum_text_bytes` bounds stored message text, which says nothing about the
/// per-message envelope. A conversation of one-word replies therefore fills far
/// more delivery pages than its text bytes suggest, so a caller sizing a batch
/// against a fixed agent context window can also bound the message count.
pub fn next_personal_memory_batch_with_bounds(
    corpus_directory: &Path,
    state_path: &Path,
    wiki_directory: Option<&Path>,
    maximum_text_bytes: usize,
    maximum_messages: Option<usize>,
    scope_options: Option<&PersonalMemoryScopeOptions>,
) -> Result<Value, RestoreError> {
    next_personal_memory_batch_with_bounds_and_format(
        corpus_directory,
        state_path,
        wiki_directory,
        maximum_text_bytes,
        maximum_messages,
        scope_options,
        None,
    )
}

/// Same as [`next_personal_memory_batch_with_bounds`], with an optional
/// `output_format` selection that is written to the state on the first bind
/// and verified on every subsequent call.
pub fn next_personal_memory_batch_with_bounds_and_format(
    corpus_directory: &Path,
    state_path: &Path,
    wiki_directory: Option<&Path>,
    maximum_text_bytes: usize,
    maximum_messages: Option<usize>,
    scope_options: Option<&PersonalMemoryScopeOptions>,
    output_format: Option<OutputFormat>,
) -> Result<Value, RestoreError> {
    if !(MINIMUM_NEXT_TEXT_BYTES..=MAXIMUM_NEXT_TEXT_BYTES).contains(&maximum_text_bytes) {
        return Err(RestoreError::Integrity(format!(
            "memory next --max-text-bytes must be between {MINIMUM_NEXT_TEXT_BYTES} and {MAXIMUM_NEXT_TEXT_BYTES}"
        )));
    }
    if maximum_messages.is_some_and(|maximum| !(1..=MAXIMUM_BATCH_MESSAGES).contains(&maximum)) {
        return Err(RestoreError::Integrity(format!(
            "memory next --max-messages must be between 1 and {MAXIMUM_BATCH_MESSAGES}"
        )));
    }
    // A soft bound: it stops a batch from taking another unit, and never splits
    // or refuses the one unit a batch must always deliver whole.
    let maximum_packed_messages = maximum_messages
        .unwrap_or(MAXIMUM_BATCH_MESSAGES)
        .min(MAXIMUM_BATCH_MESSAGES);
    let corpus = load_corpus(corpus_directory)?;
    let _lock = acquire_state_lock(state_path)?;
    let mut state = load_or_initialize_state(state_path, &corpus, scope_options)?;
    // Apply or verify the output format on bind.
    if let Some(requested_format) = output_format {
        if state.output_format == OutputFormat::Wiki
            && state.next_unit_index == 0
            && state.outstanding.is_none()
            && state.last_committed.is_none()
        {
            // First bind — write the requested format into the state.
            state.output_format = requested_format;
            state.updated_at_unix_milliseconds = now_unix_milliseconds()?;
            write_state_atomic(state_path, &state)?;
        } else if state.output_format != requested_format {
            return Err(RestoreError::Integrity(format!(
                "memory next --format does not match the format already bound to this state \
                 (bound: {:?}; requested: {:?})",
                state.output_format, requested_format
            )));
        }
    }
    let (scope, scoped_units) = effective_scope_and_units(&state, &corpus);
    if state.next_unit_index > scoped_units.len() {
        return Err(RestoreError::Integrity(
            "memory state cursor exceeds the immutable scoped unit count".into(),
        ));
    }
    let verified_wiki_before = if state.outstanding.is_none() {
        let current = if state.output_format == OutputFormat::Wiki {
            wiki_directory.map(scan_wiki).transpose()?
        } else {
            None
        };
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
    if state.outstanding.is_none() && state.next_unit_index == scoped_units.len() {
        if state.committed_wiki.is_none() && verified_wiki_before.is_some() {
            state.committed_wiki = verified_wiki_before;
            state.updated_at_unix_milliseconds = now_unix_milliseconds()?;
            write_state_atomic(state_path, &state)?;
        }
        return Ok(json!({
            "schema": PERSONAL_MEMORY_BATCH_SCHEMA,
            "formatVersion": PERSONAL_MEMORY_FORMAT_VERSION,
            "deliveryOrder": corpus.manifest.delivery_order,
            "scope": memory_scope_output(&scope, manifest_timezone(&corpus.manifest)?)?,
            "complete": true,
            "position": {
                "firstUnit": state.next_unit_index.saturating_add(1),
                "unitCount": 0,
                "totalUnits": scoped_units.len(),
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
        let first = scoped_units.get(start).ok_or_else(|| {
            RestoreError::Integrity("memory state points to a missing scoped unit".into())
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
        while let Some(selection) = scoped_units.get(end) {
            let entry = &corpus.unit_index.units[selection.corpus_unit_index];
            let next_text = text_byte_count.saturating_add(selection.text_byte_count);
            let next_messages = message_count.saturating_add(selection.message_count);
            if end > start
                && (next_text > maximum_text_bytes || next_messages > maximum_packed_messages)
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
            let unit = load_scoped_unit(&corpus, selection)?;
            extend_memory_target_pages(&mut target_pages, &scope, &unit, entry);
            evidence_aliases.extend(unit.m.iter().map(|message| message.e.clone()));
            unit_hashes.push(scope_unit_binding_hash(entry, selection));
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
    ensure_outstanding_delivery(&corpus, &scope, &scoped_units, outstanding)?;
    state.updated_at_unix_milliseconds = now_unix_milliseconds()?;
    write_state_atomic(state_path, &state)?;
    render_outstanding_batch(
        state
            .outstanding
            .as_ref()
            .ok_or_else(|| RestoreError::Integrity("outstanding batch was not persisted".into()))?,
        scoped_units.len(),
        corpus.manifest.delivery_order,
        &scope,
        manifest_timezone(&corpus.manifest)?,
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
    let (scope, scoped_units) = effective_scope_and_units(&state, &corpus);
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
    let rendered = ensure_outstanding_delivery(&corpus, &scope, &scoped_units, outstanding)?;
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
            "scope": memory_scope_output(&scope, manifest_timezone(&corpus.manifest)?)?,
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
    let (scope, scoped_units) = effective_scope_and_units(&state, &corpus);
    let outstanding = state.outstanding.as_mut().ok_or_else(|| {
        RestoreError::Integrity("memory acknowledge has no outstanding batch".into())
    })?;
    if !memory_selector_matches(batch_id, &outstanding.batch_id) {
        return Err(RestoreError::Integrity(
            "memory acknowledge batch identifier does not match the outstanding batch".into(),
        ));
    }
    let resolved_batch_id = outstanding.batch_id.clone();
    ensure_outstanding_delivery(&corpus, &scope, &scoped_units, outstanding)?;
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
    let (scope, scoped_units) = effective_scope_and_units(&state, &corpus);
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
            total_units: scoped_units.len(),
            complete: state.next_unit_index == scoped_units.len(),
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
    ensure_outstanding_delivery(&corpus, &scope, &scoped_units, &mut outstanding)?;
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
    let changed_pages = match state.output_format {
        OutputFormat::Wiki => {
            let current_wiki = scan_wiki(wiki_directory)?;
            let changed = match disposition {
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
                            RestoreError::Integrity(
                                "changed account-holder wiki page is missing".into(),
                            )
                        })?;
                        let self_evidence_aliases =
                            load_self_evidence_aliases(&corpus, &me.citations)?;
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
            state.committed_wiki = Some(current_wiki);
            changed
        }
        OutputFormat::Python => {
            validate_python_format_commit(wiki_directory)?;
            Vec::new()
        }
        OutputFormat::Markdown => {
            validate_markdown_format_commit(wiki_directory)?;
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
        total_units: scoped_units.len(),
        complete: state.next_unit_index == scoped_units.len(),
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
    let (scope, scoped_units) = state.as_ref().map_or_else(
        || {
            (
                ResolvedPersonalMemoryScope::default(),
                all_corpus_unit_selections(&corpus),
            )
        },
        |state| effective_scope_and_units(state, &corpus),
    );
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
    let total = scoped_units.len();
    let selected_message_count = scoped_units
        .iter()
        .map(|unit| unit.message_count as u64)
        .sum();
    let committed_message_count = scoped_units[..next_unit_index]
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
        eligible_message_count: corpus.coverage.eligible_message_count,
        corpus_message_count: corpus.manifest.evidence_count,
        selected_message_count,
        source_coverage_complete: corpus.manifest.source_coverage_complete,
        row_coverage_complete: corpus.coverage.row_coverage_complete,
        content_complete: corpus.manifest.content_complete,
        unmatched_message_table_count: corpus.manifest.unmatched_message_table_count,
        limitation_codes: corpus.coverage.limitation_codes.clone(),
        delivery_order: corpus.manifest.delivery_order,
        scope: memory_scope_status(&scope, manifest_timezone(&corpus.manifest)?)?,
        completed_scope_count: state
            .as_ref()
            .map(|state| state.completed_scopes.len())
            .unwrap_or_default(),
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
        read_corpus_owner_file_limited(&manifest_path, MAXIMUM_CONTROL_FILE_BYTES)?;
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
    if manifest.corpus_mode == PersonalMemoryCorpusMode::AllMessages
        && !manifest
            .files
            .iter()
            .any(|record| record.relative_path == "conversations.jsonl")
    {
        return Err(RestoreError::Integrity(
            "canonical personal-memory corpus has no conversation selector sidecar".into(),
        ));
    }
    for record in &manifest.files {
        if !valid_sha256(&record.sha256) {
            return Err(RestoreError::Integrity(
                "corpus manifest contains an invalid file hash".into(),
            ));
        }
        let path = safe_corpus_path(&root, &record.relative_path)?;
        let metadata = corpus_owner_file_metadata(&path)?;
        if metadata.len() != record.byte_count {
            return Err(RestoreError::Integrity(format!(
                "corpus file {} has an unexpected byte count",
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
        read_corpus_owner_file_limited(&coverage_path, MAXIMUM_CONTROL_FILE_BYTES)?;
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
        || coverage.row_coverage_complete != manifest.row_coverage_complete
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
    if manifest.corpus_mode == PersonalMemoryCorpusMode::AllMessages
        && (coverage.selected_message_count != coverage.eligible_message_count
            || coverage.omitted_outside_time_range != 0
            || coverage.omitted_inactive_month != 0
            || coverage.omitted_silent_session != 0
            || coverage.omitted_context_bound != 0
            || coverage.omitted_filtered_conversation != 0)
    {
        return Err(RestoreError::Integrity(
            "canonical personal-memory corpus was pre-filtered before run-time scoping".into(),
        ));
    }
    if manifest.corpus_mode == PersonalMemoryCorpusMode::AllMessages
        && coverage.row_coverage_complete
            != (coverage.metadata_decode_failure_count == 0
                && !coverage.limitation_codes.iter().any(|code| {
                    matches!(
                        code.as_str(),
                        "messageTableInventoryUnavailable"
                            | "messageTableInventoryIncomplete"
                            | "corpusMetadataQueryFailed"
                            | "corpusMetadataRowFailed"
                            | "unsupportedCorpusMessageSchema"
                            | "shardUnavailable"
                            | "shardSchemaUnavailable"
                    )
                }))
    {
        return Err(RestoreError::Integrity(
            "canonical personal-memory row-coverage accounting is inconsistent".into(),
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
        read_corpus_owner_file_limited(&unit_index_path, MAXIMUM_UNIT_INDEX_BYTES)?;
    if unit_index_bytes.len() as u64 != unit_index_record.byte_count
        || sha256_bytes(&unit_index_bytes) != unit_index_record.sha256
    {
        return Err(RestoreError::Integrity(
            "prepared-unit index does not match the corpus manifest".into(),
        ));
    }
    let unit_index: UnitIndex = serde_json::from_slice(&unit_index_bytes)?;
    let legacy_unit_index = unit_index.schema == "greenbubbles.personal-memory-unit-index.v1"
        && unit_index.format_version == PERSONAL_MEMORY_FORMAT_VERSION;
    let compact_unit_index = unit_index.schema == "greenbubbles.personal-memory-unit-index.v2"
        && unit_index.format_version == 2;
    if (!legacy_unit_index && !compact_unit_index) || unit_index.units.len() != manifest.unit_count
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
        let expected_first_evidence_ordinal = evidence_count.saturating_add(1);
        let compact_metadata_valid = compact_unit_index
            && unit.evidence_aliases.is_empty()
            && unit.target_pages.is_empty()
            && unit.conversation_id.is_empty()
            && unit.first_evidence_ordinal == Some(expected_first_evidence_ordinal)
            && unit
                .person_aliases
                .iter()
                .all(|alias| valid_person_alias(alias))
            && unit.person_aliases.windows(2).all(|pair| pair[0] < pair[1])
            && unit
                .sender_aliases
                .iter()
                .all(|alias| valid_person_alias(alias))
            && unit.sender_aliases.windows(2).all(|pair| pair[0] < pair[1])
            && unit
                .sender_aliases
                .iter()
                .all(|alias| unit.person_aliases.binary_search(alias).is_ok());
        let legacy_metadata_valid = legacy_unit_index
            && unit.message_count == unit.evidence_aliases.len()
            && unit.person_aliases.is_empty()
            && unit.sender_aliases.is_empty()
            && !unit.has_account_holder_sender
            && !unit.has_unknown_sender
            && unit.first_evidence_ordinal.is_none();
        if unit.id != expected_id
            || !ids.insert(unit.id.clone())
            || !paths.insert(unit.relative_path.clone())
            || unit.relative_path != format!("batches/{}.json", unit.id)
            || !valid_sha256(&unit.sha256)
            || (!compact_metadata_valid && !legacy_metadata_valid)
            || unit.message_count == 0
            || unit.message_count > 1_000
            || unit.text_byte_count > 512 * 1024
            || manifest.corpus_mode == PersonalMemoryCorpusMode::AllMessages
                && (unit.conversation.is_empty()
                    || legacy_unit_index && unit.conversation_id.is_empty()
                    || unit.from > unit.to)
        {
            return Err(RestoreError::Integrity(
                "prepared-unit index contains an invalid or duplicate entry".into(),
            ));
        }
        safe_corpus_path(&root, &unit.relative_path)?;
        evidence_count = evidence_count.saturating_add(unit.message_count as u64);
        largest_unit_text_bytes = largest_unit_text_bytes.max(unit.text_byte_count);
        if legacy_unit_index {
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
    if let Some(link) = &manifest.extends {
        if !valid_sha256(&link.base_manifest_sha256) {
            return Err(RestoreError::Integrity(
                "corpus extends link contains an invalid base manifest hash".into(),
            ));
        }
        if link.carried_unit_count != link.first_new_unit_index {
            return Err(RestoreError::Integrity(
                "corpus extends link has inconsistent carried-unit and first-new-unit counts"
                    .into(),
            ));
        }
        if link.first_new_unit_index > manifest.unit_count {
            return Err(RestoreError::Integrity(
                "corpus extends link first-new-unit index exceeds total unit count".into(),
            ));
        }
        if link.carried_message_count > manifest.evidence_count {
            return Err(RestoreError::Integrity(
                "corpus extends link carried-message count exceeds total evidence count".into(),
            ));
        }
        if link.generation == 0 {
            return Err(RestoreError::Integrity(
                "corpus extends link generation must be at least 1".into(),
            ));
        }
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
    scope: &ResolvedPersonalMemoryScope,
    timezone: Tz,
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
        scope: memory_scope_output(scope, timezone)?,
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

fn all_corpus_unit_selections(corpus: &LoadedCorpus) -> Vec<ScopedUnitSelection> {
    corpus
        .unit_index
        .units
        .iter()
        .enumerate()
        .map(|(corpus_unit_index, entry)| ScopedUnitSelection {
            corpus_unit_index,
            all_messages: true,
            message_bitmap: None,
            message_count: entry.message_count,
            text_byte_count: entry.text_byte_count,
        })
        .collect()
}

fn effective_scope_and_units(
    state: &MemoryRunState,
    corpus: &LoadedCorpus,
) -> (ResolvedPersonalMemoryScope, Vec<ScopedUnitSelection>) {
    let scope = state.scope.clone().unwrap_or_default();
    let units = if state.scope.is_none()
        || scope_selects_all_evidence(&scope) && state.scoped_units.is_empty()
    {
        all_corpus_unit_selections(corpus)
    } else {
        state.scoped_units.clone()
    };
    (scope, units)
}

fn state_total_units(state: &MemoryRunState, corpus: &LoadedCorpus) -> usize {
    if state.scope.is_none()
        || state.scope.as_ref().is_some_and(scope_selects_all_evidence)
            && state.scoped_units.is_empty()
    {
        corpus.unit_index.units.len()
    } else {
        state.scoped_units.len()
    }
}

fn scope_unit_binding_hash(entry: &UnitIndexEntry, selection: &ScopedUnitSelection) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"greenbubbles-scope-unit-v1\0");
    hasher.update(entry.sha256.as_bytes());
    hasher.update((selection.corpus_unit_index as u64).to_le_bytes());
    hasher.update([u8::from(selection.all_messages)]);
    if let Some(bitmap) = &selection.message_bitmap {
        hasher.update(bitmap.as_bytes());
    }
    hasher.update((selection.message_count as u64).to_le_bytes());
    hasher.update((selection.text_byte_count as u64).to_le_bytes());
    hex::encode(hasher.finalize())
}

fn extend_memory_target_pages(
    target_pages: &mut BTreeSet<String>,
    scope: &ResolvedPersonalMemoryScope,
    unit: &PreparedUnitFile,
    entry: &UnitIndexEntry,
) {
    target_pages.insert("index.md".into());
    target_pages.insert(format!("conversations/{}.md", unit.c));
    match &scope.summary_subject {
        ResolvedMemorySummarySubject::AccountHolder => {
            target_pages.insert("me.md".into());
            target_pages.extend(
                unit_person_aliases(entry)
                    .into_iter()
                    .map(|alias| format!("people/{alias}.md")),
            );
            target_pages.extend(
                unit.m
                    .iter()
                    .filter_map(|message| message.p.as_ref())
                    .map(|alias| format!("people/{alias}.md")),
            );
        }
        ResolvedMemorySummarySubject::Person { alias } => {
            target_pages.insert(format!("people/{alias}.md"));
        }
        ResolvedMemorySummarySubject::None => {}
    }
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
    let evidence_aliases_match = if let Some(first) = entry.first_evidence_ordinal {
        entry.evidence_aliases.is_empty()
            && unit.m.iter().enumerate().all(|(offset, message)| {
                message.e == format!("E{:09}", first.saturating_add(offset as u64))
            })
    } else {
        unit.m
            .iter()
            .map(|message| message.e.as_str())
            .eq(entry.evidence_aliases.iter().map(String::as_str))
    };
    if unit.schema != PERSONAL_MEMORY_BATCH_SCHEMA
        || unit.id != entry.id
        || unit.m.len() != entry.message_count
        || unit.m.iter().map(|message| message.x.len()).sum::<usize>() != entry.text_byte_count
        || !evidence_aliases_match
    {
        return Err(RestoreError::Integrity(format!(
            "prepared unit {} has invalid internal accounting",
            entry.id
        )));
    }
    Ok(unit)
}

fn delivery_person_identity(record: &ContactSidecarRecord, alias: &str) -> DeliveryPersonIdentity {
    // The corpus normalises the account holder to the second person, which reads
    // as a stray pronoun in a third-person wiki and previously fell through to
    // the raw source id. The source id still travels beside this label.
    let display_name = if record.is_account_holder && record.display_name.trim() == "You" {
        ACCOUNT_HOLDER_DELIVERY_LABEL.to_string()
    } else {
        model_safe_person_label(record, alias)
    };
    DeliveryPersonIdentity {
        source_id: record.source_id.clone(),
        display_name,
        remark: record.remark.clone(),
        nickname: record.nickname.clone(),
        wechat_alias: record.wechat_alias.clone(),
    }
}

fn load_delivery_identities(
    corpus: &LoadedCorpus,
    requested_people: &BTreeSet<String>,
    requested_conversations: &BTreeSet<String>,
) -> Result<DeliveryIdentityIndex, RestoreError> {
    let contacts_path = verified_corpus_sidecar_path(corpus, "contacts.jsonl")?
        .ok_or_else(|| RestoreError::Integrity("corpus contacts sidecar is missing".into()))?;
    let mut account_holder = None;
    let mut people = BTreeMap::new();
    let reader = BufReader::new(File::open(contacts_path)?);
    for line in reader.lines() {
        let record: ContactSidecarRecord = serde_json::from_str(&line?)?;
        if record.is_account_holder
            && account_holder
                .replace(delivery_person_identity(&record, "self"))
                .is_some()
        {
            return Err(RestoreError::Integrity(
                "corpus contacts sidecar contains more than one account holder".into(),
            ));
        }
        if let Some(alias) = record.alias.as_deref() {
            if requested_people.contains(alias)
                && people
                    .insert(alias.to_string(), delivery_person_identity(&record, alias))
                    .is_some()
            {
                return Err(RestoreError::Integrity(
                    "corpus contacts sidecar repeats a requested person alias".into(),
                ));
            }
        }
    }
    let account_holder = account_holder.ok_or_else(|| {
        RestoreError::Integrity(
            "corpus contacts sidecar has no authenticated account-holder identity".into(),
        )
    })?;
    if people.keys().collect::<BTreeSet<_>>() != requested_people.iter().collect::<BTreeSet<_>>() {
        return Err(RestoreError::Integrity(
            "corpus contacts sidecar cannot resolve every requested person identity".into(),
        ));
    }

    let conversations_path = verified_corpus_sidecar_path(corpus, "conversations.jsonl")?
        .ok_or_else(|| RestoreError::Integrity("corpus conversations sidecar is missing".into()))?;
    let mut conversations = BTreeMap::new();
    for line in BufReader::new(File::open(conversations_path)?).lines() {
        let record: ConversationSidecarRecord = serde_json::from_str(&line?)?;
        if requested_conversations.contains(&record.alias)
            && conversations
                .insert(
                    record.alias.clone(),
                    DeliveryConversationIdentity {
                        source_id: record.source_id,
                        title: if record.display_name.trim().is_empty() {
                            record.alias
                        } else {
                            record.display_name
                        },
                        kind: record.kind,
                    },
                )
                .is_some()
        {
            return Err(RestoreError::Integrity(
                "corpus conversations sidecar repeats a requested conversation alias".into(),
            ));
        }
    }
    if conversations.keys().collect::<BTreeSet<_>>()
        != requested_conversations.iter().collect::<BTreeSet<_>>()
    {
        return Err(RestoreError::Integrity(
            "corpus conversations sidecar cannot resolve every requested conversation identity"
                .into(),
        ));
    }
    Ok(DeliveryIdentityIndex {
        account_holder,
        people,
        conversations,
    })
}

#[derive(Debug, Default)]
struct ScopeSelectorMaps {
    conversations: BTreeMap<String, String>,
    senders: BTreeMap<String, String>,
}

fn insert_scope_selector(
    map: &mut BTreeMap<String, String>,
    selector: &str,
    alias: &str,
    description: &str,
) -> Result<(), RestoreError> {
    if selector.is_empty() || alias.is_empty() {
        return Ok(());
    }
    if let Some(existing) = map.insert(selector.to_string(), alias.to_string()) {
        if existing != alias {
            return Err(RestoreError::Integrity(format!(
                "personal-memory {description} selector is ambiguous"
            )));
        }
    }
    Ok(())
}

fn verified_corpus_sidecar_path(
    corpus: &LoadedCorpus,
    relative_path: &str,
) -> Result<Option<PathBuf>, RestoreError> {
    let Some(record) = corpus
        .manifest
        .files
        .iter()
        .find(|record| record.relative_path == relative_path)
    else {
        return Ok(None);
    };
    let path = safe_corpus_path(&corpus.root, relative_path)?;
    let metadata = immutable_owner_file_metadata(&path)?;
    if metadata.len() != record.byte_count || sha256_file(&path)? != record.sha256 {
        return Err(RestoreError::Integrity(format!(
            "corpus {relative_path} sidecar no longer matches the immutable manifest"
        )));
    }
    Ok(Some(path))
}

fn load_scope_selector_maps(corpus: &LoadedCorpus) -> Result<ScopeSelectorMaps, RestoreError> {
    let mut maps = ScopeSelectorMaps::default();
    if let Some(path) = verified_corpus_sidecar_path(corpus, "contacts.jsonl")? {
        for line in BufReader::new(File::open(path)?).lines() {
            let record: ContactSidecarRecord = serde_json::from_str(&line?)?;
            let Some(alias) = record.alias.as_deref() else {
                continue;
            };
            insert_scope_selector(&mut maps.senders, alias, alias, "sender")?;
            insert_scope_selector(&mut maps.senders, &record.source_id, alias, "sender")?;
        }
    }
    if let Some(path) = verified_corpus_sidecar_path(corpus, "conversations.jsonl")? {
        for line in BufReader::new(File::open(path)?).lines() {
            let record: ConversationSidecarRecord = serde_json::from_str(&line?)?;
            insert_scope_selector(
                &mut maps.conversations,
                &record.alias,
                &record.alias,
                "conversation",
            )?;
            insert_scope_selector(
                &mut maps.conversations,
                &record.source_id,
                &record.alias,
                "conversation",
            )?;
        }
    }
    for entry in &corpus.unit_index.units {
        if !entry.conversation.is_empty() {
            insert_scope_selector(
                &mut maps.conversations,
                &entry.conversation,
                &entry.conversation,
                "conversation",
            )?;
            insert_scope_selector(
                &mut maps.conversations,
                &entry.conversation_id,
                &entry.conversation,
                "conversation",
            )?;
        } else {
            let unit = load_unit(corpus, entry)?;
            insert_scope_selector(&mut maps.conversations, &unit.c, &unit.c, "conversation")?;
            for message in &unit.m {
                if let Some(alias) = &message.p {
                    insert_scope_selector(&mut maps.senders, alias, alias, "sender")?;
                }
            }
        }
    }
    Ok(maps)
}

fn resolve_personal_memory_scope(
    corpus: &LoadedCorpus,
    requested: &PersonalMemoryScopeOptions,
) -> Result<(ResolvedPersonalMemoryScope, String), RestoreError> {
    let (not_before_unix, not_after_unix) = requested.validate_shape()?;
    let maps = load_scope_selector_maps(corpus)?;
    let mut conversation_aliases = BTreeSet::new();
    for selector in &requested.conversation_selectors {
        let alias = maps
            .conversations
            .get(selector.trim())
            .cloned()
            .ok_or_else(|| {
                RestoreError::Integrity(
                    "personal-memory scope names an unknown conversation selector".into(),
                )
            })?;
        if !conversation_aliases.insert(alias) {
            return Err(RestoreError::Integrity(
                "conversationSelectors resolves the same conversation more than once".into(),
            ));
        }
    }
    let mut sender_aliases = BTreeSet::new();
    let mut include_account_holder_sender = false;
    let mut include_unknown_sender = false;
    for selector in &requested.sender_selectors {
        match selector.trim() {
            "self" | "accountHolder" => {
                if include_account_holder_sender {
                    return Err(RestoreError::Integrity(
                        "senderSelectors resolves the account holder more than once".into(),
                    ));
                }
                include_account_holder_sender = true;
            }
            "unknown" => {
                if include_unknown_sender {
                    return Err(RestoreError::Integrity(
                        "senderSelectors resolves the unknown sender more than once".into(),
                    ));
                }
                include_unknown_sender = true;
            }
            selector => {
                let alias = maps.senders.get(selector).cloned().ok_or_else(|| {
                    RestoreError::Integrity(
                        "personal-memory scope names an unknown sender selector".into(),
                    )
                })?;
                if !sender_aliases.insert(alias) {
                    return Err(RestoreError::Integrity(
                        "senderSelectors resolves the same sender more than once".into(),
                    ));
                }
            }
        }
    }
    let summary_subject = match &requested.summary_subject {
        PersonalMemorySummarySubjectSelector::AccountHolder => {
            ResolvedMemorySummarySubject::AccountHolder
        }
        PersonalMemorySummarySubjectSelector::None => ResolvedMemorySummarySubject::None,
        PersonalMemorySummarySubjectSelector::Person { selector } => {
            let alias = maps.senders.get(selector.trim()).cloned().ok_or_else(|| {
                RestoreError::Integrity(
                    "summarySubject names a person unavailable in the canonical corpus".into(),
                )
            })?;
            ResolvedMemorySummarySubject::Person { alias }
        }
    };
    let resolved = ResolvedPersonalMemoryScope {
        conversation_aliases,
        conversation_kinds: requested.conversation_kinds.iter().copied().collect(),
        not_before_unix,
        not_after_unix,
        sender_aliases,
        include_account_holder_sender,
        include_unknown_sender,
        summary_subject,
    };
    let digest = sha256_bytes(&serde_json::to_vec(&resolved)?);
    Ok((resolved, digest))
}

fn scope_has_sender_filter(scope: &ResolvedPersonalMemoryScope) -> bool {
    scope.include_account_holder_sender
        || scope.include_unknown_sender
        || !scope.sender_aliases.is_empty()
}

fn scope_selects_all_evidence(scope: &ResolvedPersonalMemoryScope) -> bool {
    scope.conversation_aliases.is_empty()
        && scope.conversation_kinds.is_empty()
        && scope.not_before_unix.is_none()
        && scope.not_after_unix.is_none()
        && !scope_has_sender_filter(scope)
}

fn scope_matches_message(
    scope: &ResolvedPersonalMemoryScope,
    conversation_alias: &str,
    message: &CompactMessage,
) -> bool {
    if !scope.conversation_aliases.is_empty()
        && !scope.conversation_aliases.contains(conversation_alias)
    {
        return false;
    }
    if scope
        .not_before_unix
        .is_some_and(|not_before| message.t < not_before)
        || scope
            .not_after_unix
            .is_some_and(|not_after| message.t > not_after)
    {
        return false;
    }
    if !scope_has_sender_filter(scope) {
        return true;
    }
    match message.a.as_str() {
        "self" => scope.include_account_holder_sender,
        "unknown" => scope.include_unknown_sender,
        "other" => message
            .p
            .as_ref()
            .is_some_and(|alias| scope.sender_aliases.contains(alias)),
        _ => false,
    }
}

fn load_conversation_contact_kinds(
    corpus: &LoadedCorpus,
) -> Result<BTreeMap<String, ContactKind>, RestoreError> {
    let mut kinds = BTreeMap::new();
    if let Some(path) = verified_corpus_sidecar_path(corpus, "conversations.jsonl")? {
        for line in BufReader::new(File::open(path)?).lines() {
            let record: ConversationSidecarRecord = serde_json::from_str(&line?)?;
            if kinds.insert(record.alias, record.contact_kind).is_some() {
                return Err(RestoreError::Integrity(
                    "corpus conversation sidecar repeats a stable alias".into(),
                ));
            }
        }
    }
    Ok(kinds)
}

fn scope_matches_conversation_kind(
    scope: &ResolvedPersonalMemoryScope,
    conversation_alias: &str,
    kind: ConversationKind,
    contact_kinds: &BTreeMap<String, ContactKind>,
) -> bool {
    if scope.conversation_kinds.is_empty() {
        return true;
    }
    let contact_kind = contact_kinds.get(conversation_alias).copied();
    scope
        .conversation_kinds
        .iter()
        .any(|selector| match selector {
            PersonalMemoryConversationKindSelector::Direct => kind == ConversationKind::Direct,
            PersonalMemoryConversationKindSelector::Group => kind == ConversationKind::Group,
            PersonalMemoryConversationKindSelector::Official => {
                contact_kind == Some(ContactKind::Official) || kind == ConversationKind::Business
            }
            PersonalMemoryConversationKindSelector::Service => {
                contact_kind == Some(ContactKind::Service)
            }
        })
}

fn select_scope_units(
    corpus: &LoadedCorpus,
    scope: &ResolvedPersonalMemoryScope,
) -> Result<Vec<ScopedUnitSelection>, RestoreError> {
    let mut selected = Vec::new();
    let contact_kinds = if scope.conversation_kinds.is_empty() {
        BTreeMap::new()
    } else {
        load_conversation_contact_kinds(corpus)?
    };
    for (corpus_unit_index, entry) in corpus.unit_index.units.iter().enumerate() {
        if !scope.conversation_aliases.is_empty()
            && !entry.conversation.is_empty()
            && !scope.conversation_aliases.contains(&entry.conversation)
        {
            continue;
        }
        if scope
            .not_before_unix
            .is_some_and(|not_before| entry.to != 0 && entry.to < not_before)
            || scope
                .not_after_unix
                .is_some_and(|not_after| entry.from != 0 && entry.from > not_after)
        {
            continue;
        }
        // Compact v2 indexes carry privacy-minimized sender-presence metadata.
        // Use it only as a safe negative filter; v1 indexes have no such fields
        // and therefore fall through to exact unit inspection.
        if scope_has_sender_filter(scope)
            && entry.first_evidence_ordinal.is_some()
            && !(scope.include_account_holder_sender && entry.has_account_holder_sender
                || scope.include_unknown_sender && entry.has_unknown_sender
                || entry
                    .sender_aliases
                    .iter()
                    .any(|alias| scope.sender_aliases.contains(alias)))
        {
            continue;
        }
        let unit = load_unit(corpus, entry)?;
        if !scope_matches_conversation_kind(scope, &unit.c, unit.kind, &contact_kinds) {
            continue;
        }
        let message_offsets = unit
            .m
            .iter()
            .enumerate()
            .filter_map(|(offset, message)| {
                scope_matches_message(scope, &unit.c, message).then_some(offset)
            })
            .collect::<Vec<_>>();
        if message_offsets.is_empty() {
            continue;
        }
        let all_messages = message_offsets.len() == unit.m.len();
        let text_byte_count = message_offsets
            .iter()
            .map(|offset| unit.m[*offset].x.len())
            .sum();
        selected.push(ScopedUnitSelection {
            corpus_unit_index,
            all_messages,
            message_count: message_offsets.len(),
            text_byte_count,
            message_bitmap: (!all_messages).then(|| {
                let mut bitmap = vec![0_u8; unit.m.len().div_ceil(8)];
                for offset in &message_offsets {
                    bitmap[*offset / 8] |= 1 << (*offset % 8);
                }
                hex::encode(bitmap)
            }),
        });
    }
    Ok(selected)
}

fn load_scoped_unit(
    corpus: &LoadedCorpus,
    selection: &ScopedUnitSelection,
) -> Result<PreparedUnitFile, RestoreError> {
    let entry = corpus
        .unit_index
        .units
        .get(selection.corpus_unit_index)
        .ok_or_else(|| RestoreError::Integrity("scope references a missing corpus unit".into()))?;
    let mut unit = load_unit(corpus, entry)?;
    if selection.all_messages {
        if selection.message_bitmap.is_some()
            || selection.message_count != unit.m.len()
            || selection.text_byte_count
                != unit.m.iter().map(|message| message.x.len()).sum::<usize>()
        {
            return Err(RestoreError::Integrity(
                "scope all-message unit accounting is inconsistent".into(),
            ));
        }
    } else {
        let bitmap = selection
            .message_bitmap
            .as_ref()
            .ok_or_else(|| RestoreError::Integrity("partial scope unit has no bitmap".into()))?;
        let bitmap = hex::decode(bitmap).map_err(|_| {
            RestoreError::Integrity("scope contains a malformed message bitmap".into())
        })?;
        if bitmap.len() != unit.m.len().div_ceil(8)
            || unit.m.len() % 8 != 0
                && bitmap.last().is_some_and(|byte| {
                    let used = unit.m.len() % 8;
                    *byte & !((1_u8 << used) - 1) != 0
                })
        {
            return Err(RestoreError::Integrity(
                "scope contains an invalid message bitmap".into(),
            ));
        }
        unit.m = unit
            .m
            .into_iter()
            .enumerate()
            .filter_map(|(offset, message)| {
                (bitmap[offset / 8] & (1 << (offset % 8)) != 0).then_some(message)
            })
            .collect();
        if selection.message_count != unit.m.len()
            || selection.text_byte_count
                != unit.m.iter().map(|message| message.x.len()).sum::<usize>()
        {
            return Err(RestoreError::Integrity(
                "scope filtered-message byte accounting is inconsistent".into(),
            ));
        }
    }
    unit.from = unit.m.first().map(|message| message.t).unwrap_or_default();
    unit.to = unit.m.last().map(|message| message.t).unwrap_or_default();
    Ok(unit)
}

/// The alias is the first field the sidecar writer emits, so a commit can decide
/// whether a line is worth decoding without paying `serde_json` for it. Anything
/// that is not exactly that shape returns `None` and is parsed in full rather
/// than skipped, so an unexpected sidecar still fails loudly.
fn evidence_line_alias(line: &[u8]) -> Option<&str> {
    let rest = line.strip_prefix(b"{\"alias\":\"")?;
    let end = rest.iter().position(|byte| *byte == b'"')?;
    if rest.get(end.saturating_add(1)) != Some(&b',') {
        return None;
    }
    let alias = std::str::from_utf8(&rest[..end]).ok()?;
    let mut characters = alias.chars();
    if characters.next() != Some('E') {
        return None;
    }
    let digits = characters.as_str();
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(alias)
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
    let metadata = corpus_owner_file_metadata(&path)?;
    if metadata.len() != record.byte_count {
        return Err(RestoreError::Integrity(
            "corpus evidence sidecar has an unexpected byte count".into(),
        ));
    }
    // The default 8-KiB buffer costs a read syscall every nine records across a
    // gigabyte-scale sidecar.
    let mut reader = BufReader::with_capacity(1 << 20, File::open(path)?);
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
        // The sidecar is over a gigabyte at corpus scale and a commit cites a
        // handful of aliases, so only the cited lines are decoded. Every byte is
        // still hashed, which is what actually binds the file to its manifest.
        if evidence_line_alias(&line).is_some_and(|alias| !requested.contains(alias)) {
            continue;
        }
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
    scope_options: Option<&PersonalMemoryScopeOptions>,
) -> Result<MemoryRunState, RestoreError> {
    if state_path.try_exists()? {
        let mut state = load_existing_state(state_path, corpus)?;
        if let Some(scope_options) = scope_options {
            let (requested, requested_hash) = resolve_personal_memory_scope(corpus, scope_options)?;
            if state.scope.as_ref() != Some(&requested)
                || state.scope_sha256.as_deref() != Some(requested_hash.as_str())
            {
                let current_total = state_total_units(&state, corpus);
                if state.outstanding.is_some() || state.next_unit_index != current_total {
                    return Err(RestoreError::Integrity(
                        "personal-memory state cannot change scope before the current scope is complete"
                            .into(),
                    ));
                }
                if state.completed_scopes.len() >= MAXIMUM_SCOPE_SELECTORS {
                    return Err(RestoreError::Integrity(
                        "personal-memory state completed-scope history exceeds its fixed limit"
                            .into(),
                    ));
                }
                if let (Some(scope_sha256), Some(message_count)) =
                    (state.scope_sha256.clone(), state.scoped_message_count)
                {
                    state.completed_scopes.push(CompletedMemoryScope {
                        scope_sha256,
                        unit_count: current_total,
                        message_count,
                        completed_at_unix_milliseconds: now_unix_milliseconds()?,
                    });
                }
                let (scoped_units, scoped_message_count) = if scope_selects_all_evidence(&requested)
                {
                    (Vec::new(), corpus.manifest.evidence_count)
                } else {
                    let units = select_scope_units(corpus, &requested)?;
                    let message_count = units.iter().map(|unit| unit.message_count as u64).sum();
                    (units, message_count)
                };
                state.scope = Some(requested);
                state.scope_sha256 = Some(requested_hash);
                state.scoped_units = scoped_units;
                state.scoped_message_count = Some(scoped_message_count);
                state.next_unit_index = 0;
                state.last_committed = None;
                state.updated_at_unix_milliseconds = now_unix_milliseconds()?;
                write_state_atomic(state_path, &state)?;
            }
        }
        return Ok(state);
    }
    let (scope, scope_sha256) = if let Some(scope_options) = scope_options {
        resolve_personal_memory_scope(corpus, scope_options)?
    } else {
        let scope = ResolvedPersonalMemoryScope::default();
        let scope_sha256 = sha256_bytes(&serde_json::to_vec(&scope)?);
        (scope, scope_sha256)
    };
    let (scoped_units, scoped_message_count) = if scope_selects_all_evidence(&scope) {
        (Vec::new(), corpus.manifest.evidence_count)
    } else {
        let units = select_scope_units(corpus, &scope)?;
        let message_count = units.iter().map(|unit| unit.message_count as u64).sum();
        (units, message_count)
    };
    let now = now_unix_milliseconds()?;
    let state = MemoryRunState {
        schema: PERSONAL_MEMORY_STATE_SCHEMA.into(),
        format_version: PERSONAL_MEMORY_FORMAT_VERSION,
        corpus_manifest_sha256: corpus.manifest_sha256.clone(),
        scope: Some(scope),
        scope_sha256: Some(scope_sha256),
        scoped_units,
        scoped_message_count: Some(scoped_message_count),
        completed_scopes: Vec::new(),
        output_format: OutputFormat::Wiki,
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
    let total_units = state_total_units(&state, corpus);
    if state.schema != PERSONAL_MEMORY_STATE_SCHEMA
        || state.format_version != PERSONAL_MEMORY_FORMAT_VERSION
        || state.corpus_manifest_sha256 != corpus.manifest_sha256
        || state.next_unit_index > total_units
        || state.created_at_unix_milliseconds > state.updated_at_unix_milliseconds
    {
        return Err(RestoreError::Integrity(
            "memory state does not belong to this immutable corpus or is inconsistent".into(),
        ));
    }
    if state.completed_scopes.len() > MAXIMUM_SCOPE_SELECTORS
        || state.completed_scopes.iter().any(|completed| {
            !valid_sha256(&completed.scope_sha256)
                || completed.completed_at_unix_milliseconds < state.created_at_unix_milliseconds
                || completed.completed_at_unix_milliseconds > state.updated_at_unix_milliseconds
        })
    {
        return Err(RestoreError::Integrity(
            "memory state completed-scope history is invalid".into(),
        ));
    }
    if let Some(scope) = &state.scope {
        let expected_scope_hash = sha256_bytes(&serde_json::to_vec(scope)?);
        if state.scope_sha256.as_deref() != Some(expected_scope_hash.as_str()) {
            return Err(RestoreError::Integrity(
                "memory state scope hash is inconsistent".into(),
            ));
        }
        let compact_all_messages =
            scope_selects_all_evidence(scope) && state.scoped_units.is_empty();
        let mut previous_corpus_index = None;
        let mut scoped_message_count = if compact_all_messages {
            corpus.manifest.evidence_count
        } else {
            0
        };
        for selection in &state.scoped_units {
            let Some(entry) = corpus.unit_index.units.get(selection.corpus_unit_index) else {
                return Err(RestoreError::Integrity(
                    "memory state scope references a missing corpus unit".into(),
                ));
            };
            if previous_corpus_index.is_some_and(|previous| previous >= selection.corpus_unit_index)
            {
                return Err(RestoreError::Integrity(
                    "memory state scoped units are not in canonical order".into(),
                ));
            }
            previous_corpus_index = Some(selection.corpus_unit_index);
            let valid_selection = if selection.all_messages {
                selection.message_bitmap.is_none()
                    && selection.message_count == entry.message_count
                    && selection.text_byte_count == entry.text_byte_count
            } else if let Some(bitmap) = &selection.message_bitmap {
                hex::decode(bitmap).is_ok_and(|bytes| {
                    bytes.len() == entry.message_count.div_ceil(8)
                        && bytes
                            .iter()
                            .map(|byte| byte.count_ones() as usize)
                            .sum::<usize>()
                            == selection.message_count
                        && selection.message_count > 0
                        && selection.message_count < entry.message_count
                        && selection.text_byte_count <= entry.text_byte_count
                        && (entry.message_count % 8 == 0
                            || bytes.last().is_some_and(|byte| {
                                let used = entry.message_count % 8;
                                *byte & !((1_u8 << used) - 1) == 0
                            }))
                })
            } else {
                false
            };
            if !valid_selection {
                return Err(RestoreError::Integrity(
                    "memory state contains an invalid scoped unit selection".into(),
                ));
            }
            scoped_message_count =
                scoped_message_count.saturating_add(selection.message_count as u64);
        }
        if state.scoped_message_count != Some(scoped_message_count) {
            return Err(RestoreError::Integrity(
                "memory state scoped message accounting is inconsistent".into(),
            ));
        }
    } else if state.scope_sha256.is_some()
        || !state.scoped_units.is_empty()
        || state.scoped_message_count.is_some()
    {
        return Err(RestoreError::Integrity(
            "legacy memory state contains partial scope metadata".into(),
        ));
    }
    if let Some(outstanding) = &state.outstanding {
        if outstanding.start_unit_index != state.next_unit_index
            || outstanding.start_unit_index >= outstanding.end_unit_index_exclusive
            || outstanding.end_unit_index_exclusive > total_units
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
    let mut bytes = serde_json::to_vec(state)?;
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

/// Like `immutable_owner_file_metadata` but also accepts owner-writable 0600 files,
/// which are produced by `memory prepare --extend` to allow future extend runs.
fn corpus_owner_file_metadata(path: &Path) -> Result<fs::Metadata, RestoreError> {
    let metadata = fs::symlink_metadata(path)?;
    let mode = metadata.permissions().mode() & 0o777;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || (mode != 0o400 && mode != 0o600)
        || metadata.nlink() != 1
    {
        return Err(RestoreError::Integrity(
            "corpus files must be current-user-owned regular files with 0400 or 0600 permissions"
                .into(),
        ));
    }
    Ok(metadata)
}

fn read_corpus_owner_file_limited(
    path: &Path,
    maximum_bytes: u64,
) -> Result<Vec<u8>, RestoreError> {
    let metadata = corpus_owner_file_metadata(path)?;
    if metadata.len() > maximum_bytes {
        return Err(RestoreError::Integrity(format!(
            "corpus file exceeds the fixed {maximum_bytes}-byte safety limit"
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum_bytes {
        return Err(RestoreError::Integrity(format!(
            "corpus file exceeds the fixed {maximum_bytes}-byte safety limit"
        )));
    }
    Ok(bytes)
}

fn protect_extendable_corpus_tree(root: &Path) -> Result<(), RestoreError> {
    for entry in walkdir::WalkDir::new(root)
        .contents_first(true)
        .follow_links(false)
    {
        let entry = entry.map_err(|_| {
            RestoreError::Integrity(
                "prepared extend-corpus tree could not be finalized safely".into(),
            )
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
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o600))?;
        } else if metadata.is_dir() {
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o700))?;
        } else {
            return Err(RestoreError::Integrity(
                "prepared corpus tree contains a non-file, non-directory entry".into(),
            ));
        }
    }
    Ok(())
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
        let relative_display = entry
            .path()
            .strip_prefix(&root)
            .map(|relative| relative.to_string_lossy().into_owned())
            .unwrap_or_else(|_| entry.path().to_string_lossy().into_owned());
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(RestoreError::Integrity(format!(
                "wiki entry {relative_display} must be current-user-owned and may not be a symbolic link"
            )));
        }
        if metadata.is_dir() {
            let mode = metadata.permissions().mode() & 0o7777;
            if mode & 0o077 != 0 {
                return Err(RestoreError::Integrity(format!(
                    "wiki directory {relative_display} is mode {mode:04o}; wiki directories must be owner-only (chmod 700)"
                )));
            }
            continue;
        }
        let mode = metadata.permissions().mode() & 0o7777;
        if !metadata.is_file() || mode & 0o077 != 0 || metadata.nlink() != 1 {
            return Err(RestoreError::Integrity(format!(
                "wiki file {relative_display} is mode {mode:04o} with {} link(s); wiki files must be owner-only (chmod 600), singly linked regular files",
                metadata.nlink()
            )));
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
            return Err(RestoreError::Integrity(format!(
                "wiki entry {relative} is not Markdown; a wiki may contain only owner-only .md files and directories"
            )));
        }
        if metadata.len() > MAXIMUM_WIKI_FILE_BYTES {
            return Err(RestoreError::Integrity(format!(
                "wiki Markdown file {relative} is {} bytes and exceeds the fixed {MAXIMUM_WIKI_FILE_BYTES}-byte safety limit; split it into smaller pages",
                metadata.len()
            )));
        }
        let bytes = read_owner_file_limited(entry.path(), MAXIMUM_WIKI_FILE_BYTES)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            RestoreError::Integrity(format!("wiki Markdown file {relative} must be valid UTF-8"))
        })?;
        let prose_lines = markdown_prose_lines(text);
        snapshot.insert(
            relative,
            WikiFileSnapshot {
                sha256: sha256_bytes(&bytes),
                citations: extract_evidence_aliases(text),
                has_prose: !prose_lines.is_empty(),
                uncited_prose_line_count: uncited_prose_lines(&prose_lines).len(),
                excessive_citation_prose_line_count: excessive_citation_prose_lines(&prose_lines)
                    .len(),
                prose_lines,
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
    let mut problems = Vec::<String>::new();
    if let Some(before) = before {
        let deleted = before
            .keys()
            .filter(|path| !current.contains_key(*path))
            .cloned()
            .collect::<Vec<_>>();
        if !deleted.is_empty() {
            problems.push(format!(
                "memory commit may not delete wiki page(s) {}",
                format_bounded_list(deleted)
            ));
        }
    }
    for (path, record) in current {
        let previous_record = before.and_then(|before| before.get(path));
        let changed_file = previous_record.is_none_or(|previous| previous.sha256 != record.sha256);
        if changed_file {
            if !target_pages.contains(path) {
                problems.push(format!(
                    "memory commit changed non-target wiki page {path}; declare it in targetPages or restore it"
                ));
            }
            changed.push(path.clone());
            if path != "index.md" {
                changed_factual_page_count = changed_factual_page_count.saturating_add(1);
                if !record.has_prose {
                    problems.push(format!(
                        "changed factual wiki page {path} has no prose; empty or heading-only placeholders cannot advance memory"
                    ));
                }
                if record.citations.is_empty() {
                    problems.push(format!(
                        "changed factual wiki page {path} has no evidence alias"
                    ));
                }
                let uncited = uncited_prose_lines(&record.prose_lines);
                if !uncited.is_empty() {
                    problems.push(format!(
                        "changed factual wiki page {path} has {} uncited prose line(s) at line {}",
                        uncited.len(),
                        format_line_numbers(&uncited)
                    ));
                }
                let excessive = excessive_citation_prose_lines(&record.prose_lines);
                if !excessive.is_empty() {
                    problems.push(format!(
                        "changed factual wiki page {path} has {} prose line(s) with more than {MAXIMUM_WIKI_CITATIONS_PER_PROSE_LINE} citations at line {}; retain a representative evidence set instead of citation dumping",
                        excessive.len(),
                        format_line_numbers(&excessive)
                    ));
                }
            }
        }
        let mut unknown = Vec::new();
        let mut introduced = Vec::new();
        for alias in &record.citations {
            if !valid_evidence_alias(alias, evidence_count) {
                unknown.push(alias.clone());
                continue;
            }
            if changed_file
                && !batch_aliases.contains(alias)
                && previous_record.is_none_or(|previous| !previous.citations.contains(alias))
            {
                introduced.push(alias.clone());
            }
            if changed_file && path != "index.md" && batch_aliases.contains(alias) {
                current_batch_cited = true;
            }
        }
        if !unknown.is_empty() {
            problems.push(format!(
                "wiki page {path} cites unknown evidence alias(es) {}",
                format_bounded_list(unknown)
            ));
        }
        if !introduced.is_empty() {
            problems.push(format!(
                "wiki page {path} introduced evidence alias(es) {} that are absent from this batch and from that same prior page",
                format_bounded_list(introduced)
            ));
        }
    }
    let cited_aliases = current
        .values()
        .flat_map(|record| record.citations.iter().cloned())
        .collect::<BTreeSet<_>>();
    let retained_but_uncited = batch_aliases
        .difference(&cited_aliases)
        .cloned()
        .collect::<Vec<_>>();
    if !retained_but_uncited.is_empty() {
        problems.push(format!(
            "memory commit retained {} evidence alias(es) that no durable wiki page cites: {}",
            retained_but_uncited.len(),
            format_bounded_list(retained_but_uncited)
        ));
    }
    if changed_factual_page_count == 0 || !current_batch_cited {
        problems.push(
            "memory commit requires at least one changed non-index page with prose and an evidence alias from the outstanding batch"
                .to_string(),
        );
    }
    if !problems.is_empty() {
        return Err(wiki_validation_error(problems));
    }
    changed.sort();
    Ok(changed)
}

fn validate_me_self_attribution(
    me: &WikiFileSnapshot,
    self_evidence_aliases: &BTreeSet<String>,
) -> Result<(), RestoreError> {
    let incoming_only = me
        .prose_lines
        .iter()
        .filter(|prose| prose.citations.is_disjoint(self_evidence_aliases))
        .map(|prose| prose.number)
        .collect::<Vec<_>>();
    if !incoming_only.is_empty() {
        return Err(RestoreError::Integrity(format!(
            "changed account-holder wiki page has {} factual prose line(s) without a self-authored citation at line {}; incoming-only facts belong on a person page or must be explicitly supported by account-holder evidence",
            incoming_only.len(),
            format_line_numbers(&incoming_only)
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

/// Validate a Python-format output directory at `memory commit` time.
///
/// Checks:
/// - `manifest.py` exists and is a regular, owner-only file.
/// - Every `.py` file in the tree parses as valid Python (via `python3 -c "import ast;
///   ast.parse(open('<file>').read())"`).
/// - No file outside the allowed extensions (`.py`, `.txt`, `.json`, `.md`) is present.
fn validate_python_format_commit(output_directory: &Path) -> Result<(), RestoreError> {
    let root = fs::canonicalize(output_directory).map_err(|_| {
        RestoreError::Integrity("Python output directory does not exist or cannot be read".into())
    })?;
    let manifest_path = root.join("manifest.py");
    if !manifest_path.try_exists().unwrap_or(false) {
        return Err(RestoreError::Integrity(
            "Python format commit requires manifest.py in the output directory".into(),
        ));
    }
    ensure_private_directory(&root)?;
    const ALLOWED_EXTENSIONS: &[&str] = &["py", "txt", "json", "md"];
    let mut py_files: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(&root).follow_links(false) {
        let entry = entry.map_err(|_| {
            RestoreError::Integrity("Python output directory could not be traversed".into())
        })?;
        if entry.path() == root || entry.file_type().is_dir() {
            continue;
        }
        // Skip hidden files and files inside hidden directories (.git/, .gitignore,
        // .greenbubbles-runs/).  These are tool/VCS metadata, not user output.
        // Compare the path *relative* to root so that hidden components in the
        // system temp dir (outside our control) are not considered.
        let relative_hidden = entry
            .path()
            .strip_prefix(&root)
            .unwrap_or(entry.path())
            .components()
            .any(|c| {
                c.as_os_str()
                    .to_str()
                    .map_or(false, |s| s.starts_with('.'))
            });
        if relative_hidden {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            return Err(RestoreError::Integrity(
                "Python output directory must not contain symbolic links".into(),
            ));
        }
        let ext = entry
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !ALLOWED_EXTENSIONS.contains(&ext) {
            let relative = entry
                .path()
                .strip_prefix(&root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| entry.path().to_string_lossy().into_owned());
            return Err(RestoreError::Integrity(format!(
                "Python format output contains a file with a disallowed extension: {relative}"
            )));
        }
        if ext == "py" {
            py_files.push(entry.path().to_path_buf());
        }
    }
    // Syntax-check every .py file.
    for py_path in &py_files {
        let status = std::process::Command::new("python3")
            .arg("-c")
            .arg(format!(
                "import ast; ast.parse(open({}).read())",
                serde_json::to_string(&py_path.to_string_lossy().into_owned()).unwrap_or_default()
            ))
            .status()
            .map_err(|_| {
                RestoreError::Integrity(
                    "Python format commit requires python3 to be available for syntax checking"
                        .into(),
                )
            })?;
        if !status.success() {
            let relative = py_path
                .strip_prefix(&root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| py_path.to_string_lossy().into_owned());
            return Err(RestoreError::Integrity(format!(
                "Python format file {relative} failed syntax check"
            )));
        }
    }
    Ok(())
}

/// Validate a Markdown-format output directory at `memory commit` time.
///
/// Checks:
/// - `manifest.md` exists in the output directory.
/// - Every `domains/*.md` file contains the three required section headings:
///   `## Schema`, `## State`, and `## History`.
fn validate_markdown_format_commit(output_directory: &Path) -> Result<(), RestoreError> {
    let root = fs::canonicalize(output_directory).map_err(|_| {
        RestoreError::Integrity("Markdown output directory does not exist or cannot be read".into())
    })?;
    let manifest_path = root.join("manifest.md");
    if !manifest_path.try_exists().unwrap_or(false) {
        return Err(RestoreError::Integrity(
            "Markdown format commit requires manifest.md in the output directory".into(),
        ));
    }
    let domains_path = root.join("domains");
    if domains_path.try_exists().unwrap_or(false) {
        for entry in walkdir::WalkDir::new(&domains_path)
            .max_depth(1)
            .follow_links(false)
        {
            let entry = entry.map_err(|_| {
                RestoreError::Integrity("Markdown domains directory could not be traversed".into())
            })?;
            if entry.path() == domains_path || entry.file_type().is_dir() {
                continue;
            }
            if entry
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                != "md"
            {
                continue;
            }
            let bytes = read_owner_file_limited(entry.path(), MAXIMUM_WIKI_FILE_BYTES)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| {
                let relative = entry
                    .path()
                    .strip_prefix(&root)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| entry.path().to_string_lossy().into_owned());
                RestoreError::Integrity(format!(
                    "Markdown domains file {relative} must be valid UTF-8"
                ))
            })?;
            let relative = entry
                .path()
                .strip_prefix(&root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| entry.path().to_string_lossy().into_owned());
            for heading in &["## Schema", "## State", "## History"] {
                if !text.contains(heading) {
                    return Err(RestoreError::Integrity(format!(
                        "Markdown domains file {relative} is missing required section heading: {heading}"
                    )));
                }
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

fn markdown_prose_lines(text: &str) -> Vec<ProseLine> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| markdown_line_is_prose(line))
        .map(|(index, line)| ProseLine {
            number: index.saturating_add(1),
            citations: extract_evidence_aliases(line),
        })
        .collect()
}

fn uncited_prose_lines(prose_lines: &[ProseLine]) -> Vec<usize> {
    prose_lines
        .iter()
        .filter(|prose| prose.citations.is_empty())
        .map(|prose| prose.number)
        .collect()
}

fn excessive_citation_prose_lines(prose_lines: &[ProseLine]) -> Vec<usize> {
    prose_lines
        .iter()
        .filter(|prose| prose.citations.len() > MAXIMUM_WIKI_CITATIONS_PER_PROSE_LINE)
        .map(|prose| prose.number)
        .collect()
}

/// A rejected commit costs a full agent turn, so every reason the wiki is
/// invalid is reported at once instead of one per retry.
fn wiki_validation_error(problems: Vec<String>) -> RestoreError {
    let total = problems.len();
    let mut shown = problems;
    let hidden = total.saturating_sub(MAXIMUM_REPORTED_WIKI_PROBLEMS);
    shown.truncate(MAXIMUM_REPORTED_WIKI_PROBLEMS);
    let mut message = format!(
        "memory commit rejected with {total} problem(s); fix every one before committing again: {}",
        shown.join("; ")
    );
    if hidden > 0 {
        message.push_str(&format!("; and {hidden} more"));
    }
    RestoreError::Integrity(message)
}

fn format_line_numbers(lines: &[usize]) -> String {
    format_bounded_list(lines.iter().map(|line| line.to_string()))
}

fn format_bounded_list(values: impl IntoIterator<Item = String>) -> String {
    let values = values.into_iter().collect::<Vec<_>>();
    let hidden = values.len().saturating_sub(MAXIMUM_REPORTED_WIKI_LOCATIONS);
    let shown = values
        .into_iter()
        .take(MAXIMUM_REPORTED_WIKI_LOCATIONS)
        .collect::<Vec<_>>()
        .join(", ");
    if hidden > 0 {
        format!("{shown}, and {hidden} more")
    } else {
        shown
    }
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

fn valid_person_alias(alias: &str) -> bool {
    alias.len() == 7
        && alias.starts_with('P')
        && alias[1..].bytes().all(|byte| byte.is_ascii_digit())
        && alias[1..].parse::<u64>().is_ok_and(|number| number > 0)
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
        delivery_message_text, evidence_line_alias, extract_evidence_aliases, markdown_prose_lines,
        order_unit_drafts, validate_me_self_attribution,
        validate_reviewed_no_durable_memory_commit, validate_wiki_commit, MemoryDeliveryOrder,
        ProseLine, UnitDraft, WikiFileSnapshot,
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
            prose_lines: Vec::new(),
        }
    }

    fn prose_line(number: usize, aliases: &[&str]) -> ProseLine {
        ProseLine {
            number,
            citations: aliases.iter().map(|alias| (*alias).to_string()).collect(),
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
                prose_lines: vec![ProseLine {
                    number: 3,
                    citations: aliases.iter().cloned().collect(),
                }],
            },
        )]);
        assert!(
            validate_wiki_commit(&current, None, &["me.md".to_string()], &aliases, 9,).is_err()
        );
    }

    #[test]
    fn account_holder_prose_requires_self_authored_support() {
        let mut me = snapshot("me-after", &["E000000001"], true);
        me.prose_lines = vec![prose_line(4, &["E000000001"])];
        assert!(validate_me_self_attribution(&me, &BTreeSet::new()).is_err());
        assert!(
            validate_me_self_attribution(&me, &BTreeSet::from(["E000000001".to_string()]),).is_ok()
        );

        me.prose_lines = vec![prose_line(4, &["E000000001", "E000000002"])];
        assert!(
            validate_me_self_attribution(&me, &BTreeSet::from(["E000000002".to_string()]),).is_ok()
        );
    }

    #[test]
    fn sticker_envelopes_collapse_while_location_and_system_text_survive() {
        let sticker = "<msg><emoji fromusername = \"someone\" tousername = \"wxid_example\" type=\"2\" md5=\"82a3c0358c131b7f4b8d5987a3ca4e0e\" len = \"10967\" cdnurl = \"http://vweixinf.tc.qq.com/very/long/path\" aeskey=\"0123456789abcdef\" /></msg>";
        assert_eq!(delivery_message_text("Emoji", sticker), "[Emoji]");

        let location = "<?xml version=\"1.0\"?>\n<msg>\n\t<location x=\"31.19\" y=\"121.31\" scale=\"16\" label=\"Hongqiao Tiandi F6\" maptype=\"roadmap\" poiname=\"New Discovery\" poiid=\"qqmap_1055\" />\n</msg>";
        assert_eq!(
            delivery_message_text("Location", location),
            "Hongqiao Tiandi F6 New Discovery"
        );

        let system = "<?xml version=\"1.0\"?>\n<sysmsg type=\"sysmsgtemplate\">\n<template><![CDATA[\"$username$\" invited you to this group chat]]></template>\n<nickname><![CDATA[Zheng]]></nickname>\n</sysmsg>";
        assert_eq!(
            delivery_message_text("System", system),
            "\"$username$\" invited you to this group chat Zheng"
        );

        assert_eq!(
            delivery_message_text("Text", "<3 not markup"),
            "<3 not markup"
        );
        let long = "\u{4e2d}".repeat(4000);
        let bounded = delivery_message_text("Text", &long);
        assert!(bounded.len() <= 4096 + '\u{2026}'.len_utf8());
        assert!(bounded.ends_with('\u{2026}'));
    }

    #[test]
    fn prose_lines_carry_one_based_source_line_numbers() {
        let lines = markdown_prose_lines("# Title\n\nfact one [E000000001]\nuncited fact\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].number, 3);
        assert_eq!(lines[1].number, 4);
        assert!(lines[1].citations.is_empty());
    }

    #[test]
    fn only_cited_evidence_lines_are_worth_decoding() {
        assert_eq!(
            evidence_line_alias(br#"{"alias":"E000236493","canonicalId":"eyJ2Ijox"}"#),
            Some("E000236493")
        );
        assert_eq!(
            evidence_line_alias(b"{\"alias\":\"E000000001\",\"actor\":\"self\"}\n"),
            Some("E000000001")
        );
        // Anything the writer would not have produced falls through to a full
        // parse rather than being silently skipped.
        assert_eq!(evidence_line_alias(br#"{"alias":"E00023649"#), None);
        assert_eq!(evidence_line_alias(br#"{"alias":"","actor":"self"}"#), None);
        assert_eq!(
            evidence_line_alias(br#"{"alias":"X000236493","a":1}"#),
            None
        );
        assert_eq!(
            evidence_line_alias(br#"{"alias":"E00023649x","a":1}"#),
            None
        );
        assert_eq!(evidence_line_alias(br#"{"alias":"E000236493"}"#), None);
        assert_eq!(
            evidence_line_alias(br#"{"actor":"self","alias":"E000236493"}"#),
            None
        );
    }

    #[test]
    fn a_rejected_commit_reports_every_problem_at_once() {
        let mut page = snapshot("after", &["E000000001"], true);
        page.prose_lines = vec![prose_line(3, &["E000000001"]), prose_line(7, &[])];
        let current = BTreeMap::from([("people/P000001.md".to_string(), page)]);
        let error = validate_wiki_commit(
            &current,
            None,
            &[],
            &["E000000001".to_string(), "E000000002".to_string()],
            9,
        )
        .expect_err("an untargeted page with an uncited line and a stray alias is invalid");
        let message = error.to_string();
        assert!(message.contains("3 problem(s)"), "{message}");
        assert!(
            message.contains("non-target wiki page people/P000001.md"),
            "{message}"
        );
        assert!(
            message.contains("uncited prose line(s) at line 7"),
            "{message}"
        );
        assert!(message.contains("E000000002"), "{message}");
    }

    #[test]
    fn reviewed_no_memory_still_rejects_unknown_existing_citations() {
        let wiki = BTreeMap::from([("me.md".to_string(), snapshot("same", &["E999999999"], true))]);
        assert!(validate_reviewed_no_durable_memory_commit(&wiki, Some(&wiki), 3).is_err());
    }

    // ── Task 2: OutputFormat serde round-trips ────────────────────────────────

    #[test]
    fn output_format_serializes_to_lowercase_strings() {
        use super::OutputFormat;
        assert_eq!(
            serde_json::to_string(&OutputFormat::Wiki).unwrap(),
            "\"wiki\""
        );
        assert_eq!(
            serde_json::to_string(&OutputFormat::Python).unwrap(),
            "\"python\""
        );
        assert_eq!(
            serde_json::to_string(&OutputFormat::Markdown).unwrap(),
            "\"markdown\""
        );
    }

    #[test]
    fn output_format_deserializes_from_lowercase_strings() {
        use super::OutputFormat;
        let wiki: OutputFormat = serde_json::from_str("\"wiki\"").unwrap();
        assert_eq!(wiki, OutputFormat::Wiki);
        let python: OutputFormat = serde_json::from_str("\"python\"").unwrap();
        assert_eq!(python, OutputFormat::Python);
        let markdown: OutputFormat = serde_json::from_str("\"markdown\"").unwrap();
        assert_eq!(markdown, OutputFormat::Markdown);
    }

    #[test]
    fn output_format_default_is_wiki() {
        use super::OutputFormat;
        assert_eq!(OutputFormat::default(), OutputFormat::Wiki);
    }

    // ── Task 2: Python format commit validation ───────────────────────────────

    #[test]
    fn python_commit_requires_manifest_py() {
        use super::validate_python_format_commit;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path();
        // Set dir permissions to 0700 (owner-only) so scan_wiki / ensure_private_directory passes
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        // No manifest.py → should fail
        let result = validate_python_format_commit(path);
        assert!(result.is_err(), "expected error when manifest.py is absent");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("manifest.py"), "{msg}");
    }

    #[test]
    fn python_commit_rejects_invalid_python_syntax() {
        use super::validate_python_format_commit;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path();
        std::fs::set_permissions(path, PermissionsExt::from_mode(0o700)).unwrap();
        // Write a syntactically broken manifest.py
        let manifest = path.join("manifest.py");
        std::fs::write(&manifest, b"def broken(\n").unwrap();
        std::fs::set_permissions(&manifest, PermissionsExt::from_mode(0o600)).unwrap();
        let result = validate_python_format_commit(path);
        assert!(result.is_err(), "expected syntax error to be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("syntax") || msg.contains("manifest.py"),
            "{msg}"
        );
    }

    #[test]
    fn python_commit_rejects_disallowed_extensions() {
        use super::validate_python_format_commit;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path();
        std::fs::set_permissions(path, PermissionsExt::from_mode(0o700)).unwrap();
        // Valid manifest.py
        let manifest = path.join("manifest.py");
        std::fs::write(&manifest, b"x = 1\n").unwrap();
        std::fs::set_permissions(&manifest, PermissionsExt::from_mode(0o600)).unwrap();
        // Binary file with disallowed extension
        let binary = path.join("data.bin");
        std::fs::write(&binary, b"\x00\x01\x02").unwrap();
        std::fs::set_permissions(&binary, PermissionsExt::from_mode(0o600)).unwrap();
        let result = validate_python_format_commit(path);
        assert!(
            result.is_err(),
            "expected disallowed extension to be rejected"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("disallowed") || msg.contains("extension"),
            "{msg}"
        );
    }

    #[test]
    fn python_commit_accepts_valid_directory() {
        use super::validate_python_format_commit;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path();
        std::fs::set_permissions(path, PermissionsExt::from_mode(0o700)).unwrap();
        // Valid manifest.py
        let manifest = path.join("manifest.py");
        std::fs::write(&manifest, b"schema = 'memory-v1'\n").unwrap();
        std::fs::set_permissions(&manifest, PermissionsExt::from_mode(0o600)).unwrap();
        // python3 must be available in the test environment; skip if not
        if std::process::Command::new("python3")
            .arg("--version")
            .status()
            .is_err()
        {
            return;
        }
        let result = validate_python_format_commit(path);
        assert!(
            result.is_ok(),
            "expected valid directory to pass: {result:?}"
        );
    }

    #[test]
    fn python_commit_allows_hidden_files_and_git_dir() {
        use super::validate_python_format_commit;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path();
        std::fs::set_permissions(path, PermissionsExt::from_mode(0o700)).unwrap();
        // Valid manifest.py
        let manifest = path.join("manifest.py");
        std::fs::write(&manifest, b"x = 1\n").unwrap();
        std::fs::set_permissions(&manifest, PermissionsExt::from_mode(0o600)).unwrap();
        // .gitignore — hidden file, should be skipped
        let gitignore = path.join(".gitignore");
        std::fs::write(&gitignore, b"__pycache__/\n").unwrap();
        std::fs::set_permissions(&gitignore, PermissionsExt::from_mode(0o600)).unwrap();
        // .git/config — file inside hidden directory, should be skipped
        let git_dir = path.join(".git");
        std::fs::create_dir(&git_dir).unwrap();
        std::fs::set_permissions(&git_dir, PermissionsExt::from_mode(0o700)).unwrap();
        let git_config = git_dir.join("config");
        std::fs::write(&git_config, b"[core]\n").unwrap();
        std::fs::set_permissions(&git_config, PermissionsExt::from_mode(0o600)).unwrap();
        if std::process::Command::new("python3")
            .arg("--version")
            .status()
            .is_err()
        {
            return;
        }
        // Hidden files/dirs should be skipped; the directory should pass
        let result = validate_python_format_commit(path);
        assert!(
            result.is_ok(),
            "expected .gitignore and .git/ to be allowed: {result:?}"
        );
    }

    // ── Task 2: Markdown format commit validation ─────────────────────────────

    #[test]
    fn markdown_commit_requires_manifest_md() {
        use super::validate_markdown_format_commit;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path();
        std::fs::set_permissions(path, PermissionsExt::from_mode(0o700)).unwrap();
        let result = validate_markdown_format_commit(path);
        assert!(result.is_err(), "expected error when manifest.md is absent");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("manifest.md"), "{msg}");
    }

    #[test]
    fn markdown_commit_rejects_domains_file_missing_required_headings() {
        use super::validate_markdown_format_commit;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path();
        std::fs::set_permissions(path, PermissionsExt::from_mode(0o700)).unwrap();
        let manifest = path.join("manifest.md");
        std::fs::write(&manifest, b"# Memory\n").unwrap();
        std::fs::set_permissions(&manifest, PermissionsExt::from_mode(0o600)).unwrap();
        let domains = path.join("domains");
        std::fs::create_dir(&domains).unwrap();
        std::fs::set_permissions(&domains, PermissionsExt::from_mode(0o700)).unwrap();
        let domain_file = domains.join("contacts.md");
        // Missing ## History
        std::fs::write(&domain_file, b"## Schema\n\n## State\n").unwrap();
        std::fs::set_permissions(&domain_file, PermissionsExt::from_mode(0o600)).unwrap();
        let result = validate_markdown_format_commit(path);
        assert!(result.is_err(), "expected missing heading to be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("## History") || msg.contains("missing"),
            "{msg}"
        );
    }

    #[test]
    fn markdown_commit_accepts_valid_directory() {
        use super::validate_markdown_format_commit;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path();
        std::fs::set_permissions(path, PermissionsExt::from_mode(0o700)).unwrap();
        let manifest = path.join("manifest.md");
        std::fs::write(&manifest, b"# Memory\n").unwrap();
        std::fs::set_permissions(&manifest, PermissionsExt::from_mode(0o600)).unwrap();
        let domains = path.join("domains");
        std::fs::create_dir(&domains).unwrap();
        std::fs::set_permissions(&domains, PermissionsExt::from_mode(0o700)).unwrap();
        let domain_file = domains.join("contacts.md");
        std::fs::write(&domain_file, b"## Schema\n\n## State\n\n## History\n").unwrap();
        std::fs::set_permissions(&domain_file, PermissionsExt::from_mode(0o600)).unwrap();
        let result = validate_markdown_format_commit(path);
        assert!(
            result.is_ok(),
            "expected valid directory to pass: {result:?}"
        );
    }

    // ── Task 1: CorpusGenerationLink manifest field serde ────────────────────

    #[test]
    fn corpus_generation_link_serializes_with_camel_case() {
        use super::CorpusGenerationLink;
        let link = CorpusGenerationLink {
            base_manifest_sha256: "abc123".to_string(),
            generation: 1,
            first_new_unit_index: 5,
            carried_unit_count: 5,
            carried_message_count: 100,
        };
        let json = serde_json::to_value(&link).unwrap();
        assert!(json.get("baseManifestSHA256").is_some());
        assert!(json.get("generation").is_some());
        assert!(json.get("firstNewUnitIndex").is_some());
        assert!(json.get("carriedUnitCount").is_some());
        assert!(json.get("carriedMessageCount").is_some());
    }

    #[test]
    fn corpus_generation_link_roundtrips() {
        use super::CorpusGenerationLink;
        let link = CorpusGenerationLink {
            base_manifest_sha256: "deadbeef".to_string(),
            generation: 2,
            first_new_unit_index: 10,
            carried_unit_count: 10,
            carried_message_count: 42,
        };
        let json = serde_json::to_vec(&link).unwrap();
        let decoded: CorpusGenerationLink = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded.base_manifest_sha256, "deadbeef");
        assert_eq!(decoded.generation, 2);
        assert_eq!(decoded.first_new_unit_index, 10);
        assert_eq!(decoded.carried_unit_count, 10);
        assert_eq!(decoded.carried_message_count, 42);
    }
}
