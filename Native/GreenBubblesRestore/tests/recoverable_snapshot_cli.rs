use std::fs;
use std::io::{Read, Write};
use std::os::raw::c_void;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use bip39::{Language, Mnemonic};
use rusqlite::{params, Connection};
use serde_json::Value;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

const WECHAT_KEY: [u8; 32] = [0xAB; 32];
const RECOVERY_KEY: [u8; 32] = [0x37; 32];
const ROTATED_RECOVERY_KEY: [u8; 32] = [0x59; 32];

#[test]
fn help_documents_argon2id_access_and_secret_input_ordering() {
    for arguments in [
        vec!["source", "status", "--help"],
        vec!["messages", "search", "--help"],
        vec!["attachment", "inspect", "--help"],
        vec!["snapshot", "create", "--help"],
        vec!["snapshot", "verify", "--help"],
        vec!["snapshot", "rewrap", "--help"],
        vec!["snapshot", "retention", "quarantine", "--help"],
        vec!["snapshot", "retention", "restore", "--help"],
        vec!["connector-query-direct", "--help"],
    ] {
        let output = run(&arguments, None);
        assert_success(&output);
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(
            help.contains("passphrase"),
            "passphrase syntax missing from: {arguments:?}"
        );
    }

    let create = run(&["snapshot", "create", "--help"], None);
    let create_help = String::from_utf8(create.stdout).unwrap();
    assert!(create_help.contains("Optional Argon2id protector"));
    assert!(create_help.contains("source key"));

    let search = run(&["messages", "search", "--help"], None);
    let search_help = String::from_utf8(search.stdout).unwrap();
    assert!(search_help.contains("snapshot passphrase"));
    assert!(search_help.contains("all remaining"));

    let rewrap = run(&["snapshot", "rewrap", "--help"], None);
    let rewrap_help = String::from_utf8(rewrap.stdout).unwrap();
    assert!(rewrap_help.contains("stdin line 1 is the old passphrase"));
    assert!(rewrap_help.contains("line 2 is"));
    assert!(rewrap_help.contains("the new passphrase"));
}

#[test]
fn bip39_recovery_kit_wraps_a_distinct_database_key_and_survives_source_loss() {
    let fixture = EncryptedFixture::new();
    let kit = fixture.parent.join(".family-a-recovery-words.txt");
    let kit_created = run(
        &["snapshot", "recovery-kit", "create", kit.to_str().unwrap()],
        None,
    );
    assert_success(&kit_created);
    let report: Value = serde_json::from_slice(&kit_created.stdout).unwrap();
    assert_eq!(report["schema"], "greenbubbles.recovery-kit.v1");
    assert_eq!(report["wordCount"], 24);
    assert_eq!(report["checksumValidated"], true);
    assert_eq!(report["fileCreated"], true);
    assert!(!String::from_utf8_lossy(&kit_created.stdout).contains(kit.to_str().unwrap()));
    let metadata = fs::metadata(&kit).unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert_eq!(metadata.nlink(), 1);

    let kit_text = fs::read_to_string(&kit).unwrap();
    let words = kit_text
        .lines()
        .find_map(|line| line.strip_prefix("words: "))
        .unwrap();
    assert_eq!(words.split_whitespace().count(), 24);
    let mnemonic = Mnemonic::parse_in_normalized(Language::English, words).unwrap();
    let recovery_entropy = mnemonic.to_entropy();
    assert_eq!(recovery_entropy.len(), 32);
    assert!(!String::from_utf8_lossy(&kit_created.stdout).contains(words));

    let validated = run(
        &[
            "snapshot",
            "recovery-kit",
            "validate",
            kit.to_str().unwrap(),
        ],
        None,
    );
    assert_success(&validated);
    let validated: Value = serde_json::from_slice(&validated.stdout).unwrap();
    assert_eq!(validated["fileCreated"], false);

    let wrong_kit = fixture.parent.join(".wrong-recovery-words.txt");
    assert_success(&run(
        &[
            "snapshot",
            "recovery-kit",
            "create",
            wrong_kit.to_str().unwrap(),
        ],
        None,
    ));

    let snapshot = fixture.parent.join("wrapped-snapshot");
    let source_input = format!("{}\n", hex::encode(WECHAT_KEY));
    let created = run(
        &[
            "snapshot",
            "create",
            fixture.source.to_str().unwrap(),
            snapshot.to_str().unwrap(),
            "--source-passphrase-stdin",
            "--snapshot-recovery-kit",
            kit.to_str().unwrap(),
        ],
        Some(source_input.as_bytes()),
    );
    assert_success(&created);
    let manifest: Value = serde_json::from_slice(&created.stdout).unwrap();
    assert_eq!(manifest["schema"], "greenbubbles.recoverable-snapshot.v2");
    assert_eq!(manifest["formatVersion"], 2);
    assert_eq!(
        manifest["protection"]["recoveryProtector"],
        "multiProtectorEnvelopeV1"
    );
    assert_eq!(
        manifest["protection"]["protectors"][0]["kind"],
        "bip39English24"
    );
    assert_eq!(manifest["protection"]["protectors"][0]["portable"], true);
    let manifest_text = fs::read_to_string(snapshot.join("manifest.json")).unwrap();
    assert!(!manifest_text.contains(words));
    assert!(!manifest_text.contains(&hex::encode(&recovery_entropy)));
    assert!(!manifest_text.contains(&hex::encode(WECHAT_KEY)));
    assert_snapshot_has_only_encrypted_databases_without_sidecars(&snapshot);

    fs::remove_dir_all(&fixture.source).unwrap();
    let verified = run(
        &[
            "snapshot",
            "verify",
            snapshot.to_str().unwrap(),
            "--snapshot-recovery-kit",
            kit.to_str().unwrap(),
        ],
        None,
    );
    assert_success(&verified);
    let verified: Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(verified["recoveryVerifiedWithoutWechatKey"], true);
    assert_eq!(verified["portableRecoveryProtectorVerified"], true);
    assert_eq!(verified["protectorCount"], 1);

    let messages = run(
        &[
            "messages",
            "list",
            snapshot.to_str().unwrap(),
            "--snapshot-recovery-kit",
            kit.to_str().unwrap(),
            "--conversation",
            "wxid_talker",
            "--limit",
            "2",
        ],
        None,
    );
    assert_success(&messages);
    let messages: Value = serde_json::from_slice(&messages.stdout).unwrap();
    assert_eq!(messages["items"][0]["content"]["Text"], "newer");
    assert_eq!(messages["items"][1]["content"]["Text"], "older");

    let search = run(
        &[
            "messages",
            "search",
            snapshot.to_str().unwrap(),
            "--snapshot-recovery-kit",
            kit.to_str().unwrap(),
            "--query-stdin",
        ],
        Some(b"recoverable\n"),
    );
    assert_success(&search);
    let search: Value = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(search["items"][0]["snippet"], "recoverable hello");

    let wrong_words = run(
        &[
            "snapshot",
            "verify",
            snapshot.to_str().unwrap(),
            "--snapshot-recovery-kit",
            wrong_kit.to_str().unwrap(),
        ],
        None,
    );
    assert!(!wrong_words.status.success());

    // The mnemonic entropy is a wrapping credential, not the SQLCipher DEK.
    let entropy_input = format!("{}\n", hex::encode(recovery_entropy));
    let direct_entropy = run(
        &[
            "snapshot",
            "verify",
            snapshot.to_str().unwrap(),
            "--snapshot-key-stdin",
        ],
        Some(entropy_input.as_bytes()),
    );
    assert!(!direct_entropy.status.success());

    fs::set_permissions(&kit, fs::Permissions::from_mode(0o644)).unwrap();
    let unsafe_kit = run(
        &[
            "snapshot",
            "recovery-kit",
            "validate",
            kit.to_str().unwrap(),
        ],
        None,
    );
    assert!(!unsafe_kit.status.success());
}

#[test]
fn local_credential_reopens_snapshot_while_recovery_words_remain_the_backup() {
    let fixture = EncryptedFixture::new();
    let kit = fixture.parent.join(".portable-recovery-words.txt");
    let local = fixture.parent.join(".greenbubbles-local-unlock");
    let wrong_local = fixture.parent.join(".wrong-local-unlock");
    assert_success(&run(
        &["snapshot", "recovery-kit", "create", kit.to_str().unwrap()],
        None,
    ));
    let local_created = run(
        &[
            "snapshot",
            "local-credential",
            "create",
            local.to_str().unwrap(),
        ],
        None,
    );
    assert_success(&local_created);
    let local_report: Value = serde_json::from_slice(&local_created.stdout).unwrap();
    assert_eq!(
        local_report["schema"],
        "greenbubbles.local-unlock-credential.v1"
    );
    assert_eq!(local_report["localConvenience"], true);
    assert_eq!(local_report["portable"], false);
    assert!(!String::from_utf8_lossy(&local_created.stdout).contains(local.to_str().unwrap()));
    assert_eq!(fs::metadata(&local).unwrap().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(&local).unwrap().nlink(), 1);
    assert_success(&run(
        &[
            "snapshot",
            "local-credential",
            "validate",
            local.to_str().unwrap(),
        ],
        None,
    ));
    assert_success(&run(
        &[
            "snapshot",
            "local-credential",
            "create",
            wrong_local.to_str().unwrap(),
        ],
        None,
    ));

    let local_text = fs::read_to_string(&local).unwrap();
    let local_secret = local_text
        .lines()
        .find_map(|line| line.strip_prefix("secret: "))
        .unwrap();
    assert_eq!(URL_SAFE_NO_PAD.decode(local_secret).unwrap().len(), 32);

    let snapshot = fixture.parent.join("dual-protector-snapshot");
    let source_input = format!("{}\n", hex::encode(WECHAT_KEY));
    let created = run(
        &[
            "snapshot",
            "create",
            fixture.source.to_str().unwrap(),
            snapshot.to_str().unwrap(),
            "--source-passphrase-stdin",
            "--snapshot-recovery-kit",
            kit.to_str().unwrap(),
            "--snapshot-local-credential",
            local.to_str().unwrap(),
        ],
        Some(source_input.as_bytes()),
    );
    assert_success(&created);
    let manifest: Value = serde_json::from_slice(&created.stdout).unwrap();
    let protectors = manifest["protection"]["protectors"].as_array().unwrap();
    assert_eq!(protectors.len(), 2);
    assert!(protectors
        .iter()
        .any(|protector| protector["kind"] == "bip39English24" && protector["portable"] == true));
    assert!(protectors
        .iter()
        .any(|protector| protector["kind"] == "localCredentialV1"
            && protector["portable"] == false
            && protector["credentialId"].is_string()));
    let manifest_text = fs::read_to_string(snapshot.join("manifest.json")).unwrap();
    assert!(!manifest_text.contains(local_secret));
    assert!(!manifest_text.contains(&hex::encode(WECHAT_KEY)));

    fs::remove_dir_all(&fixture.source).unwrap();
    let verified_local = run(
        &[
            "snapshot",
            "verify",
            snapshot.to_str().unwrap(),
            "--snapshot-local-credential",
            local.to_str().unwrap(),
        ],
        None,
    );
    assert_success(&verified_local);
    let verified: Value = serde_json::from_slice(&verified_local.stdout).unwrap();
    assert_eq!(verified["recoveryVerifiedWithoutWechatKey"], true);
    assert_eq!(verified["protectorCount"], 2);
    assert_eq!(verified["localConvenienceProtectorCount"], 1);
    assert_eq!(verified["portableRecoveryProtectorVerified"], true);

    let messages = run(
        &[
            "messages",
            "list",
            snapshot.to_str().unwrap(),
            "--snapshot-local-credential",
            local.to_str().unwrap(),
            "--conversation",
            "wxid_talker",
            "--limit",
            "1",
        ],
        None,
    );
    assert_success(&messages);
    let messages: Value = serde_json::from_slice(&messages.stdout).unwrap();
    assert_eq!(messages["items"][0]["content"]["Text"], "newer");

    let wrong = run(
        &[
            "snapshot",
            "verify",
            snapshot.to_str().unwrap(),
            "--snapshot-local-credential",
            wrong_local.to_str().unwrap(),
        ],
        None,
    );
    assert!(!wrong.status.success());

    fs::remove_file(&local).unwrap();
    let recovered = run(
        &[
            "snapshot",
            "verify",
            snapshot.to_str().unwrap(),
            "--snapshot-recovery-kit",
            kit.to_str().unwrap(),
        ],
        None,
    );
    assert_success(&recovered);
}

#[test]
fn argon2id_passphrase_is_optional_while_24_word_recovery_remains_mandatory() {
    let fixture = EncryptedFixture::new();
    let kit = fixture.parent.join(".passphrase-recovery-words.txt");
    assert_success(&run(
        &["snapshot", "recovery-kit", "create", kit.to_str().unwrap()],
        None,
    ));
    let snapshot = fixture.parent.join("passphrase-protected-snapshot");
    let passphrase = "correct horse battery staple for GreenBubbles";
    let create_input = format!("{}\n{passphrase}\n", hex::encode(WECHAT_KEY));
    let created = run(
        &[
            "snapshot",
            "create",
            fixture.source.to_str().unwrap(),
            snapshot.to_str().unwrap(),
            "--source-passphrase-stdin",
            "--snapshot-recovery-kit",
            kit.to_str().unwrap(),
            "--snapshot-passphrase-stdin",
        ],
        Some(create_input.as_bytes()),
    );
    assert_success(&created);
    let manifest: Value = serde_json::from_slice(&created.stdout).unwrap();
    let protectors = manifest["protection"]["protectors"].as_array().unwrap();
    assert_eq!(protectors.len(), 2);
    assert!(protectors
        .iter()
        .any(|protector| protector["kind"] == "bip39English24"));
    let passphrase_protector = protectors
        .iter()
        .find(|protector| protector["kind"] == "argon2idPassphraseV1")
        .unwrap();
    assert_eq!(
        passphrase_protector["keyDerivation"],
        "argon2idV19-m65536-t3-p1"
    );
    assert_eq!(passphrase_protector["portable"], true);
    let manifest_text = fs::read_to_string(snapshot.join("manifest.json")).unwrap();
    assert!(!manifest_text.contains(passphrase));
    assert!(!String::from_utf8_lossy(&created.stdout).contains(passphrase));

    fs::remove_dir_all(&fixture.source).unwrap();
    let passphrase_input = format!("{passphrase}\n");
    let verified = run(
        &[
            "snapshot",
            "verify",
            snapshot.to_str().unwrap(),
            "--snapshot-passphrase-stdin",
        ],
        Some(passphrase_input.as_bytes()),
    );
    assert_success(&verified);
    let report: Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(report["passphraseProtectorCount"], 1);
    assert_eq!(report["portableRecoveryProtectorVerified"], true);

    let messages = run(
        &[
            "messages",
            "list",
            snapshot.to_str().unwrap(),
            "--snapshot-passphrase-stdin",
            "--conversation",
            "wxid_talker",
            "--limit",
            "1",
        ],
        Some(passphrase_input.as_bytes()),
    );
    assert_success(&messages);
    let messages: Value = serde_json::from_slice(&messages.stdout).unwrap();
    assert_eq!(messages["items"][0]["content"]["Text"], "newer");

    let search_input = format!("{passphrase}\nrecoverable\n");
    let search = run(
        &[
            "messages",
            "search",
            snapshot.to_str().unwrap(),
            "--snapshot-passphrase-stdin",
            "--query-stdin",
        ],
        Some(search_input.as_bytes()),
    );
    assert_success(&search);
    let search: Value = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(search["items"][0]["snippet"], "recoverable hello");

    let wrong = run(
        &[
            "snapshot",
            "verify",
            snapshot.to_str().unwrap(),
            "--snapshot-passphrase-stdin",
        ],
        Some(b"this passphrase is definitely incorrect\n"),
    );
    assert!(!wrong.status.success());

    let words = run(
        &[
            "snapshot",
            "verify",
            snapshot.to_str().unwrap(),
            "--snapshot-recovery-kit",
            kit.to_str().unwrap(),
        ],
        None,
    );
    assert_success(&words);

    let next_kit = fixture.parent.join(".next-passphrase-recovery-words.txt");
    assert_success(&run(
        &[
            "snapshot",
            "recovery-kit",
            "create",
            next_kit.to_str().unwrap(),
        ],
        None,
    ));
    let next_passphrase = "a distinct replacement passphrase for GreenBubbles";
    let rewrapped = fixture.parent.join("passphrase-rewrapped-snapshot");
    let rewrap_input = format!("{passphrase}\n{next_passphrase}\n");
    let rewrap = run(
        &[
            "snapshot",
            "rewrap",
            snapshot.to_str().unwrap(),
            rewrapped.to_str().unwrap(),
            "--old-snapshot-passphrase-stdin",
            "--new-snapshot-recovery-kit",
            next_kit.to_str().unwrap(),
            "--new-snapshot-passphrase-stdin",
        ],
        Some(rewrap_input.as_bytes()),
    );
    assert_success(&rewrap);
    let rewrapped_manifest: Value = serde_json::from_slice(&rewrap.stdout).unwrap();
    assert!(rewrapped_manifest["protection"]["protectors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|protector| protector["kind"] == "bip39English24"));
    assert!(rewrapped_manifest["protection"]["protectors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|protector| protector["kind"] == "argon2idPassphraseV1"));

    let next_passphrase_input = format!("{next_passphrase}\n");
    assert_success(&run(
        &[
            "snapshot",
            "verify",
            rewrapped.to_str().unwrap(),
            "--snapshot-passphrase-stdin",
        ],
        Some(next_passphrase_input.as_bytes()),
    ));
    let old_passphrase_rejected = run(
        &[
            "snapshot",
            "verify",
            rewrapped.to_str().unwrap(),
            "--snapshot-passphrase-stdin",
        ],
        Some(passphrase_input.as_bytes()),
    );
    assert!(!old_passphrase_rejected.status.success());
    assert_success(&run(
        &[
            "snapshot",
            "verify",
            rewrapped.to_str().unwrap(),
            "--snapshot-recovery-kit",
            next_kit.to_str().unwrap(),
        ],
        None,
    ));

    let quarantine = fixture.parent.join("passphrase-retention-quarantine");
    fs::create_dir(&quarantine).unwrap();
    fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)).unwrap();
    let retired = run(
        &[
            "snapshot",
            "retention",
            "quarantine",
            snapshot.to_str().unwrap(),
            rewrapped.to_str().unwrap(),
            quarantine.to_str().unwrap(),
            "--retiring-snapshot-passphrase-stdin",
            "--replacement-recovery-kit",
            next_kit.to_str().unwrap(),
        ],
        Some(passphrase_input.as_bytes()),
    );
    assert_success(&retired);
    assert!(!snapshot.exists());
    let quarantined = fs::read_dir(&quarantine)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let restored = run(
        &[
            "snapshot",
            "retention",
            "restore",
            quarantined.to_str().unwrap(),
            snapshot.to_str().unwrap(),
            "--snapshot-passphrase-stdin",
        ],
        Some(passphrase_input.as_bytes()),
    );
    assert_success(&restored);
    assert!(snapshot.exists());
}

#[test]
fn wrapped_snapshot_rejects_tampered_envelopes_and_nonportable_manifests() {
    let fixture = EncryptedFixture::new();
    let kit = fixture.parent.join(".tamper-recovery-words.txt");
    let local = fixture.parent.join(".tamper-local-unlock");
    assert_success(&run(
        &["snapshot", "recovery-kit", "create", kit.to_str().unwrap()],
        None,
    ));
    assert_success(&run(
        &[
            "snapshot",
            "local-credential",
            "create",
            local.to_str().unwrap(),
        ],
        None,
    ));
    let snapshot = fixture.parent.join("protector-tamper-snapshot");
    let source_input = format!("{}\n", hex::encode(WECHAT_KEY));
    assert_success(&run(
        &[
            "snapshot",
            "create",
            fixture.source.to_str().unwrap(),
            snapshot.to_str().unwrap(),
            "--source-passphrase-stdin",
            "--snapshot-recovery-kit",
            kit.to_str().unwrap(),
            "--snapshot-local-credential",
            local.to_str().unwrap(),
        ],
        Some(source_input.as_bytes()),
    ));
    let manifest_path = snapshot.join("manifest.json");
    let original_bytes = fs::read(&manifest_path).unwrap();
    let original: Value = serde_json::from_slice(&original_bytes).unwrap();

    for field in ["protectorId", "salt", "nonce", "wrappedDatabaseKey"] {
        let mut changed = original.clone();
        let value = changed["protection"]["protectors"][0][field]
            .as_str()
            .unwrap();
        changed["protection"]["protectors"][0][field] =
            Value::String(change_first_ascii_character(value));
        write_private_manifest(&manifest_path, &changed);
        let verification = run(
            &[
                "snapshot",
                "verify",
                snapshot.to_str().unwrap(),
                "--snapshot-recovery-kit",
                kit.to_str().unwrap(),
            ],
            None,
        );
        assert!(
            !verification.status.success(),
            "tampered field {field} was accepted"
        );
    }

    let mut duplicate = original.clone();
    let duplicate_id = duplicate["protection"]["protectors"][0]["protectorId"].clone();
    duplicate["protection"]["protectors"][1]["protectorId"] = duplicate_id;
    write_private_manifest(&manifest_path, &duplicate);
    let duplicate_result = run(
        &[
            "snapshot",
            "verify",
            snapshot.to_str().unwrap(),
            "--snapshot-recovery-kit",
            kit.to_str().unwrap(),
        ],
        None,
    );
    assert!(!duplicate_result.status.success());

    let mut nonportable = original.clone();
    nonportable["protection"]["protectors"] =
        Value::Array(vec![original["protection"]["protectors"][1].clone()]);
    write_private_manifest(&manifest_path, &nonportable);
    let no_portable_result = run(
        &[
            "snapshot",
            "verify",
            snapshot.to_str().unwrap(),
            "--snapshot-local-credential",
            local.to_str().unwrap(),
        ],
        None,
    );
    assert!(!no_portable_result.status.success());

    fs::write(&manifest_path, original_bytes).unwrap();
    fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o600)).unwrap();
    let restored = run(
        &[
            "snapshot",
            "verify",
            snapshot.to_str().unwrap(),
            "--snapshot-recovery-kit",
            kit.to_str().unwrap(),
        ],
        None,
    );
    assert_success(&restored);

    let oversized = fixture.parent.join(".oversized-recovery-kit");
    fs::write(&oversized, vec![b'x'; 2 * 1024 + 1]).unwrap();
    fs::set_permissions(&oversized, fs::Permissions::from_mode(0o600)).unwrap();
    let oversized_result = run(
        &[
            "snapshot",
            "recovery-kit",
            "validate",
            oversized.to_str().unwrap(),
        ],
        None,
    );
    assert!(!oversized_result.status.success());
}

#[test]
fn protector_rewrap_keeps_encrypted_database_bytes_and_source_generation_unchanged() {
    let fixture = EncryptedFixture::new();
    let old_kit = fixture.parent.join(".old-recovery-words");
    let old_local = fixture.parent.join(".old-local-unlock");
    let new_kit = fixture.parent.join(".new-recovery-words");
    let new_local = fixture.parent.join(".new-local-unlock");
    for (kind, path) in [
        ("recovery-kit", &old_kit),
        ("local-credential", &old_local),
        ("recovery-kit", &new_kit),
        ("local-credential", &new_local),
    ] {
        assert_success(&run(
            &["snapshot", kind, "create", path.to_str().unwrap()],
            None,
        ));
    }

    let original = fixture.parent.join("original-wrapped-generation");
    let create_input = format!("{}\n", hex::encode(WECHAT_KEY));
    let created = run(
        &[
            "snapshot",
            "create",
            fixture.source.to_str().unwrap(),
            original.to_str().unwrap(),
            "--source-passphrase-stdin",
            "--snapshot-recovery-kit",
            old_kit.to_str().unwrap(),
            "--snapshot-local-credential",
            old_local.to_str().unwrap(),
        ],
        Some(create_input.as_bytes()),
    );
    assert_success(&created);
    let original_manifest_bytes = fs::read(original.join("manifest.json")).unwrap();
    let original_databases = relative_database_file_contents(&original);

    let rewrapped = fixture.parent.join("rewrapped-generation");
    let output = run(
        &[
            "snapshot",
            "rewrap",
            original.to_str().unwrap(),
            rewrapped.to_str().unwrap(),
            "--old-snapshot-local-credential",
            old_local.to_str().unwrap(),
            "--new-snapshot-recovery-kit",
            new_kit.to_str().unwrap(),
            "--new-snapshot-local-credential",
            new_local.to_str().unwrap(),
        ],
        None,
    );
    assert_success(&output);
    let original_manifest: Value = serde_json::from_slice(&original_manifest_bytes).unwrap();
    let new_manifest: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        new_manifest["parentSnapshotId"],
        original_manifest["snapshotId"]
    );
    assert_ne!(new_manifest["snapshotId"], original_manifest["snapshotId"]);
    assert_eq!(
        new_manifest["consistency"]["guarantee"],
        "encryptedDatabaseByteCopyRewrap"
    );
    assert_eq!(
        new_manifest["protection"]["protectors"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        fs::read(original.join("manifest.json")).unwrap(),
        original_manifest_bytes
    );
    assert_eq!(
        relative_database_file_contents(&original),
        original_databases
    );
    assert_eq!(
        relative_database_file_contents(&rewrapped),
        original_databases
    );

    fs::remove_dir_all(&fixture.source).unwrap();
    fs::remove_dir_all(&original).unwrap();
    fs::remove_file(&old_kit).unwrap();
    fs::remove_file(&old_local).unwrap();
    let portable = run(
        &[
            "snapshot",
            "verify",
            rewrapped.to_str().unwrap(),
            "--snapshot-recovery-kit",
            new_kit.to_str().unwrap(),
        ],
        None,
    );
    assert_success(&portable);
    let local = run(
        &[
            "messages",
            "list",
            rewrapped.to_str().unwrap(),
            "--snapshot-local-credential",
            new_local.to_str().unwrap(),
            "--conversation",
            "wxid_talker",
            "--limit",
            "1",
        ],
        None,
    );
    assert_success(&local);
}

#[test]
fn retention_quarantines_only_after_portable_replacement_proof_and_can_restore() {
    let fixture = EncryptedFixture::new();
    let kit = fixture.parent.join(".retention-recovery-words");
    let wrong_kit = fixture.parent.join(".retention-wrong-words");
    let local = fixture.parent.join(".retention-local-unlock");
    for (kind, path) in [
        ("recovery-kit", &kit),
        ("recovery-kit", &wrong_kit),
        ("local-credential", &local),
    ] {
        assert_success(&run(
            &["snapshot", kind, "create", path.to_str().unwrap()],
            None,
        ));
    }
    let original = fixture.parent.join("retention-original");
    let source_input = format!("{}\n", hex::encode(WECHAT_KEY));
    assert_success(&run(
        &[
            "snapshot",
            "create",
            fixture.source.to_str().unwrap(),
            original.to_str().unwrap(),
            "--source-passphrase-stdin",
            "--snapshot-recovery-kit",
            kit.to_str().unwrap(),
            "--snapshot-local-credential",
            local.to_str().unwrap(),
        ],
        Some(source_input.as_bytes()),
    ));
    let replacement = fixture.parent.join("retention-replacement");
    assert_success(&run(
        &[
            "snapshot",
            "rewrap",
            original.to_str().unwrap(),
            replacement.to_str().unwrap(),
            "--old-snapshot-local-credential",
            local.to_str().unwrap(),
            "--new-snapshot-recovery-kit",
            kit.to_str().unwrap(),
            "--new-snapshot-local-credential",
            local.to_str().unwrap(),
        ],
        None,
    ));
    let quarantine = fixture.parent.join("retired-snapshots");
    fs::create_dir(&quarantine).unwrap();
    fs::set_permissions(&quarantine, fs::Permissions::from_mode(0o700)).unwrap();

    let rejected = run(
        &[
            "snapshot",
            "retention",
            "quarantine",
            original.to_str().unwrap(),
            replacement.to_str().unwrap(),
            quarantine.to_str().unwrap(),
            "--retiring-local-credential",
            local.to_str().unwrap(),
            "--replacement-recovery-kit",
            wrong_kit.to_str().unwrap(),
        ],
        None,
    );
    assert!(!rejected.status.success());
    assert!(original.exists());
    assert_eq!(fs::read_dir(&quarantine).unwrap().count(), 0);

    let retired = run(
        &[
            "snapshot",
            "retention",
            "quarantine",
            original.to_str().unwrap(),
            replacement.to_str().unwrap(),
            quarantine.to_str().unwrap(),
            "--retiring-local-credential",
            local.to_str().unwrap(),
            "--replacement-recovery-kit",
            kit.to_str().unwrap(),
        ],
        None,
    );
    assert_success(&retired);
    let report: Value = serde_json::from_slice(&retired.stdout).unwrap();
    assert_eq!(report["schema"], "greenbubbles.snapshot-retention.v1");
    assert_eq!(report["operation"], "quarantine");
    assert_eq!(report["replacementPortableRecoveryVerified"], true);
    assert_eq!(report["wholeGeneration"], true);
    assert_eq!(report["recoverableMove"], true);
    assert!(!original.exists());
    assert!(replacement.exists());
    let entries = fs::read_dir(&quarantine)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    let quarantined = &entries[0];

    let restored = run(
        &[
            "snapshot",
            "retention",
            "restore",
            quarantined.to_str().unwrap(),
            original.to_str().unwrap(),
            "--snapshot-recovery-kit",
            kit.to_str().unwrap(),
        ],
        None,
    );
    assert_success(&restored);
    let report: Value = serde_json::from_slice(&restored.stdout).unwrap();
    assert_eq!(report["operation"], "restore");
    assert!(original.exists());
    assert_eq!(fs::read_dir(&quarantine).unwrap().count(), 0);
    assert_success(&run(
        &[
            "snapshot",
            "verify",
            original.to_str().unwrap(),
            "--snapshot-recovery-kit",
            kit.to_str().unwrap(),
        ],
        None,
    ));
}

#[test]
fn encrypted_wechat_source_becomes_independently_recoverable_snapshot() {
    let fixture = EncryptedFixture::new();
    let snapshot = fixture.parent.join("durable-snapshot");
    let create_input = format!(
        "{}\n{}\n",
        hex::encode(WECHAT_KEY),
        hex::encode(RECOVERY_KEY)
    );
    let created = run(
        &[
            "snapshot",
            "create",
            fixture.source.to_str().unwrap(),
            snapshot.to_str().unwrap(),
            "--source-passphrase-stdin",
            "--snapshot-key-stdin",
        ],
        Some(create_input.as_bytes()),
    );
    assert_success(&created);
    let manifest: Value = serde_json::from_slice(&created.stdout).unwrap();
    assert_eq!(manifest["schema"], "greenbubbles.recoverable-snapshot.v1");
    assert_eq!(manifest["protection"]["independentOfWechatKey"], true);
    assert_eq!(manifest["protection"]["plaintextDatabaseFiles"], false);
    assert_eq!(manifest["recoveryVerified"], true);
    assert_eq!(manifest["databases"].as_array().unwrap().len(), 5);

    let manifest_text = fs::read_to_string(snapshot.join("manifest.json")).unwrap();
    assert!(!manifest_text.contains(&hex::encode(WECHAT_KEY)));
    assert!(!manifest_text.contains(&hex::encode(RECOVERY_KEY)));
    assert_snapshot_has_only_encrypted_databases_without_sidecars(&snapshot);

    // Prove that subsequent verification and querying do not depend on the
    // source files or WeChat key material.
    fs::remove_dir_all(&fixture.source).unwrap();
    let recovery_input = format!("{}\n", hex::encode(RECOVERY_KEY));
    let verified = run(
        &[
            "snapshot",
            "verify",
            snapshot.to_str().unwrap(),
            "--snapshot-key-stdin",
        ],
        Some(recovery_input.as_bytes()),
    );
    assert_success(&verified);
    let report: Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(report["recoveryVerifiedWithoutWechatKey"], true);
    assert_eq!(report["independentOfWechatKey"], true);
    assert_eq!(report["encryptedAtRest"], true);
    assert_eq!(report["databaseCount"], 5);

    let status = run(
        &[
            "source",
            "status",
            snapshot.to_str().unwrap(),
            "--snapshot-key-stdin",
        ],
        Some(recovery_input.as_bytes()),
    );
    assert_success(&status);
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["operation"], "source.status");
    assert_eq!(status["source"]["mode"], "snapshotEncrypted");
    assert_eq!(status["databaseCount"], 5);
    assert_eq!(status["writeAheadLogBytes"], 0);
    assert_eq!(status["sharedMemoryBytes"], 0);

    let conversations = run(
        &[
            "conversations",
            "list",
            snapshot.to_str().unwrap(),
            "--snapshot-key-stdin",
            "--limit",
            "1",
        ],
        Some(recovery_input.as_bytes()),
    );
    assert_success(&conversations);
    let response: Value = serde_json::from_slice(&conversations.stdout).unwrap();
    assert_eq!(response["source"]["mode"], "snapshotEncrypted");
    assert_eq!(response["items"][0]["id"], "wxid_a");

    let messages = run(
        &[
            "messages",
            "list",
            snapshot.to_str().unwrap(),
            "--snapshot-key-stdin",
            "--conversation",
            "wxid_talker",
            "--limit",
            "2",
        ],
        Some(recovery_input.as_bytes()),
    );
    assert_success(&messages);
    let response: Value = serde_json::from_slice(&messages.stdout).unwrap();
    assert_eq!(response["items"][0]["content"]["Text"], "newer");
    assert_eq!(response["items"][1]["content"]["Text"], "older");
    let message_id = response["items"][0]["id"].as_str().unwrap();

    let exact = run(
        &[
            "message",
            "get",
            snapshot.to_str().unwrap(),
            "--snapshot-key-stdin",
            "--conversation",
            "wxid_talker",
            "--message",
            message_id,
        ],
        Some(recovery_input.as_bytes()),
    );
    assert_success(&exact);
    let response: Value = serde_json::from_slice(&exact.stdout).unwrap();
    assert_eq!(response["operation"], "message.get");
    assert_eq!(response["source"]["mode"], "snapshotEncrypted");
    assert_eq!(response["item"]["content"]["Text"], "newer");

    let search_input = format!("{}\nrecoverable\n", hex::encode(RECOVERY_KEY));
    let search = run(
        &[
            "messages",
            "search",
            snapshot.to_str().unwrap(),
            "--snapshot-key-stdin",
            "--query-stdin",
        ],
        Some(search_input.as_bytes()),
    );
    assert_success(&search);
    let response: Value = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(response["operation"], "messages.search");
    assert_eq!(response["items"][0]["snippet"], "recoverable hello");

    let obsolete_wechat_input = format!("{}\n", hex::encode(WECHAT_KEY));
    let wrong_protector = run(
        &[
            "snapshot",
            "verify",
            snapshot.to_str().unwrap(),
            "--snapshot-key-stdin",
        ],
        Some(obsolete_wechat_input.as_bytes()),
    );
    assert!(!wrong_protector.status.success());
    let stderr = String::from_utf8(wrong_protector.stderr).unwrap();
    assert!(!stderr.contains(snapshot.to_str().unwrap()));
    assert!(!stderr.contains(&hex::encode(WECHAT_KEY)));
    assert!(!stderr.contains(&hex::encode(RECOVERY_KEY)));
}

#[test]
fn snapshot_create_rejects_reusing_wechat_key_as_recovery_input() {
    let fixture = EncryptedFixture::new();
    let snapshot = fixture.parent.join("reused-key-snapshot");
    let input = format!("{}\n{}\n", hex::encode(WECHAT_KEY), hex::encode(WECHAT_KEY));
    let output = run(
        &[
            "snapshot",
            "create",
            fixture.source.to_str().unwrap(),
            snapshot.to_str().unwrap(),
            "--source-passphrase-stdin",
            "--snapshot-key-stdin",
        ],
        Some(input.as_bytes()),
    );
    assert!(!output.status.success());
    assert!(!snapshot.exists());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("must be distinct"));
}

#[test]
fn tampered_snapshot_database_fails_manifest_verification() {
    let fixture = EncryptedFixture::new();
    let snapshot = fixture.parent.join("tamper-snapshot");
    let create_input = format!(
        "{}\n{}\n",
        hex::encode(WECHAT_KEY),
        hex::encode(RECOVERY_KEY)
    );
    assert_success(&run(
        &[
            "snapshot",
            "create",
            fixture.source.to_str().unwrap(),
            snapshot.to_str().unwrap(),
            "--source-passphrase-stdin",
            "--snapshot-key-stdin",
        ],
        Some(create_input.as_bytes()),
    ));

    let database = snapshot.join("data/session/session.db");
    let mut bytes = fs::read(&database).unwrap();
    let index = bytes.len() / 2;
    bytes[index] ^= 0x01;
    fs::write(&database, bytes).unwrap();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();

    let recovery_input = format!("{}\n", hex::encode(RECOVERY_KEY));
    let output = run(
        &[
            "snapshot",
            "verify",
            snapshot.to_str().unwrap(),
            "--snapshot-key-stdin",
        ],
        Some(recovery_input.as_bytes()),
    );
    assert!(!output.status.success());
}

#[test]
fn snapshot_rekey_atomically_publishes_a_separately_recoverable_generation() {
    let fixture = EncryptedFixture::new();
    let original = fixture.parent.join("original-snapshot");
    let rotated = fixture.parent.join("rotated-snapshot");
    let create_input = format!(
        "{}\n{}\n",
        hex::encode(WECHAT_KEY),
        hex::encode(RECOVERY_KEY)
    );
    assert_success(&run(
        &[
            "snapshot",
            "create",
            fixture.source.to_str().unwrap(),
            original.to_str().unwrap(),
            "--source-passphrase-stdin",
            "--snapshot-key-stdin",
        ],
        Some(create_input.as_bytes()),
    ));
    let original_before = relative_files_with_sizes(&original);

    let rekey_input = format!(
        "{}\n{}\n",
        hex::encode(RECOVERY_KEY),
        hex::encode(ROTATED_RECOVERY_KEY)
    );
    let rekeyed = run(
        &[
            "snapshot",
            "rekey",
            original.to_str().unwrap(),
            rotated.to_str().unwrap(),
            "--old-snapshot-key-stdin",
            "--new-snapshot-key-stdin",
        ],
        Some(rekey_input.as_bytes()),
    );
    assert_success(&rekeyed);
    let manifest: Value = serde_json::from_slice(&rekeyed.stdout).unwrap();
    assert_eq!(manifest["sourceMode"], "recoverableSnapshot");
    assert_eq!(manifest["protection"]["independentOfWechatKey"], true);
    assert_eq!(relative_files_with_sizes(&original), original_before);
    assert_snapshot_has_only_encrypted_databases_without_sidecars(&rotated);

    let reused_key_input = format!(
        "{}\n{}\n",
        hex::encode(RECOVERY_KEY),
        hex::encode(RECOVERY_KEY)
    );
    let rejected_output = fixture.parent.join("rejected-rekey");
    let rejected = run(
        &[
            "snapshot",
            "rekey",
            original.to_str().unwrap(),
            rejected_output.to_str().unwrap(),
            "--old-snapshot-key-stdin",
            "--new-snapshot-key-stdin",
        ],
        Some(reused_key_input.as_bytes()),
    );
    assert!(!rejected.status.success());
    assert!(!rejected_output.exists());

    fs::remove_dir_all(&fixture.source).unwrap();
    fs::remove_dir_all(&original).unwrap();
    let new_key_input = format!("{}\n", hex::encode(ROTATED_RECOVERY_KEY));
    let verified = run(
        &[
            "snapshot",
            "verify",
            rotated.to_str().unwrap(),
            "--snapshot-key-stdin",
        ],
        Some(new_key_input.as_bytes()),
    );
    assert_success(&verified);
    let verified: Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(verified["recoveryVerifiedWithoutWechatKey"], true);

    let messages = run(
        &[
            "messages",
            "list",
            rotated.to_str().unwrap(),
            "--snapshot-key-stdin",
            "--conversation",
            "wxid_talker",
            "--limit",
            "1",
        ],
        Some(new_key_input.as_bytes()),
    );
    assert_success(&messages);
    let messages: Value = serde_json::from_slice(&messages.stdout).unwrap();
    assert_eq!(messages["items"][0]["content"]["Text"], "newer");

    let old_key_input = format!("{}\n", hex::encode(RECOVERY_KEY));
    let wrong_key = run(
        &[
            "snapshot",
            "verify",
            rotated.to_str().unwrap(),
            "--snapshot-key-stdin",
        ],
        Some(old_key_input.as_bytes()),
    );
    assert!(!wrong_key.status.success());
}

#[test]
fn stable_filesystem_capture_converts_without_plaintext_staging_or_live_source() {
    let fixture = EncryptedFixture::new();
    let capture = fixture.parent.join("stable-acquisition-capture");
    create_stable_capture(&fixture.source, &capture);
    let durable = fixture.parent.join("durable-from-capture");
    let input = format!(
        "{}\n{}\n",
        hex::encode(WECHAT_KEY),
        hex::encode(RECOVERY_KEY)
    );
    let created = run(
        &[
            "snapshot",
            "create-capture",
            capture.to_str().unwrap(),
            durable.to_str().unwrap(),
            "--source-passphrase-stdin",
            "--snapshot-key-stdin",
        ],
        Some(input.as_bytes()),
    );
    assert_success(&created);
    let manifest: Value = serde_json::from_slice(&created.stdout).unwrap();
    assert_eq!(manifest["sourceMode"], "stableAcquisitionSnapshot");
    assert_eq!(
        manifest["consistency"]["guarantee"],
        "stableAcquisitionSnapshotConversion"
    );
    assert_eq!(manifest["consistency"]["crossDatabaseAtomic"], false);
    assert_snapshot_has_only_encrypted_databases_without_sidecars(&durable);

    fs::remove_dir_all(&fixture.source).unwrap();
    fs::remove_dir_all(&capture).unwrap();
    let recovery_input = format!("{}\n", hex::encode(RECOVERY_KEY));
    let verified = run(
        &[
            "snapshot",
            "verify",
            durable.to_str().unwrap(),
            "--snapshot-key-stdin",
        ],
        Some(recovery_input.as_bytes()),
    );
    assert_success(&verified);
    let verified: Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(verified["recoveryVerifiedWithoutWechatKey"], true);

    let messages = run(
        &[
            "messages",
            "list",
            durable.to_str().unwrap(),
            "--snapshot-key-stdin",
            "--conversation",
            "wxid_talker",
            "--limit",
            "2",
        ],
        Some(recovery_input.as_bytes()),
    );
    assert_success(&messages);
    let messages: Value = serde_json::from_slice(&messages.stdout).unwrap();
    assert_eq!(messages["items"][0]["content"]["Text"], "newer");
    assert_eq!(messages["items"][1]["content"]["Text"], "older");
}

struct EncryptedFixture {
    _directory: tempfile::TempDir,
    parent: PathBuf,
    source: PathBuf,
}

impl EncryptedFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let parent = directory.path().to_path_buf();
        let source = parent.join("db_storage");
        fs::create_dir_all(source.join("contact")).unwrap();
        fs::create_dir_all(source.join("session")).unwrap();
        fs::create_dir_all(source.join("message")).unwrap();

        create_encrypted_database(
            &source.join("contact/contact.db"),
            "CREATE TABLE contact(username TEXT PRIMARY KEY);",
        );
        create_encrypted_database(
            &source.join("session/session.db"),
            "CREATE TABLE SessionTable(
                username TEXT NOT NULL,
                sort_timestamp INTEGER NOT NULL,
                summary BLOB,
                last_msg_type INTEGER,
                last_msg_sender TEXT,
                last_sender_display_name TEXT
             );
             INSERT INTO SessionTable VALUES ('wxid_a', 30, 'hello', 1, 'wxid_a', 'A');",
        );

        let table = format!("Msg_{:x}", md5::compute(b"wxid_talker"));
        for (shard, sort_sequence, body) in [(0, 10, "older"), (1, 20, "newer")] {
            let path = source.join(format!("message/message_{shard}.db"));
            let connection = open_encrypted_database(&path);
            connection
                .execute_batch(&format!(
                    "CREATE TABLE Name2Id(user_name TEXT);
                     INSERT INTO Name2Id(rowid, user_name) VALUES (1, 'wxid_sender');
                     CREATE TABLE [{table}](
                        server_id INTEGER,
                        sort_seq INTEGER,
                        local_type INTEGER,
                        real_sender_id INTEGER,
                        create_time INTEGER,
                        status INTEGER,
                        message_content BLOB,
                        packed_info_data BLOB,
                        WCDB_CT_message_content INTEGER,
                        compress_content BLOB
                     );"
                ))
                .unwrap();
            connection
                .execute(
                    &format!(
                        "INSERT INTO [{table}](server_id, sort_seq, local_type, real_sender_id, \
                         create_time, status, message_content, WCDB_CT_message_content) \
                         VALUES (?1, ?2, 1, 1, ?3, 0, ?4, 0)"
                    ),
                    params![shard + 1, sort_sequence, 1000 + shard, body.as_bytes()],
                )
                .unwrap();
        }

        let fts = open_encrypted_database(&source.join("message/message_fts.db"));
        wx_context::register_mm_fts_tokenizer(&fts).unwrap();
        fts.execute_batch(
            "CREATE TABLE name2id(rowid INTEGER PRIMARY KEY, username TEXT NOT NULL);
             INSERT INTO name2id VALUES (1, 'wxid_talker');
             INSERT INTO name2id VALUES (2, 'wxid_sender');
             CREATE VIRTUAL TABLE message_fts_v4_0 USING fts5(
                acontent, message_local_id UNINDEXED, sort_seq UNINDEXED,
                local_type UNINDEXED, session_id UNINDEXED, sender_id UNINDEXED,
                create_time UNINDEXED, tokenize='MMFtsTokenizer disable_pinyin'
             );
             INSERT INTO message_fts_v4_0 VALUES
                ('recoverable hello', 1, 20, 1, 1, 2, 1000);",
        )
        .unwrap();

        Self {
            _directory: directory,
            parent,
            source,
        }
    }
}

fn create_encrypted_database(path: &Path, sql: &str) {
    let connection = open_encrypted_database(path);
    connection.execute_batch(sql).unwrap();
}

fn open_encrypted_database(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    unsafe {
        let result = rusqlite::ffi::sqlite3_key(
            connection.handle(),
            WECHAT_KEY.as_ptr() as *const c_void,
            WECHAT_KEY.len() as i32,
        );
        assert_eq!(result, 0);
    }
    connection
}

fn create_stable_capture(source: &Path, capture: &Path) {
    fs::create_dir(capture).unwrap();
    fs::set_permissions(capture, fs::Permissions::from_mode(0o700)).unwrap();
    let sets = capture.join("sets");
    fs::create_dir(&sets).unwrap();
    fs::set_permissions(&sets, fs::Permissions::from_mode(0o700)).unwrap();
    let logical_paths = [
        "contact/contact.db",
        "message/message_0.db",
        "message/message_1.db",
        "message/message_fts.db",
        "session/session.db",
    ];
    let mut entries = Vec::new();
    for (index, logical_path) in logical_paths.iter().enumerate() {
        let set_directory = sets.join(format!("{index:04}"));
        fs::create_dir(&set_directory).unwrap();
        fs::set_permissions(&set_directory, fs::Permissions::from_mode(0o700)).unwrap();
        let destination = set_directory.join("database.db");
        fs::copy(source.join(logical_path), &destination).unwrap();
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o600)).unwrap();
        let metadata = fs::metadata(&destination).unwrap();
        let bytes = fs::read(&destination).unwrap();
        entries.push(serde_json::json!({
            "source": { "opaqueID": format!("{:064x}", index + 1) },
            "sourceSetID": format!("{:064x}", index + 101),
            "logicalPath": logical_path,
            "relativePath": format!("sets/{index:04}/database.db"),
            "role": "database",
            "fingerprint": {
                "deviceID": metadata.dev(),
                "fileID": metadata.ino(),
                "byteCount": metadata.len(),
                "modifiedSeconds": metadata.mtime(),
                "modifiedNanoseconds": metadata.mtime_nsec()
            },
            "sha256": hex::encode(Sha256::digest(&bytes))
        }));
    }
    let manifest = serde_json::json!({
        "manifestFormatVersion": 1,
        "snapshotID": "00000000-0000-4000-8000-000000000001",
        "createdAt": "2026-08-28T00:00:00Z",
        "sourceFingerprint": "ab".repeat(32),
        "entries": entries
    });
    let manifest_path = capture.join("manifest.json");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    fs::set_permissions(manifest_path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn assert_snapshot_has_only_encrypted_databases_without_sidecars(snapshot: &Path) {
    let mut database_count = 0usize;
    for entry in WalkDir::new(snapshot.join("data")) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        assert_eq!(
            path.extension().and_then(|value| value.to_str()),
            Some("db")
        );
        assert!(!path.to_string_lossy().ends_with("-wal"));
        assert!(!path.to_string_lossy().ends_with("-shm"));
        assert!(!path.to_string_lossy().ends_with("-journal"));
        let mut header = [0u8; 16];
        fs::File::open(path)
            .unwrap()
            .read_exact(&mut header)
            .unwrap();
        assert_ne!(&header, b"SQLite format 3\0");
        database_count += 1;
    }
    assert_eq!(database_count, 5);
}

fn relative_files_with_sizes(root: &Path) -> Vec<(String, u64)> {
    let mut files = WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            (
                entry
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                entry.metadata().unwrap().len(),
            )
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn relative_database_file_contents(root: &Path) -> Vec<(String, Vec<u8>)> {
    let data_root = root.join("data");
    let mut files = WalkDir::new(&data_root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            (
                entry
                    .path()
                    .strip_prefix(&data_root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

fn change_first_ascii_character(value: &str) -> String {
    let mut changed = value.to_string();
    let replacement = if changed.starts_with('A') { "B" } else { "A" };
    changed.replace_range(..1, replacement);
    changed
}

fn write_private_manifest(path: &Path, manifest: &Value) {
    let mut bytes = serde_json::to_vec_pretty(manifest).unwrap();
    bytes.push(b'\n');
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn run(arguments: &[&str], input: Option<&[u8]>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_greenbubbles-restore"));
    command
        .args(arguments)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
}
