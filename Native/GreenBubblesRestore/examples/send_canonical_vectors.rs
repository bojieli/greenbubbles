//! Emits the cross-language canonical-encoding vectors for the send adapter.
//!
//! The Rust control plane and the Swift input helper both sign and verify the
//! same calibration profiles and both compute the same action-capability
//! binding digest. Those encodings are hand-written in two languages, so the
//! only way to keep them from drifting is to pin one fixture and assert
//! against it from both test suites. Regenerate with:
//!
//! ```text
//! cargo run --example send_canonical_vectors > ../../docs/send-canonical-vectors.json
//! ```

use greenbubbles_restore::send_contract::{
    capability_binding_sha256, normalized_send_text, normalized_send_text_sha256,
    ActionCapabilityEnvelope, SendRolloutStage, SEND_CONTRACT_VERSION,
};
use greenbubbles_restore::send_profile::{
    calibration_profile_signing_bytes, compatibility_matrix_signing_bytes,
    sign_calibration_profile, sign_compatibility_matrix, signing_key_public_hex,
    CalibrationAnchors, CalibrationOcrRegions, CalibrationProfileBody, CalibrationSelfTest,
    CompatibilityEntry, CompatibilityMatrixBody, CompatibilityState, WindowRelativePoint,
    WindowRelativeRect,
};
use sha2::{Digest, Sha256};

fn main() {
    let profile = CalibrationProfileBody {
        schema: 1,
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
        issued_at_unix_seconds: 1_756_000_000,
        expires_at_unix_seconds: 1_788_000_000,
    };
    let matrix = CompatibilityMatrixBody {
        schema: 1,
        matrix_id: "send-compat-2026-08-29".to_string(),
        issued_at_unix_seconds: 1_756_000_000,
        expires_at_unix_seconds: 1_788_000_000,
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
    };
    let body = "hello  from\nthe adapter ".to_string();
    let mut capability = ActionCapabilityEnvelope {
        format_version: SEND_CONTRACT_VERSION,
        capability_id: "11".repeat(32),
        action_id: "22".repeat(32),
        draft_id: "33".repeat(32),
        approval_id: "44".repeat(32),
        idempotency_key: "55".repeat(32),
        account_id: "canonical-account".to_string(),
        conversation_id: "filehelper".to_string(),
        search_key: "File Transfer".to_string(),
        expected_title: "File Transfer".to_string(),
        body_sha256: hex::encode(Sha256::digest(body.as_bytes())),
        normalized_body_sha256: normalized_send_text_sha256(&body),
        body,
        client_build_profile_id: "wechat-macos-4.1.13-269579".to_string(),
        calibration_profile_id: "wechat-4.1.13.269579-macos-26".to_string(),
        calibration_profile_sha256: "66".repeat(32),
        rollout_stage: SendRolloutStage::SelfSend,
        permit_send: true,
        issued_at_unix_nanoseconds: 1_756_000_000_000_000_000,
        valid_until_unix_nanoseconds: 1_756_000_120_000_000_000,
        binding_sha256: String::new(),
    };
    capability.binding_sha256 = capability_binding_sha256(&capability).unwrap();

    let normalization = [
        "  spaced   out  ",
        "line\nbreaks\r\nfolded",
        "\ttabs\tand spaces ",
    ]
    .into_iter()
    .map(|input| {
        serde_json::json!({
            "input": input,
            "normalized": normalized_send_text(input),
            "sha256": normalized_send_text_sha256(input),
        })
    })
    .collect::<Vec<_>>();

    // A signature produced by the Rust signer under a fixed development seed.
    // The Swift test suite verifies it with CryptoKit, which proves the two
    // implementations agree on the signature scheme, not just on the digest.
    const DEVELOPMENT_SEED: [u8; 32] = [7; 32];
    let signed_profile = sign_calibration_profile(&profile, &DEVELOPMENT_SEED).unwrap();
    let signed_matrix = sign_compatibility_matrix(&matrix, &DEVELOPMENT_SEED).unwrap();

    let document = serde_json::json!({
        "formatVersion": 1,
        "purpose":
            "Cross-language canonical-encoding vectors shared by the Rust control plane and the Swift input helper.",
        "calibrationProfile": {
            "body": profile,
            "canonicalSha256": hex::encode(Sha256::digest(
                calibration_profile_signing_bytes(&profile).unwrap(),
            )),
        },
        "compatibilityMatrix": {
            "body": matrix,
            "canonicalSha256": hex::encode(Sha256::digest(
                compatibility_matrix_signing_bytes(&matrix).unwrap(),
            )),
        },
        "actionCapability": capability,
        "normalizedText": normalization,
        "developmentSigning": {
            "publicKeyHex": signing_key_public_hex(&DEVELOPMENT_SEED),
            "signedCalibrationProfile": signed_profile,
            "signedCompatibilityMatrix": signed_matrix,
        },
    });
    println!("{}", serde_json::to_string_pretty(&document).unwrap());
}
