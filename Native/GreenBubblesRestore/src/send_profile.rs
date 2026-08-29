//! Signed calibration profiles and the (macOS build x WeChat build)
//! compatibility matrix for the deterministic UI-automation send adapter.
//!
//! Both artifacts are *data, not code*: a WeChat layout change is fixed by
//! shipping a new signed profile rather than rebuilding the application. They
//! are therefore treated as untrusted input until an Ed25519 signature made by
//! a pinned release key verifies over a canonical, language-independent byte
//! encoding. Anything unknown, unsigned, expired, or bound to a different
//! client build fails closed: the send path stays disabled.
//!
//! Window-relative geometry is expressed in integer parts-per-million rather
//! than floating point so the signed bytes are exactly reproducible in every
//! language that verifies them (the Swift helper mirrors
//! `canonical_signing_bytes` byte for byte; `tests/send_profile_vectors.rs`
//! and the Swift test suite both pin the same fixture digest).

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::archive::ensure_private_regular_file;
use crate::send_contract::CanonicalWriter;
use crate::RestoreError;

/// Schema version of the signed calibration profile document.
pub const CALIBRATION_PROFILE_SCHEMA_VERSION: u32 = 1;
/// Schema version of the signed compatibility-matrix document.
pub const COMPATIBILITY_MATRIX_SCHEMA_VERSION: u32 = 1;
/// Upper bound on any signed send artifact read from disk.
pub const MAXIMUM_SIGNED_SEND_ARTIFACT_BYTES: u64 = 256 * 1024;
/// Upper bound on the number of entries in one compatibility matrix.
pub const MAXIMUM_COMPATIBILITY_ENTRY_COUNT: usize = 4_096;
/// One million; window-relative fractions are integers in `0..=PARTS_PER_MILLION`.
pub const PARTS_PER_MILLION: u32 = 1_000_000;

const CALIBRATION_PROFILE_DOMAIN: &str = "greenbubbles.send.calibration-profile.v1";
const COMPATIBILITY_MATRIX_DOMAIN: &str = "greenbubbles.send.compatibility-matrix.v1";

/// A point inside a window, as integer parts-per-million of the window's
/// width and height. Integers keep the signed bytes exactly reproducible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WindowRelativePoint {
    pub x_parts_per_million: u32,
    pub y_parts_per_million: u32,
}

impl WindowRelativePoint {
    pub(crate) fn valid(&self) -> bool {
        self.x_parts_per_million <= PARTS_PER_MILLION
            && self.y_parts_per_million <= PARTS_PER_MILLION
    }
}

/// A rectangle inside a window, in integer parts-per-million.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WindowRelativeRect {
    pub x_parts_per_million: u32,
    pub y_parts_per_million: u32,
    pub width_parts_per_million: u32,
    pub height_parts_per_million: u32,
}

impl WindowRelativeRect {
    pub(crate) fn valid(&self) -> bool {
        self.width_parts_per_million > 0
            && self.height_parts_per_million > 0
            && u64::from(self.x_parts_per_million) + u64::from(self.width_parts_per_million)
                <= u64::from(PARTS_PER_MILLION)
            && u64::from(self.y_parts_per_million) + u64::from(self.height_parts_per_million)
                <= u64::from(PARTS_PER_MILLION)
    }
}

/// The three click targets the mechanical send skill needs. Mouse clicks only
/// ever *focus* one of these; every mutation is performed with the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CalibrationAnchors {
    pub search_box: WindowRelativePoint,
    pub first_result_row: WindowRelativePoint,
    pub compose_box: WindowRelativePoint,
}

/// The three capture regions the on-screen gates read with Apple Vision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CalibrationOcrRegions {
    /// GATE 0: the search field, read back to prove the click actually took
    /// focus before anything destructive is typed anywhere.
    pub search: WindowRelativeRect,
    /// GATE 1: the opened conversation's title.
    pub title: WindowRelativeRect,
    /// GATE 2: the compose box, read back after pasting the body.
    pub compose: WindowRelativeRect,
    /// GATE 3: the newest outgoing bubble, read back after Return.
    pub newest_outgoing: WindowRelativeRect,
}

/// What the no-send calibration self-test must observe to pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CalibrationSelfTest {
    pub focus_indicator: String,
    pub minimum_title_confidence_parts_per_million: u32,
}

/// The extra anchors and regions an attachment send needs. A profile without
/// this section simply cannot stage an attachment on that build, which is how
/// "attachments are unavailable until someone measures and signs them" is
/// expressed as data rather than as code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CalibrationAttachments {
    /// The compose-toolbar control that opens the file panel. Only used by the
    /// panel fallback; the pasteboard path needs no anchor at all.
    pub attach_control: WindowRelativePoint,
    /// The confirm control on the send-confirmation sheet, when the build
    /// raises one.
    pub confirm_send_button: WindowRelativePoint,
    /// Where a staged attachment's name appears in the compose area.
    pub compose_attachment: WindowRelativeRect,
    /// Where the confirmation sheet shows the file it is about to send.
    pub confirm_sheet: WindowRelativeRect,
    /// Whether this build raises a confirmation sheet at all.
    pub presents_confirmation_sheet: bool,
    /// Whether the compose box accepts a pasted file reference on this build.
    /// Answering this is the whole point of the A0 spike; a profile that says
    /// false forces the panel fallback.
    pub compose_accepts_pasted_file: bool,
}

/// Everything the release key signs. The signature is deliberately outside
/// this structure so the canonical bytes can never include it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CalibrationProfileBody {
    pub schema: u32,
    pub profile_id: String,
    pub wechat_bundle_identifier: String,
    pub wechat_marketing_version: String,
    pub wechat_build: String,
    pub client_build_profile_id: String,
    pub macos_major: u32,
    pub anchors: CalibrationAnchors,
    pub ocr_regions: CalibrationOcrRegions,
    pub selftest: CalibrationSelfTest,
    /// Absent until someone has measured this build's attachment surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<CalibrationAttachments>,
    pub issued_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
}

/// A calibration profile plus its detached hexadecimal Ed25519 signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SignedCalibrationProfile {
    #[serde(flatten)]
    pub body: CalibrationProfileBody,
    pub signature: String,
}

/// State of one (macOS build x WeChat build) combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompatibilityState {
    /// Validated end to end; the send path may open if every other gate passes.
    Supported,
    /// Never validated. Fails closed exactly like `Blocked`.
    Unverified,
    /// Known broken or deliberately disabled in the field.
    Blocked,
}

impl CompatibilityState {
    /// Only `Supported` may open the send path. Everything else fails closed.
    pub fn permits_send(self) -> bool {
        matches!(self, CompatibilityState::Supported)
    }

    fn canonical_name(self) -> &'static str {
        match self {
            CompatibilityState::Supported => "supported",
            CompatibilityState::Unverified => "unverified",
            CompatibilityState::Blocked => "blocked",
        }
    }
}

/// One row of the compatibility matrix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CompatibilityEntry {
    pub macos_build: String,
    pub macos_major: u32,
    pub wechat_build: String,
    pub client_build_profile_id: String,
    pub state: CompatibilityState,
    pub calibration_profile_id: String,
    pub note: String,
}

/// Everything the release key signs for a compatibility matrix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CompatibilityMatrixBody {
    pub schema: u32,
    pub matrix_id: String,
    pub issued_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    /// The field kill switch. Because the matrix is signed and updatable out
    /// of band, publishing one with this set disables the send path everywhere
    /// without shipping an application update; letting a matrix expire does
    /// the same thing passively.
    pub global_kill_switch_engaged: bool,
    pub entries: Vec<CompatibilityEntry>,
}

/// A compatibility matrix plus its detached hexadecimal Ed25519 signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SignedCompatibilityMatrix {
    #[serde(flatten)]
    pub body: CompatibilityMatrixBody,
    pub signature: String,
}

/// Which key verified a signed artifact. Development keys never unlock a
/// rollout stage that can press Return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SendTrustTier {
    Release,
    Development,
}

/// Where a verifying key came from. Release keys are pinned at build time;
/// development keys must be named explicitly by an owner-only file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SendTrustRoot {
    pub release_public_keys: Vec<String>,
    #[serde(default)]
    pub development_public_keys: Vec<String>,
}

/// Machine-readable reasons a signed send artifact was refused. Every variant
/// keeps the send path closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SignedArtifactDenial {
    TrustRootEmpty,
    TrustRootMalformed,
    SchemaUnsupported,
    StructurallyInvalid,
    SignatureMalformed,
    SignatureNotVerified,
    NotYetValid,
    Expired,
    ClientBuildMismatch,
    HostBuildMismatch,
    ProfileNotInMatrix,
    CombinationNotSupported,
}

/// The result of verifying a signed calibration profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct VerifiedCalibrationProfile {
    pub profile: SignedCalibrationProfile,
    pub trust_tier: SendTrustTier,
    pub canonical_sha256: String,
}

/// The result of verifying a signed compatibility matrix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct VerifiedCompatibilityMatrix {
    pub matrix: SignedCompatibilityMatrix,
    pub trust_tier: SendTrustTier,
    pub canonical_sha256: String,
}

impl SendTrustRoot {
    /// The release keys pinned into this binary at build time. An empty set is
    /// the safe default: without a provisioned release key no release-signed
    /// profile verifies and the send path cannot open.
    pub fn pinned() -> Result<Self, SignedArtifactDenial> {
        let release_public_keys = match option_env!("GREENBUBBLES_SEND_RELEASE_PUBLIC_KEYS") {
            None => Vec::new(),
            Some(value) => value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect(),
        };
        let root = Self {
            release_public_keys,
            development_public_keys: Vec::new(),
        };
        root.verifying_keys(SendTrustTier::Release)?;
        Ok(root)
    }

    /// Loads a trust root from an owner-only JSON file. Only the development
    /// keys of such a file are honored; a file can never introduce a release
    /// key, because release trust is pinned at build time.
    pub fn load_development(path: &Path) -> Result<Self, RestoreError> {
        ensure_private_regular_file(path)?;
        let bytes = bounded_private_bytes(path)?;
        let loaded: SendTrustRoot = serde_json::from_slice(&bytes)?;
        let mut root = Self::pinned().map_err(denial_error)?;
        root.development_public_keys = loaded.development_public_keys;
        root.verifying_keys(SendTrustTier::Development)
            .map_err(denial_error)?;
        Ok(root)
    }

    fn verifying_keys(
        &self,
        tier: SendTrustTier,
    ) -> Result<Vec<VerifyingKey>, SignedArtifactDenial> {
        let encoded = match tier {
            SendTrustTier::Release => &self.release_public_keys,
            SendTrustTier::Development => &self.development_public_keys,
        };
        encoded
            .iter()
            .map(|value| {
                let bytes: [u8; 32] = hex::decode(value)
                    .ok()
                    .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
                    .ok_or(SignedArtifactDenial::TrustRootMalformed)?;
                VerifyingKey::from_bytes(&bytes)
                    .map_err(|_| SignedArtifactDenial::TrustRootMalformed)
            })
            .collect()
    }

    fn verify(
        &self,
        message: &[u8],
        signature: &str,
    ) -> Result<SendTrustTier, SignedArtifactDenial> {
        let signature: [u8; 64] = hex::decode(signature)
            .ok()
            .and_then(|bytes| <[u8; 64]>::try_from(bytes.as_slice()).ok())
            .ok_or(SignedArtifactDenial::SignatureMalformed)?;
        let signature = Signature::from_bytes(&signature);
        if self.release_public_keys.is_empty() && self.development_public_keys.is_empty() {
            return Err(SignedArtifactDenial::TrustRootEmpty);
        }
        for tier in [SendTrustTier::Release, SendTrustTier::Development] {
            for key in self.verifying_keys(tier)? {
                if key.verify_strict(message, &signature).is_ok() {
                    return Ok(tier);
                }
            }
        }
        Err(SignedArtifactDenial::SignatureNotVerified)
    }
}

/// Appends one named window-relative point to a canonical encoding.
fn push_point(writer: &mut CanonicalWriter, name: &str, point: WindowRelativePoint) {
    writer
        .text(name)
        .number(u128::from(point.x_parts_per_million))
        .number(u128::from(point.y_parts_per_million));
}

/// Appends one named window-relative rectangle to a canonical encoding.
fn push_rect(writer: &mut CanonicalWriter, name: &str, rect: WindowRelativeRect) {
    writer
        .text(name)
        .number(u128::from(rect.x_parts_per_million))
        .number(u128::from(rect.y_parts_per_million))
        .number(u128::from(rect.width_parts_per_million))
        .number(u128::from(rect.height_parts_per_million));
}

/// The exact bytes a release key signs for a calibration profile.
pub fn calibration_profile_signing_bytes(body: &CalibrationProfileBody) -> Option<Vec<u8>> {
    let mut writer = CanonicalWriter::new(CALIBRATION_PROFILE_DOMAIN);
    writer
        .number(u128::from(body.schema))
        .text(&body.profile_id)
        .text(&body.wechat_bundle_identifier)
        .text(&body.wechat_marketing_version)
        .text(&body.wechat_build)
        .text(&body.client_build_profile_id)
        .number(u128::from(body.macos_major));
    push_point(&mut writer, "anchor.searchBox", body.anchors.search_box);
    push_point(
        &mut writer,
        "anchor.firstResultRow",
        body.anchors.first_result_row,
    );
    push_point(&mut writer, "anchor.composeBox", body.anchors.compose_box);
    push_rect(&mut writer, "region.search", body.ocr_regions.search);
    push_rect(&mut writer, "region.title", body.ocr_regions.title);
    push_rect(&mut writer, "region.compose", body.ocr_regions.compose);
    push_rect(
        &mut writer,
        "region.newestOutgoing",
        body.ocr_regions.newest_outgoing,
    );
    writer
        .text("selftest.focusIndicator")
        .text(&body.selftest.focus_indicator)
        .number(u128::from(
            body.selftest.minimum_title_confidence_parts_per_million,
        ))
        .flag(body.attachments.is_some());
    if let Some(attachments) = &body.attachments {
        push_point(
            &mut writer,
            "anchor.attachControl",
            attachments.attach_control,
        );
        push_point(
            &mut writer,
            "anchor.confirmSendButton",
            attachments.confirm_send_button,
        );
        push_rect(
            &mut writer,
            "region.composeAttachment",
            attachments.compose_attachment,
        );
        push_rect(
            &mut writer,
            "region.confirmSheet",
            attachments.confirm_sheet,
        );
        writer
            .flag(attachments.presents_confirmation_sheet)
            .flag(attachments.compose_accepts_pasted_file);
    }
    writer
        .number(u128::from(body.issued_at_unix_seconds))
        .number(u128::from(body.expires_at_unix_seconds));
    writer.finish()
}

/// The exact bytes a release key signs for a compatibility matrix.
pub fn compatibility_matrix_signing_bytes(body: &CompatibilityMatrixBody) -> Option<Vec<u8>> {
    let mut writer = CanonicalWriter::new(COMPATIBILITY_MATRIX_DOMAIN);
    writer
        .number(u128::from(body.schema))
        .text(&body.matrix_id)
        .number(u128::from(body.issued_at_unix_seconds))
        .number(u128::from(body.expires_at_unix_seconds))
        .flag(body.global_kill_switch_engaged)
        .number(body.entries.len() as u128);
    for entry in &body.entries {
        writer
            .text(&entry.macos_build)
            .number(u128::from(entry.macos_major))
            .text(&entry.wechat_build)
            .text(&entry.client_build_profile_id)
            .text(entry.state.canonical_name())
            .text(&entry.calibration_profile_id)
            .text(&entry.note);
    }
    writer.finish()
}

fn structurally_valid_profile(body: &CalibrationProfileBody) -> bool {
    body.schema == CALIBRATION_PROFILE_SCHEMA_VERSION
        && !body.profile_id.is_empty()
        && body.profile_id.len() <= 128
        && !body.wechat_bundle_identifier.is_empty()
        && !body.wechat_marketing_version.is_empty()
        && !body.wechat_build.is_empty()
        && !body.client_build_profile_id.is_empty()
        && body.macos_major >= 10
        && body.anchors.search_box.valid()
        && body.anchors.first_result_row.valid()
        && body.anchors.compose_box.valid()
        && body.ocr_regions.search.valid()
        && body.ocr_regions.title.valid()
        && body.ocr_regions.compose.valid()
        && body.ocr_regions.newest_outgoing.valid()
        && !body.selftest.focus_indicator.is_empty()
        && body.selftest.minimum_title_confidence_parts_per_million <= PARTS_PER_MILLION
        && body.attachments.as_ref().is_none_or(|attachments| {
            attachments.attach_control.valid()
                && attachments.confirm_send_button.valid()
                && attachments.compose_attachment.valid()
                && attachments.confirm_sheet.valid()
        })
        && body.issued_at_unix_seconds < body.expires_at_unix_seconds
}

fn structurally_valid_matrix(body: &CompatibilityMatrixBody) -> bool {
    if body.schema != COMPATIBILITY_MATRIX_SCHEMA_VERSION
        || body.matrix_id.is_empty()
        || body.issued_at_unix_seconds >= body.expires_at_unix_seconds
        || body.entries.is_empty()
        || body.entries.len() > MAXIMUM_COMPATIBILITY_ENTRY_COUNT
    {
        return false;
    }
    let mut seen = BTreeSet::new();
    let mut previous: Option<(&str, &str)> = None;
    for entry in &body.entries {
        if entry.macos_build.is_empty()
            || entry.wechat_build.is_empty()
            || entry.client_build_profile_id.is_empty()
            || entry.macos_major < 10
            || (entry.state == CompatibilityState::Supported
                && entry.calibration_profile_id.is_empty())
        {
            return false;
        }
        let key = (entry.macos_build.as_str(), entry.wechat_build.as_str());
        if !seen.insert(key) || previous.is_some_and(|previous| previous >= key) {
            return false;
        }
        previous = Some(key);
    }
    true
}

/// Verifies a signed calibration profile against a trust root and the current
/// wall clock. Every failure mode is an explicit denial; there is no partial
/// acceptance and no "warn but continue" path.
pub fn verify_calibration_profile(
    profile: &SignedCalibrationProfile,
    trust_root: &SendTrustRoot,
    now_unix_seconds: u64,
) -> Result<VerifiedCalibrationProfile, SignedArtifactDenial> {
    if profile.body.schema != CALIBRATION_PROFILE_SCHEMA_VERSION {
        return Err(SignedArtifactDenial::SchemaUnsupported);
    }
    if !structurally_valid_profile(&profile.body) {
        return Err(SignedArtifactDenial::StructurallyInvalid);
    }
    let message = calibration_profile_signing_bytes(&profile.body)
        .ok_or(SignedArtifactDenial::StructurallyInvalid)?;
    let trust_tier = trust_root.verify(&message, &profile.signature)?;
    if now_unix_seconds < profile.body.issued_at_unix_seconds {
        return Err(SignedArtifactDenial::NotYetValid);
    }
    if now_unix_seconds >= profile.body.expires_at_unix_seconds {
        return Err(SignedArtifactDenial::Expired);
    }
    Ok(VerifiedCalibrationProfile {
        profile: profile.clone(),
        trust_tier,
        canonical_sha256: hex::encode(Sha256::digest(&message)),
    })
}

/// Verifies a signed compatibility matrix against a trust root and the clock.
pub fn verify_compatibility_matrix(
    matrix: &SignedCompatibilityMatrix,
    trust_root: &SendTrustRoot,
    now_unix_seconds: u64,
) -> Result<VerifiedCompatibilityMatrix, SignedArtifactDenial> {
    if matrix.body.schema != COMPATIBILITY_MATRIX_SCHEMA_VERSION {
        return Err(SignedArtifactDenial::SchemaUnsupported);
    }
    if !structurally_valid_matrix(&matrix.body) {
        return Err(SignedArtifactDenial::StructurallyInvalid);
    }
    let message = compatibility_matrix_signing_bytes(&matrix.body)
        .ok_or(SignedArtifactDenial::StructurallyInvalid)?;
    let trust_tier = trust_root.verify(&message, &matrix.signature)?;
    if now_unix_seconds < matrix.body.issued_at_unix_seconds {
        return Err(SignedArtifactDenial::NotYetValid);
    }
    if now_unix_seconds >= matrix.body.expires_at_unix_seconds {
        return Err(SignedArtifactDenial::Expired);
    }
    Ok(VerifiedCompatibilityMatrix {
        matrix: matrix.clone(),
        trust_tier,
        canonical_sha256: hex::encode(Sha256::digest(&message)),
    })
}

/// The compatibility decision for one host and client build. An unknown
/// combination is reported as `Unverified`, which never permits a send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CompatibilityDecision {
    pub macos_build: String,
    pub wechat_build: String,
    pub state: CompatibilityState,
    pub known_combination: bool,
    /// Copied from the signed matrix so callers cannot look at a combination
    /// without also seeing the field kill switch.
    pub field_kill_switch_engaged: bool,
    pub expected_calibration_profile_id: String,
    pub client_build_profile_id: String,
    pub note: String,
}

/// Looks one (macOS build x WeChat build) combination up in a verified matrix.
pub fn compatibility_decision(
    matrix: &VerifiedCompatibilityMatrix,
    macos_build: &str,
    wechat_build: &str,
) -> CompatibilityDecision {
    match matrix
        .matrix
        .body
        .entries
        .iter()
        .find(|entry| entry.macos_build == macos_build && entry.wechat_build == wechat_build)
    {
        Some(entry) => CompatibilityDecision {
            macos_build: macos_build.to_string(),
            wechat_build: wechat_build.to_string(),
            state: entry.state,
            known_combination: true,
            field_kill_switch_engaged: matrix.matrix.body.global_kill_switch_engaged,
            expected_calibration_profile_id: entry.calibration_profile_id.clone(),
            client_build_profile_id: entry.client_build_profile_id.clone(),
            note: entry.note.clone(),
        },
        None => CompatibilityDecision {
            macos_build: macos_build.to_string(),
            wechat_build: wechat_build.to_string(),
            state: CompatibilityState::Unverified,
            known_combination: false,
            field_kill_switch_engaged: matrix.matrix.body.global_kill_switch_engaged,
            expected_calibration_profile_id: String::new(),
            client_build_profile_id: String::new(),
            note: "combination is absent from the signed compatibility matrix".to_string(),
        },
    }
}

/// Binds a verified profile to a verified compatibility decision. The pair is
/// only usable when the matrix says `supported` *and* names exactly this
/// profile for exactly this client build.
pub fn bind_profile_to_compatibility(
    profile: &VerifiedCalibrationProfile,
    decision: &CompatibilityDecision,
    expected_macos_major: u32,
) -> Result<(), SignedArtifactDenial> {
    if decision.field_kill_switch_engaged
        || !decision.known_combination
        || !decision.state.permits_send()
    {
        return Err(SignedArtifactDenial::CombinationNotSupported);
    }
    if decision.expected_calibration_profile_id != profile.profile.body.profile_id {
        return Err(SignedArtifactDenial::ProfileNotInMatrix);
    }
    if decision.client_build_profile_id != profile.profile.body.client_build_profile_id
        || decision.wechat_build != profile.profile.body.wechat_build
    {
        return Err(SignedArtifactDenial::ClientBuildMismatch);
    }
    if profile.profile.body.macos_major != expected_macos_major {
        return Err(SignedArtifactDenial::HostBuildMismatch);
    }
    Ok(())
}

/// Reads and verifies a signed calibration profile from an owner-only file.
pub fn load_calibration_profile(
    path: &Path,
    trust_root: &SendTrustRoot,
    now_unix_seconds: u64,
) -> Result<VerifiedCalibrationProfile, RestoreError> {
    ensure_private_regular_file(path)?;
    let bytes = bounded_private_bytes(path)?;
    let profile: SignedCalibrationProfile = serde_json::from_slice(&bytes)?;
    verify_calibration_profile(&profile, trust_root, now_unix_seconds).map_err(denial_error)
}

/// Reads and verifies a signed compatibility matrix from an owner-only file.
pub fn load_compatibility_matrix(
    path: &Path,
    trust_root: &SendTrustRoot,
    now_unix_seconds: u64,
) -> Result<VerifiedCompatibilityMatrix, RestoreError> {
    ensure_private_regular_file(path)?;
    let bytes = bounded_private_bytes(path)?;
    let matrix: SignedCompatibilityMatrix = serde_json::from_slice(&bytes)?;
    verify_compatibility_matrix(&matrix, trust_root, now_unix_seconds).map_err(denial_error)
}

/// Signs a calibration profile body with a 32-byte Ed25519 seed. Used only by
/// the release tooling; the serving path never holds a signing key.
pub fn sign_calibration_profile(
    body: &CalibrationProfileBody,
    signing_key_seed: &[u8; 32],
) -> Result<SignedCalibrationProfile, RestoreError> {
    if !structurally_valid_profile(body) {
        return Err(denial_error(SignedArtifactDenial::StructurallyInvalid));
    }
    let message = calibration_profile_signing_bytes(body)
        .ok_or_else(|| denial_error(SignedArtifactDenial::StructurallyInvalid))?;
    let key = SigningKey::from_bytes(signing_key_seed);
    Ok(SignedCalibrationProfile {
        body: body.clone(),
        signature: hex::encode(key.sign(&message).to_bytes()),
    })
}

/// Signs a compatibility matrix body with a 32-byte Ed25519 seed.
pub fn sign_compatibility_matrix(
    body: &CompatibilityMatrixBody,
    signing_key_seed: &[u8; 32],
) -> Result<SignedCompatibilityMatrix, RestoreError> {
    if !structurally_valid_matrix(body) {
        return Err(denial_error(SignedArtifactDenial::StructurallyInvalid));
    }
    let message = compatibility_matrix_signing_bytes(body)
        .ok_or_else(|| denial_error(SignedArtifactDenial::StructurallyInvalid))?;
    let key = SigningKey::from_bytes(signing_key_seed);
    Ok(SignedCompatibilityMatrix {
        body: body.clone(),
        signature: hex::encode(key.sign(&message).to_bytes()),
    })
}

/// Derives the hexadecimal public key for a signing seed, so release tooling
/// can print the value that must be pinned into the verifying binaries.
pub fn signing_key_public_hex(signing_key_seed: &[u8; 32]) -> String {
    hex::encode(
        SigningKey::from_bytes(signing_key_seed)
            .verifying_key()
            .to_bytes(),
    )
}

fn bounded_private_bytes(path: &Path) -> Result<Vec<u8>, RestoreError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAXIMUM_SIGNED_SEND_ARTIFACT_BYTES {
        return Err(RestoreError::Integrity(
            "signed send artifact exceeds the verification limit".to_string(),
        ));
    }
    Ok(fs::read(path)?)
}

fn denial_error(denial: SignedArtifactDenial) -> RestoreError {
    RestoreError::Integrity(format!(
        "signed send artifact was refused: {}",
        serde_json::to_string(&denial).unwrap_or_else(|_| "\"unknown\"".to_string())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn seed(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    pub(crate) fn profile_body() -> CalibrationProfileBody {
        CalibrationProfileBody {
            schema: CALIBRATION_PROFILE_SCHEMA_VERSION,
            profile_id: "wechat-4.1.13.269579-macos-26".to_string(),
            wechat_bundle_identifier: "com.tencent.xinWeChat".to_string(),
            wechat_marketing_version: "4.1.13".to_string(),
            wechat_build: "4.1.13.269579".to_string(),
            client_build_profile_id: "wechat-macos-4.1.13-269579".to_string(),
            macos_major: 26,
            anchors: CalibrationAnchors {
                search_box: WindowRelativePoint {
                    x_parts_per_million: 235_000,
                    y_parts_per_million: 36_000,
                },
                first_result_row: WindowRelativePoint {
                    x_parts_per_million: 235_000,
                    y_parts_per_million: 115_000,
                },
                compose_box: WindowRelativePoint {
                    x_parts_per_million: 715_000,
                    y_parts_per_million: 870_000,
                },
            },
            ocr_regions: CalibrationOcrRegions {
                search: WindowRelativeRect {
                    x_parts_per_million: 40_000,
                    y_parts_per_million: 15_000,
                    width_parts_per_million: 200_000,
                    height_parts_per_million: 35_000,
                },
                title: WindowRelativeRect {
                    x_parts_per_million: 440_000,
                    y_parts_per_million: 20_000,
                    width_parts_per_million: 300_000,
                    height_parts_per_million: 50_000,
                },
                compose: WindowRelativeRect {
                    x_parts_per_million: 400_000,
                    y_parts_per_million: 830_000,
                    width_parts_per_million: 560_000,
                    height_parts_per_million: 110_000,
                },
                newest_outgoing: WindowRelativeRect {
                    x_parts_per_million: 620_000,
                    y_parts_per_million: 700_000,
                    width_parts_per_million: 280_000,
                    height_parts_per_million: 200_000,
                },
            },
            selftest: CalibrationSelfTest {
                focus_indicator: "search_caret".to_string(),
                minimum_title_confidence_parts_per_million: 900_000,
            },
            attachments: None,
            issued_at_unix_seconds: 1_000,
            expires_at_unix_seconds: 100_000,
        }
    }

    pub(crate) fn matrix_body() -> CompatibilityMatrixBody {
        CompatibilityMatrixBody {
            schema: COMPATIBILITY_MATRIX_SCHEMA_VERSION,
            matrix_id: "send-compat-2026-08-29".to_string(),
            issued_at_unix_seconds: 1_000,
            expires_at_unix_seconds: 100_000,
            global_kill_switch_engaged: false,
            entries: vec![
                CompatibilityEntry {
                    macos_build: "25G83".to_string(),
                    macos_major: 26,
                    wechat_build: "4.1.13.269579".to_string(),
                    client_build_profile_id: "wechat-macos-4.1.13-269579".to_string(),
                    state: CompatibilityState::Supported,
                    calibration_profile_id: "wechat-4.1.13.269579-macos-26".to_string(),
                    note: "validated end to end".to_string(),
                },
                CompatibilityEntry {
                    macos_build: "25G83".to_string(),
                    macos_major: 26,
                    wechat_build: "4.1.14.900000".to_string(),
                    client_build_profile_id: "wechat-macos-4.1.14-900000".to_string(),
                    state: CompatibilityState::Blocked,
                    calibration_profile_id: String::new(),
                    note: "layout drift observed".to_string(),
                },
            ],
        }
    }

    pub(crate) fn development_root(seed_byte: u8) -> SendTrustRoot {
        SendTrustRoot {
            release_public_keys: Vec::new(),
            development_public_keys: vec![signing_key_public_hex(&seed(seed_byte))],
        }
    }

    #[test]
    fn a_development_signed_profile_verifies_and_reports_its_tier() {
        let signed = sign_calibration_profile(&profile_body(), &seed(7)).unwrap();
        let verified = verify_calibration_profile(&signed, &development_root(7), 2_000).unwrap();
        assert_eq!(verified.trust_tier, SendTrustTier::Development);
        assert_eq!(verified.canonical_sha256.len(), 64);
    }

    #[test]
    fn an_empty_trust_root_refuses_every_signature() {
        let signed = sign_calibration_profile(&profile_body(), &seed(7)).unwrap();
        let empty = SendTrustRoot::default();
        assert_eq!(
            verify_calibration_profile(&signed, &empty, 2_000).unwrap_err(),
            SignedArtifactDenial::TrustRootEmpty
        );
    }

    #[test]
    fn every_signed_profile_field_is_covered_by_the_signature() {
        let signed = sign_calibration_profile(&profile_body(), &seed(7)).unwrap();
        let root = development_root(7);
        type Mutation = Box<dyn Fn(&mut CalibrationProfileBody)>;
        let mutate: Vec<Mutation> = vec![
            Box::new(|body| body.profile_id.push('x')),
            Box::new(|body| body.wechat_bundle_identifier.push('x')),
            Box::new(|body| body.wechat_marketing_version.push('x')),
            Box::new(|body| body.wechat_build.push('x')),
            Box::new(|body| body.client_build_profile_id.push('x')),
            Box::new(|body| body.macos_major += 1),
            Box::new(|body| body.anchors.search_box.x_parts_per_million += 1),
            Box::new(|body| body.anchors.first_result_row.y_parts_per_million += 1),
            Box::new(|body| body.anchors.compose_box.x_parts_per_million += 1),
            Box::new(|body| body.ocr_regions.title.width_parts_per_million += 1),
            Box::new(|body| body.ocr_regions.compose.height_parts_per_million += 1),
            Box::new(|body| body.ocr_regions.newest_outgoing.x_parts_per_million += 1),
            Box::new(|body| body.selftest.focus_indicator.push('x')),
            Box::new(|body| body.selftest.minimum_title_confidence_parts_per_million -= 1),
            Box::new(|body| body.issued_at_unix_seconds -= 1),
            Box::new(|body| body.expires_at_unix_seconds += 1),
        ];
        for mutation in mutate {
            let mut tampered = signed.clone();
            mutation(&mut tampered.body);
            assert_eq!(
                verify_calibration_profile(&tampered, &root, 2_000).unwrap_err(),
                SignedArtifactDenial::SignatureNotVerified
            );
        }
    }

    #[test]
    fn profile_validity_window_fails_closed_on_both_sides() {
        let signed = sign_calibration_profile(&profile_body(), &seed(7)).unwrap();
        let root = development_root(7);
        assert_eq!(
            verify_calibration_profile(&signed, &root, 999).unwrap_err(),
            SignedArtifactDenial::NotYetValid
        );
        assert_eq!(
            verify_calibration_profile(&signed, &root, 100_000).unwrap_err(),
            SignedArtifactDenial::Expired
        );
    }

    #[test]
    fn an_unknown_combination_is_unverified_and_never_permits_a_send() {
        let signed = sign_compatibility_matrix(&matrix_body(), &seed(9)).unwrap();
        let verified = verify_compatibility_matrix(&signed, &development_root(9), 2_000).unwrap();
        let unknown = compatibility_decision(&verified, "26A1", "4.1.13.269579");
        assert!(!unknown.known_combination);
        assert_eq!(unknown.state, CompatibilityState::Unverified);
        assert!(!unknown.state.permits_send());
        let blocked = compatibility_decision(&verified, "25G83", "4.1.14.900000");
        assert!(blocked.known_combination);
        assert!(!blocked.state.permits_send());
    }

    #[test]
    fn a_supported_combination_binds_only_to_its_named_profile() {
        let matrix = verify_compatibility_matrix(
            &sign_compatibility_matrix(&matrix_body(), &seed(9)).unwrap(),
            &development_root(9),
            2_000,
        )
        .unwrap();
        let profile = verify_calibration_profile(
            &sign_calibration_profile(&profile_body(), &seed(7)).unwrap(),
            &development_root(7),
            2_000,
        )
        .unwrap();
        let decision = compatibility_decision(&matrix, "25G83", "4.1.13.269579");
        assert!(bind_profile_to_compatibility(&profile, &decision, 26).is_ok());
        assert_eq!(
            bind_profile_to_compatibility(&profile, &decision, 25).unwrap_err(),
            SignedArtifactDenial::HostBuildMismatch
        );
        let mut renamed = profile.clone();
        renamed.profile.body.profile_id = "other".to_string();
        assert_eq!(
            bind_profile_to_compatibility(&renamed, &decision, 26).unwrap_err(),
            SignedArtifactDenial::ProfileNotInMatrix
        );
        let blocked = compatibility_decision(&matrix, "25G83", "4.1.14.900000");
        assert_eq!(
            bind_profile_to_compatibility(&profile, &blocked, 26).unwrap_err(),
            SignedArtifactDenial::CombinationNotSupported
        );
    }

    #[test]
    fn the_signed_matrix_carries_a_field_kill_switch() {
        let mut body = matrix_body();
        body.global_kill_switch_engaged = true;
        let signed = sign_compatibility_matrix(&body, &seed(9)).unwrap();
        let verified = verify_compatibility_matrix(&signed, &development_root(9), 2_000).unwrap();
        assert!(verified.matrix.body.global_kill_switch_engaged);
        // The switch is inside the signature, so it cannot be cleared in the
        // field without the release key.
        let mut cleared = signed.clone();
        cleared.body.global_kill_switch_engaged = false;
        assert_eq!(
            verify_compatibility_matrix(&cleared, &development_root(9), 2_000).unwrap_err(),
            SignedArtifactDenial::SignatureNotVerified
        );
    }

    #[test]
    fn structurally_invalid_documents_are_refused_before_any_signature_check() {
        let mut body = profile_body();
        body.anchors.search_box.x_parts_per_million = PARTS_PER_MILLION + 1;
        let signed = SignedCalibrationProfile {
            body,
            signature: "00".repeat(64),
        };
        assert_eq!(
            verify_calibration_profile(&signed, &development_root(7), 2_000).unwrap_err(),
            SignedArtifactDenial::StructurallyInvalid
        );
        let mut matrix = matrix_body();
        matrix.entries.reverse();
        let signed = SignedCompatibilityMatrix {
            body: matrix,
            signature: "00".repeat(64),
        };
        assert_eq!(
            verify_compatibility_matrix(&signed, &development_root(9), 2_000).unwrap_err(),
            SignedArtifactDenial::StructurallyInvalid
        );
    }

    #[test]
    fn the_pinned_release_trust_root_is_empty_unless_provisioned_at_build_time() {
        let pinned = SendTrustRoot::pinned().unwrap();
        assert!(pinned.development_public_keys.is_empty());
        if option_env!("GREENBUBBLES_SEND_RELEASE_PUBLIC_KEYS").is_none() {
            assert!(pinned.release_public_keys.is_empty());
        }
    }
}
