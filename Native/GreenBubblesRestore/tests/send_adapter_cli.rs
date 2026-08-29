//! End-to-end coverage of the `send` command group. The tests build a complete
//! owner-only workspace — signed profile, signed matrix, immutable draft,
//! local approval evidence — and prove that the command line stays fail-closed
//! without a pinned release key, without the input helper, and after the
//! approval has been consumed once.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use greenbubbles_restore::connector::{action_draft_identity, ActionDraft};
use serde_json::Value;
use tempfile::TempDir;

const ACCOUNT: &str = "cli-test-account";
const CONVERSATION: &str = "filehelper";
const BODY: &str = "adapter dry run";

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_greenbubbles-restore")
}

fn run(arguments: &[&str]) -> (bool, String, String) {
    let output = Command::new(binary())
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .expect("the send CLI should run");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn json(arguments: &[&str]) -> Value {
    let (success, stdout, stderr) = run(arguments);
    assert!(success, "{arguments:?} failed: {stderr}");
    serde_json::from_str(&stdout).expect("the send CLI emits JSON on standard output")
}

fn write_private(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

struct Workspace {
    root: TempDir,
    config: PathBuf,
    draft: PathBuf,
}

impl Workspace {
    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }
}

/// Builds a workspace whose calibration profile and compatibility matrix are
/// signed with a development key that the configuration names explicitly.
fn workspace(rollout_stage: &str) -> Workspace {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let key_path = root.path().join("signing-key.json");
    let key = json(&["send", "profile-keygen", key_path.to_str().unwrap()]);
    let public_key = key["publicKeyHex"].as_str().unwrap().to_string();

    let mut profile: Value = serde_json::from_str(&run(&["send", "profile-template"]).1).unwrap();
    profile["issuedAtUnixSeconds"] = Value::from(1_u64);
    profile["expiresAtUnixSeconds"] = Value::from(4_000_000_000_u64);
    let unsigned_profile = root.path().join("profile-body.json");
    write_private(&unsigned_profile, &serde_json::to_string(&profile).unwrap());
    let signed_profile = root.path().join("profile.json");
    json(&[
        "send",
        "profile-sign",
        unsigned_profile.to_str().unwrap(),
        signed_profile.to_str().unwrap(),
        "--signing-key-file",
        key_path.to_str().unwrap(),
    ]);

    let matrix = serde_json::json!({
        "schema": 1,
        "matrixId": "cli-test-matrix",
        "issuedAtUnixSeconds": 1,
        "expiresAtUnixSeconds": 4_000_000_000_u64,
        "globalKillSwitchEngaged": false,
        "entries": [{
            "macosBuild": "25G83",
            "macosMajor": 26,
            "wechatBuild": "4.1.13.269579",
            "clientBuildProfileId": "wechat-macos-4.1.13-269579",
            "state": "supported",
            "calibrationProfileId": "wechat-4.1.13.269579-macos-26",
            "note": "development fixture"
        }]
    });
    let unsigned_matrix = root.path().join("matrix-body.json");
    write_private(&unsigned_matrix, &serde_json::to_string(&matrix).unwrap());
    let signed_matrix = root.path().join("matrix.json");
    json(&[
        "send",
        "matrix-sign",
        unsigned_matrix.to_str().unwrap(),
        signed_matrix.to_str().unwrap(),
        "--signing-key-file",
        key_path.to_str().unwrap(),
    ]);

    let trust_root = root.path().join("trust-root.json");
    write_private(
        &trust_root,
        &serde_json::to_string(&serde_json::json!({
            "releasePublicKeys": [],
            "developmentPublicKeys": [public_key]
        }))
        .unwrap(),
    );

    let drafts = root.path().join("drafts");
    fs::create_dir(&drafts).unwrap();
    fs::set_permissions(&drafts, fs::Permissions::from_mode(0o700)).unwrap();
    // The draft must be live against the real clock: PRECHECK refuses an
    // expired draft, exactly as it would in production.
    let now_nanoseconds = unix_nanoseconds();
    let draft = serde_json::json!({
        "formatVersion": 1,
        "draftId": "0".repeat(64),
        "state": "draftOnly",
        "accountId": ACCOUNT,
        "conversationId": CONVERSATION,
        "recipient": {
            "conversationId": CONVERSATION,
            "kind": "direct",
            "humanLabel": "File Transfer",
            "participantCount": 0,
            "participants": [],
            "ownerParticipantId": null,
            "entityDecodeState": "complete",
            "sourceDatabaseFreshness": "fresh"
        },
        "replyTarget": null,
        "renderedText": BODY,
        "renderedTextSha256":
            "f8b4b4f0a9ee3fd4b25a1a0e3bbbaf0f1e2f2ca9e9ff56d9a5bbf7ec3fb0f0dd",
        "attachments": [],
        "connectorVersion": "1",
        "apiVersion": "1",
        "sourceFingerprint": "c".repeat(64),
        "policyDecisionId": "d".repeat(64),
        "requesterId": "local-owner",
        "createdAtUnixNanoseconds": now_nanoseconds,
        "expiresAtUnixNanoseconds": now_nanoseconds + 3_600_000_000_000_u64
    });
    let mut draft = draft;
    draft["renderedTextSha256"] = Value::from(hex_sha256(BODY));
    // The draft identity is content-addressed, so it must be derived from the
    // very fields the file carries.
    let typed: ActionDraft = serde_json::from_value(draft.clone()).unwrap();
    let draft_id = action_draft_identity(&typed);
    draft["draftId"] = Value::from(draft_id.clone());
    let draft_path = drafts.join(format!("{draft_id}.json"));
    write_private(&draft_path, &serde_json::to_string(&draft).unwrap());

    let mut config: Value = serde_json::from_str(&run(&["send", "config-template"]).1).unwrap();
    config["accountId"] = Value::from(ACCOUNT);
    config["allowList"]["accountIds"] = serde_json::json!([ACCOUNT]);
    config["rolloutStage"] = Value::from(rollout_stage);
    config["globalKillSwitchEngaged"] = Value::from(false);
    config["gate"]["gateDecisionId"] = Value::from("a".repeat(64));
    for flag in [
        "acquisitionGatePassed",
        "restorationGatePassed",
        "mechanismApproved",
        "legalReviewApproved",
    ] {
        config["gate"][flag] = Value::from(true);
    }
    config["calibrationProfilePath"] = Value::from(signed_profile.to_str().unwrap());
    config["compatibilityMatrixPath"] = Value::from(signed_matrix.to_str().unwrap());
    config["developmentTrustRootPath"] = Value::from(trust_root.to_str().unwrap());
    config["outboxDirectory"] = Value::from(root.path().join("outbox").to_str().unwrap());
    config["auditLogPath"] = Value::from(root.path().join("audit.ndjson").to_str().unwrap());
    config["draftDirectory"] = Value::from(drafts.to_str().unwrap());
    config["stagingRoot"] = Value::from(root.path().join("staging").to_str().unwrap());
    config["helper"]["dispatcherExecutable"] =
        Value::from(write_stub_dispatcher(root.path()).to_str().unwrap());
    let config_path = root.path().join("send-config.json");
    write_private(&config_path, &serde_json::to_string(&config).unwrap());

    Workspace {
        root,
        config: config_path,
        draft: draft_path,
    }
}

/// A stand-in for the packaged `greenbubbles-send` client. It speaks the exact
/// dispatcher protocol — one JSON request on standard input, one JSON response
/// on standard output — so the test exercises the real process boundary, the
/// real timeout, and the real envelope validation rather than a Rust double.
fn write_stub_dispatcher(root: &Path) -> PathBuf {
    let path = root.join("stub-dispatcher");
    fs::write(
        &path,
        r#"#!/usr/bin/env python3
import hashlib, json, sys

subcommand = sys.argv[1]
if subcommand == "capability-status":
    sys.stdin.read()
    print(json.dumps({
        "formatVersion": 1,
        "helperVersion": "1.0.0",
        "engineVersion": "1.0.0",
        "accessibilityGranted": True,
        "screenRecordingGranted": True,
        "wechatRunning": True,
        "wechatLoggedIn": True,
        "wechatBundleIdentifier": "com.tencent.xinWeChat",
        "wechatMarketingVersion": "4.1.13",
        "wechatBuild": "4.1.13.269579",
        "macosBuild": "25G83",
        "macosMajor": 26,
        "mainWindowFound": True,
        "activeCalibrationProfileId": "wechat-4.1.13.269579-macos-26",
        "engineHealthy": True,
        "boundedManifestScope": ["com.tencent.xinWeChat:click+clipboardWrite"],
        "observedAtUnixNanoseconds": 1,
    }))
elif subcommand == "execute-send":
    capability = json.loads(sys.stdin.read())
    assert capability["permitSend"] is False, "the stub refuses to press Return"
    print(json.dumps({
        "formatVersion": 1,
        "capabilityId": capability["capabilityId"],
        "capabilityBindingSha256": capability["bindingSha256"],
        "helperVersion": "1.0.0",
        "engineVersion": "1.0.0",
        "calibrationProfileId": capability["calibrationProfileId"],
        "stageReached": "contentVerify",
        "attempted": False,
        "visualConfirmation": "notAttempted",
        "failure": None,
        "evidence": {
            "titleConfidencePartsPerMillion": 1000000,
            "titleMatched": True,
            "composeMatched": True,
            "attachmentNameMatched": False,
            "attachmentStaged": False,
            "confirmationSheetConfirmed": False,
            "composeCleared": True,
            "newestOutgoingMatched": False,
            "ambiguousSearchResult": False,
            "humanActivityObserved": False,
            "windowFrameDigest": "0" * 64,
            "captureCount": 3,
            "elapsedMilliseconds": 640,
        },
        "observedAtUnixNanoseconds": 2,
    }))
else:
    sys.exit(2)
"#,
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

/// The wall clock in Unix nanoseconds, matching the control plane's own clock.
fn unix_nanoseconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the system clock is after the Unix epoch")
        .as_nanos() as u64
}

fn hex_sha256(value: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(value.as_bytes()))
}

#[test]
fn send_help_states_the_fail_closed_posture_and_every_subcommand() {
    for flag in ["--help", "-h"] {
        let (success, stdout, _) = run(&["send", flag]);
        assert!(success);
        assert!(stdout.starts_with("Usage:\n"));
        for subcommand in [
            "config-template",
            "profile-keygen",
            "profile-sign",
            "profile-verify",
            "matrix-sign",
            "doctor",
            "selftest",
            "approval-binding",
            "approve",
            "precheck",
            "submit",
            "outbox-status",
            "reconcile",
        ] {
            assert!(stdout.contains(subcommand), "help omits {subcommand}");
        }
        assert!(stdout.contains("observedSent` is created only by replica reconciliation"));
    }
    assert!(run(&["help", "send"]).1.starts_with("Usage:\n"));
}

#[test]
fn the_emitted_configuration_template_is_closed_by_default() {
    let config: Value = serde_json::from_str(&run(&["send", "config-template"]).1).unwrap();
    assert_eq!(config["rolloutStage"], "dryRun");
    assert_eq!(config["globalKillSwitchEngaged"], true);
    for flag in [
        "acquisitionGatePassed",
        "restorationGatePassed",
        "mechanismApproved",
        "legalReviewApproved",
    ] {
        assert_eq!(config["gate"][flag], false, "{flag} should start false");
    }
}

#[test]
fn a_release_signed_artifact_cannot_verify_without_a_pinned_release_key() {
    let workspace = workspace("dryRun");
    let signed_profile = workspace.path("profile.json");
    let (success, _, stderr) = run(&["send", "profile-verify", signed_profile.to_str().unwrap()]);
    assert!(!success);
    assert!(
        stderr.contains("trustRootEmpty") || stderr.contains("signatureNotVerified"),
        "unexpected refusal: {stderr}"
    );
}

#[test]
fn a_development_trust_root_verifies_only_when_named_explicitly() {
    let workspace = workspace("dryRun");
    let verified = json(&[
        "send",
        "profile-verify",
        workspace.path("profile.json").to_str().unwrap(),
        "--development-trust-root",
        workspace.path("trust-root.json").to_str().unwrap(),
    ]);
    assert_eq!(verified["trustTier"], "development");
    assert_eq!(
        verified["profile"]["profileId"],
        "wechat-4.1.13.269579-macos-26"
    );
    let matrix = json(&[
        "send",
        "matrix-verify",
        workspace.path("matrix.json").to_str().unwrap(),
        "--development-trust-root",
        workspace.path("trust-root.json").to_str().unwrap(),
    ]);
    assert_eq!(matrix["trustTier"], "development");
}

#[test]
fn doctor_reports_a_closed_send_path_and_names_the_action_for_each_reason() {
    let workspace = workspace("dryRun");
    let report = json(&[
        "send",
        "doctor",
        workspace.config.to_str().unwrap(),
        "--no-helper",
    ]);
    assert_eq!(report["sendPathOpen"], false);
    assert_eq!(report["configurationValid"], true);
    assert_eq!(report["gateEvidenceComplete"], true);
    assert_eq!(report["helperFailure"], "engineUnavailable");
    let actions = report["operatorActions"].as_array().unwrap();
    assert_eq!(
        actions.len(),
        report["blockingFailures"].as_array().unwrap().len()
    );
    assert!(actions
        .iter()
        .all(|action| action.as_str().unwrap().len() > 16));
    assert_eq!(report["calibration"]["trustTier"], "development");
    assert_eq!(report["compatibility"]["state"], "supported");
}

#[test]
fn approval_requires_confirmation_and_binds_to_exactly_one_draft() {
    let workspace = workspace("dryRun");
    let binding = json(&[
        "send",
        "approval-binding",
        workspace.config.to_str().unwrap(),
        workspace.draft.to_str().unwrap(),
    ]);
    assert_eq!(binding["humanRecipient"], "File Transfer");
    assert_eq!(
        binding["immutableBindingSha256"].as_str().unwrap().len(),
        64
    );

    let approval_path = workspace.path("approval.json");
    let (success, _, stderr) = run(&[
        "send",
        "approve",
        workspace.config.to_str().unwrap(),
        workspace.draft.to_str().unwrap(),
        approval_path.to_str().unwrap(),
        "--approver",
        "local-owner",
    ]);
    assert!(!success);
    assert!(stderr.contains("--confirm"));
    assert!(!approval_path.exists());

    let approval = json(&[
        "send",
        "approve",
        workspace.config.to_str().unwrap(),
        workspace.draft.to_str().unwrap(),
        approval_path.to_str().unwrap(),
        "--approver",
        "local-owner",
        "--confirm",
    ]);
    assert_eq!(approval["approvalId"].as_str().unwrap().len(), 64);
    assert_eq!(approval["idempotencyKey"].as_str().unwrap().len(), 64);
    let mode = fs::metadata(&approval_path).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0);
    let recorded: Value = serde_json::from_slice(&fs::read(&approval_path).unwrap()).unwrap();
    assert_eq!(
        recorded["immutableBindingSha256"],
        binding["immutableBindingSha256"]
    );
}

#[test]
fn precheck_without_the_helper_denies_and_never_permits_a_send() {
    let workspace = workspace("dryRun");
    let approval_path = workspace.path("approval.json");
    json(&[
        "send",
        "approve",
        workspace.config.to_str().unwrap(),
        workspace.draft.to_str().unwrap(),
        approval_path.to_str().unwrap(),
        "--approver",
        "local-owner",
        "--confirm",
    ]);
    let report = json(&[
        "send",
        "precheck",
        workspace.config.to_str().unwrap(),
        workspace.draft.to_str().unwrap(),
        approval_path.to_str().unwrap(),
        "--no-helper",
    ]);
    assert_eq!(report["permitted"], false);
    assert_eq!(report["permitSend"], false);
    assert!(report["failures"]
        .as_array()
        .unwrap()
        .iter()
        .any(|failure| failure == "engineUnavailable"));
    assert!(report["guardDenials"].as_array().unwrap().is_empty());
}

#[test]
fn a_self_send_configuration_refuses_a_development_signed_profile() {
    let workspace = workspace("selfSend");
    let approval_path = workspace.path("approval.json");
    json(&[
        "send",
        "approve",
        workspace.config.to_str().unwrap(),
        workspace.draft.to_str().unwrap(),
        approval_path.to_str().unwrap(),
        "--approver",
        "local-owner",
        "--confirm",
    ]);
    let report = json(&[
        "send",
        "precheck",
        workspace.config.to_str().unwrap(),
        workspace.draft.to_str().unwrap(),
        approval_path.to_str().unwrap(),
        "--no-helper",
    ]);
    assert_eq!(report["permitSend"], false);
    assert!(report["failures"]
        .as_array()
        .unwrap()
        .iter()
        .any(|failure| failure == "profileInvalid"));
}

#[test]
fn the_outbox_starts_empty_and_reports_a_bounded_attempt_window() {
    let workspace = workspace("dryRun");
    let status = json(&["send", "outbox-status", workspace.config.to_str().unwrap()]);
    assert_eq!(status["outbox"]["inFlight"], false);
    assert_eq!(status["outbox"]["pendingReconciliationCount"], 0);
    assert_eq!(status["outbox"]["circuitBreakerOpen"], false);
    assert_eq!(status["outbox"]["attemptWindowRemaining"], 3);
    assert_eq!(status["recovery"]["recoveredAttempt"], false);
    assert!(status["pendingReconciliation"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn a_world_readable_configuration_is_refused() {
    let workspace = workspace("dryRun");
    fs::set_permissions(&workspace.config, fs::Permissions::from_mode(0o644)).unwrap();
    let (success, _, stderr) = run(&[
        "send",
        "doctor",
        workspace.config.to_str().unwrap(),
        "--no-helper",
    ]);
    assert!(!success);
    assert!(
        stderr.contains("owner-only"),
        "unexpected refusal: {stderr}"
    );
}

#[test]
fn a_dry_run_submit_crosses_the_real_dispatcher_boundary_and_leaves_a_verified_journal() {
    let workspace = workspace("dryRun");
    let approval_path = workspace.path("approval.json");
    let approval = json(&[
        "send",
        "approve",
        workspace.config.to_str().unwrap(),
        workspace.draft.to_str().unwrap(),
        approval_path.to_str().unwrap(),
        "--approver",
        "local-owner",
        "--confirm",
    ]);

    let precheck = json(&[
        "send",
        "precheck",
        workspace.config.to_str().unwrap(),
        workspace.draft.to_str().unwrap(),
        approval_path.to_str().unwrap(),
    ]);
    assert_eq!(precheck["permitted"], true, "precheck: {precheck}");
    assert_eq!(precheck["permitSend"], false);

    let report = json(&[
        "send",
        "submit",
        workspace.config.to_str().unwrap(),
        workspace.draft.to_str().unwrap(),
        approval_path.to_str().unwrap(),
    ]);
    assert_eq!(report["dispatched"], true);
    assert_eq!(report["attempted"], false);
    assert_eq!(report["completion"], "dryRunCompleted");
    assert_eq!(report["awaitingReconciliation"], false);
    assert_eq!(report["lifecycleState"], Value::Null);
    assert_eq!(report["recallDeadlineUnixNanoseconds"], Value::Null);
    assert_eq!(report["idempotencyKey"], approval["idempotencyKey"]);
    assert_eq!(report["auditEventCount"], 3);

    // The same approval can never be dispatched twice.
    let second = json(&[
        "send",
        "submit",
        workspace.config.to_str().unwrap(),
        workspace.draft.to_str().unwrap(),
        approval_path.to_str().unwrap(),
    ]);
    assert_eq!(second["dispatched"], false);
    assert!(second["precheck"]["failures"]
        .as_array()
        .unwrap()
        .iter()
        .any(|failure| failure == "idempotencyConflict"));

    // The journal chain verifies and holds no message body.
    let audit = json(&[
        "audit-connector-log",
        workspace.path("audit.ndjson").to_str().unwrap(),
    ]);
    assert_eq!(audit["chainVerified"], true);
    assert_eq!(audit["fullyChained"], true);
    assert_eq!(audit["approvalEventCount"], 2);
    assert_eq!(audit["attemptEventCount"], 1);
    assert_eq!(audit["reconciliationEventCount"], 1);
    let journal = fs::read_to_string(workspace.path("audit.ndjson")).unwrap();
    assert!(!journal.contains(BODY));

    // The outbox remembers the key and stays idle.
    let status = json(&["send", "outbox-status", workspace.config.to_str().unwrap()]);
    assert_eq!(status["outbox"]["inFlight"], false);
    assert_eq!(status["outbox"]["pendingReconciliationCount"], 0);
    assert_eq!(status["outbox"]["completedAttemptTotal"], 1);
    assert_eq!(status["outbox"]["circuitBreakerOpen"], false);

    // A dry run never opens a recall window.
    let recall = json(&[
        "send",
        "recall-window",
        workspace.config.to_str().unwrap(),
        "--idempotency-key",
        report["idempotencyKey"].as_str().unwrap(),
    ]);
    assert_eq!(recall["attempted"], false);
    assert_eq!(recall["recallable"], false);
}
