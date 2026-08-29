//! Pins the cross-language canonical encodings. The Swift input helper asserts
//! against the same fixture, so a change to either encoder that is not made in
//! both languages fails here.

use std::path::PathBuf;

use greenbubbles::send_contract::{
    capability_binding_sha256, normalized_send_text, normalized_send_text_sha256,
    ActionCapabilityEnvelope, HelperSendOutcome,
};
use greenbubbles::send_profile::{
    calibration_profile_signing_bytes, compatibility_matrix_signing_bytes, CalibrationProfileBody,
    CompatibilityMatrixBody,
};
use sha2::{Digest, Sha256};

fn vectors() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/send-canonical-vectors.json")
        .canonicalize()
        .expect("the shared vector fixture must exist");
    serde_json::from_slice(&std::fs::read(path).expect("vector fixture is readable"))
        .expect("vector fixture is valid JSON")
}

#[test]
fn the_calibration_profile_encoder_matches_the_shared_fixture() {
    let vectors = vectors();
    let body: CalibrationProfileBody =
        serde_json::from_value(vectors["calibrationProfile"]["body"].clone()).unwrap();
    let expected = vectors["calibrationProfile"]["canonicalSha256"]
        .as_str()
        .unwrap();
    let actual = hex::encode(Sha256::digest(
        calibration_profile_signing_bytes(&body).unwrap(),
    ));
    assert_eq!(actual, expected);
}

#[test]
fn the_compatibility_matrix_encoder_matches_the_shared_fixture() {
    let vectors = vectors();
    let body: CompatibilityMatrixBody =
        serde_json::from_value(vectors["compatibilityMatrix"]["body"].clone()).unwrap();
    let expected = vectors["compatibilityMatrix"]["canonicalSha256"]
        .as_str()
        .unwrap();
    let actual = hex::encode(Sha256::digest(
        compatibility_matrix_signing_bytes(&body).unwrap(),
    ));
    assert_eq!(actual, expected);
}

#[test]
fn the_action_capability_binding_matches_the_shared_fixture() {
    let vectors = vectors();
    let capability: ActionCapabilityEnvelope =
        serde_json::from_value(vectors["actionCapability"].clone()).unwrap();
    assert_eq!(
        capability_binding_sha256(&capability).as_deref(),
        Some(capability.binding_sha256.as_str())
    );
    assert!(capability
        .validate(capability.issued_at_unix_nanoseconds + 1)
        .is_ok());
}

#[test]
fn the_attachment_capability_binding_matches_the_shared_fixture() {
    let vectors = vectors();
    let capability: ActionCapabilityEnvelope =
        serde_json::from_value(vectors["attachmentCapability"].clone()).unwrap();
    assert_eq!(
        capability_binding_sha256(&capability).as_deref(),
        Some(capability.binding_sha256.as_str())
    );
    let attachment = capability
        .attachment
        .as_ref()
        .expect("an attachment vector");
    assert!(capability.body.is_empty());
    assert_eq!(attachment.display_file_name, "photo.png");
    assert!(capability
        .validate(capability.issued_at_unix_nanoseconds + 1)
        .is_ok());
}

#[test]
fn the_helper_outcome_envelope_matches_the_shared_fixture() {
    // Decoding is strict on both sides, so this fails the moment one language
    // gains an evidence field the other lacks.
    let vectors = vectors();
    let outcome: HelperSendOutcome =
        serde_json::from_value(vectors["helperSendOutcome"].clone()).unwrap();
    let capability: ActionCapabilityEnvelope =
        serde_json::from_value(vectors["actionCapability"].clone()).unwrap();
    assert!(outcome.attempted);
    assert!(outcome.validate_against(&capability).is_ok());
}

#[test]
fn the_text_normalizer_matches_the_shared_fixture() {
    let vectors = vectors();
    for case in vectors["normalizedText"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        assert_eq!(
            normalized_send_text(input),
            case["normalized"].as_str().unwrap()
        );
        assert_eq!(
            normalized_send_text_sha256(input),
            case["sha256"].as_str().unwrap()
        );
    }
}
