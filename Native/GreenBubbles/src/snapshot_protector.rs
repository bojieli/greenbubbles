use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufWriter, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use bip39::{Language, Mnemonic};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::SnapshotKey;

pub const RECOVERY_KIT_SCHEMA: &str = "greenbubbles.recovery-kit.v1";
pub const RECOVERY_KIT_FORMAT_VERSION: u32 = 1;
pub const LOCAL_CREDENTIAL_SCHEMA: &str = "greenbubbles.local-unlock-credential.v1";
pub const LOCAL_CREDENTIAL_FORMAT_VERSION: u32 = 1;
pub const WRAPPED_KEY_FORMAT_VERSION: u32 = 1;

const RECOVERY_KIT_HEADER: &str = "GREENBUBBLES RECOVERY KIT";
const RECOVERY_KIT_LANGUAGE: &str = "english";
const LOCAL_CREDENTIAL_HEADER: &str = "GREENBUBBLES LOCAL UNLOCK CREDENTIAL";
const MAXIMUM_RECOVERY_KIT_BYTES: u64 = 2 * 1024;
const MAXIMUM_LOCAL_CREDENTIAL_BYTES: u64 = 1024;
const MAXIMUM_PASSPHRASE_BYTES: usize = 1024;
const MINIMUM_PASSPHRASE_BYTES: usize = 12;
const RECOVERY_ENTROPY_BYTES: usize = 32;
const LOCAL_CREDENTIAL_SECRET_BYTES: usize = 32;
const LOCAL_CREDENTIAL_ID_BYTES: usize = 16;
const DATABASE_KEY_BYTES: usize = 32;
const PROTECTOR_ID_BYTES: usize = 16;
const PROTECTOR_SALT_BYTES: usize = 32;
const XCHACHA_NONCE_BYTES: usize = 24;
const XCHACHA_TAG_BYTES: usize = 16;
const RECOVERY_WORDS_PROTECTOR_KIND: &str = "bip39English24";
const LOCAL_CREDENTIAL_PROTECTOR_KIND: &str = "localCredentialV1";
const PASSPHRASE_PROTECTOR_KIND: &str = "argon2idPassphraseV1";
const KEY_DERIVATION: &str = "hkdfSha256";
const PASSPHRASE_KEY_DERIVATION: &str = "argon2idV19-m65536-t3-p1";
const WRAPPING_CIPHER: &str = "xchacha20Poly1305";
const ARGON2_MEMORY_KIB: u32 = 65_536;
const ARGON2_TIME_COST: u32 = 3;
const ARGON2_PARALLELISM: u32 = 1;

#[derive(Debug, Error)]
pub enum SnapshotProtectorError {
    #[error("invalid snapshot recovery kit: {0}")]
    InvalidRecoveryKit(String),
    #[error("invalid local snapshot unlock credential: {0}")]
    InvalidLocalCredential(String),
    #[error("invalid snapshot passphrase: {0}")]
    InvalidPassphrase(String),
    #[error("unsafe snapshot protector path: {0}")]
    UnsafePath(String),
    #[error("snapshot key protector is invalid or could not be authenticated")]
    InvalidProtector,
    #[error("secure random generation failed")]
    RandomGeneration,
    #[error("snapshot recovery-kit I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SnapshotRecoveryWords {
    entropy: [u8; RECOVERY_ENTROPY_BYTES],
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SnapshotLocalCredential {
    credential_id: [u8; LOCAL_CREDENTIAL_ID_BYTES],
    secret: [u8; LOCAL_CREDENTIAL_SECRET_BYTES],
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SnapshotPassphrase {
    bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WrappedSnapshotKey {
    pub format_version: u32,
    pub protector_id: String,
    pub kind: String,
    pub key_derivation: String,
    pub salt: String,
    pub wrapping_cipher: String,
    pub nonce: String,
    pub wrapped_database_key: String,
    pub portable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryKitReport {
    pub schema: &'static str,
    pub format_version: u32,
    pub word_count: usize,
    pub checksum_validated: bool,
    pub portable: bool,
    pub file_created: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalCredentialReport {
    pub schema: &'static str,
    pub format_version: u32,
    pub local_convenience: bool,
    pub portable: bool,
    pub file_created: bool,
}

impl SnapshotRecoveryWords {
    pub fn generate() -> Result<Self, SnapshotProtectorError> {
        let mut entropy = [0u8; RECOVERY_ENTROPY_BYTES];
        getrandom::fill(&mut entropy).map_err(|_| SnapshotProtectorError::RandomGeneration)?;
        // Constructing the mnemonic here proves the generated entropy maps to
        // the required standard 24-word/checksummed representation.
        Mnemonic::from_entropy_in(Language::English, &entropy)
            .map_err(|_| invalid_recovery_kit("generated entropy was rejected"))?;
        Ok(Self { entropy })
    }

    pub fn parse(words: &str) -> Result<Self, SnapshotProtectorError> {
        let normalized = Zeroizing::new(words.split_whitespace().collect::<Vec<_>>().join(" "));
        if normalized.split(' ').count() != 24
            || normalized
                .bytes()
                .any(|value| !(value.is_ascii_lowercase() || value == b' '))
        {
            return Err(invalid_recovery_kit(
                "expected exactly 24 lowercase English BIP-39 words",
            ));
        }
        let mnemonic = Mnemonic::parse_in_normalized(Language::English, &normalized)
            .map_err(|_| invalid_recovery_kit("BIP-39 checksum validation failed"))?;
        let mut entropy = mnemonic.to_entropy();
        if entropy.len() != RECOVERY_ENTROPY_BYTES {
            entropy.zeroize();
            return Err(invalid_recovery_kit(
                "mnemonic does not encode 256 bits of recovery entropy",
            ));
        }
        let mut value = [0u8; RECOVERY_ENTROPY_BYTES];
        value.copy_from_slice(&entropy);
        entropy.zeroize();
        Ok(Self { entropy: value })
    }

    pub fn read_private_file(path: &Path) -> Result<Self, SnapshotProtectorError> {
        let path = validate_private_recovery_kit_file(path)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        if metadata.len() == 0 || metadata.len() > MAXIMUM_RECOVERY_KIT_BYTES {
            return Err(invalid_recovery_kit("file size is outside safe limits"));
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(metadata.len() as usize));
        file.take(MAXIMUM_RECOVERY_KIT_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAXIMUM_RECOVERY_KIT_BYTES {
            return Err(invalid_recovery_kit("file size is outside safe limits"));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| invalid_recovery_kit("file is not valid UTF-8"))?;
        parse_recovery_kit_text(text)
    }

    pub fn write_new_private_file(
        path: &Path,
    ) -> Result<RecoveryKitReport, SnapshotProtectorError> {
        let words = Self::generate()?;
        words.write_private_file(path)
    }

    pub fn write_private_file(
        &self,
        path: &Path,
    ) -> Result<RecoveryKitReport, SnapshotProtectorError> {
        let (parent, final_path) = validate_new_recovery_kit_path(path)?;
        let mnemonic = Mnemonic::from_entropy_in(Language::English, &self.entropy)
            .map_err(|_| invalid_recovery_kit("recovery entropy could not be encoded"))?;
        let word_text = Zeroizing::new(mnemonic.to_string());
        let content = Zeroizing::new(format!(
            "{RECOVERY_KIT_HEADER}\nformat: {RECOVERY_KIT_FORMAT_VERSION}\nlanguage: {RECOVERY_KIT_LANGUAGE}\nwords: {}\n",
            word_text.as_str()
        ));
        let result = (|| {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(&final_path)?;
            let mut writer = BufWriter::new(file);
            writer.write_all(content.as_bytes())?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
            fs::set_permissions(&final_path, fs::Permissions::from_mode(0o600))?;
            File::open(&parent)?.sync_all()?;
            let _ = validate_private_recovery_kit_file(&final_path)?;
            let reparsed = Self::read_private_file(&final_path)?;
            if reparsed.entropy != self.entropy {
                return Err(invalid_recovery_kit(
                    "recovery kit did not round-trip after durable creation",
                ));
            }
            Ok::<(), SnapshotProtectorError>(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&final_path);
            let _ = File::open(&parent).and_then(|directory| directory.sync_all());
            return Err(error);
        }
        Ok(recovery_kit_report(true))
    }

    pub fn validate_private_file(path: &Path) -> Result<RecoveryKitReport, SnapshotProtectorError> {
        let _ = Self::read_private_file(path)?;
        Ok(recovery_kit_report(false))
    }

    pub(crate) fn entropy(&self) -> &[u8; RECOVERY_ENTROPY_BYTES] {
        &self.entropy
    }
}

impl SnapshotLocalCredential {
    pub fn generate() -> Result<Self, SnapshotProtectorError> {
        let mut credential_id = [0u8; LOCAL_CREDENTIAL_ID_BYTES];
        let mut secret = [0u8; LOCAL_CREDENTIAL_SECRET_BYTES];
        getrandom::fill(&mut credential_id)
            .map_err(|_| SnapshotProtectorError::RandomGeneration)?;
        getrandom::fill(&mut secret).map_err(|_| SnapshotProtectorError::RandomGeneration)?;
        Ok(Self {
            credential_id,
            secret,
        })
    }

    pub fn read_private_file(path: &Path) -> Result<Self, SnapshotProtectorError> {
        let path = validate_private_local_credential_file(path)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        if metadata.len() == 0 || metadata.len() > MAXIMUM_LOCAL_CREDENTIAL_BYTES {
            return Err(invalid_local_credential("file size is outside safe limits"));
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(metadata.len() as usize));
        file.take(MAXIMUM_LOCAL_CREDENTIAL_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAXIMUM_LOCAL_CREDENTIAL_BYTES {
            return Err(invalid_local_credential("file size is outside safe limits"));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| invalid_local_credential("file is not valid UTF-8"))?;
        parse_local_credential_text(text)
    }

    pub fn write_new_private_file(
        path: &Path,
    ) -> Result<LocalCredentialReport, SnapshotProtectorError> {
        let credential = Self::generate()?;
        credential.write_private_file(path)
    }

    pub fn write_private_file(
        &self,
        path: &Path,
    ) -> Result<LocalCredentialReport, SnapshotProtectorError> {
        let (parent, final_path) = validate_new_local_credential_path(path)?;
        let credential_id = hex::encode(self.credential_id);
        let secret = Zeroizing::new(URL_SAFE_NO_PAD.encode(self.secret));
        let content = Zeroizing::new(format!(
            "{LOCAL_CREDENTIAL_HEADER}\nformat: {LOCAL_CREDENTIAL_FORMAT_VERSION}\ncredential-id: {credential_id}\nsecret: {}\n",
            secret.as_str()
        ));
        let result = (|| {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(&final_path)?;
            let mut writer = BufWriter::new(file);
            writer.write_all(content.as_bytes())?;
            writer.flush()?;
            writer.get_ref().sync_all()?;
            fs::set_permissions(&final_path, fs::Permissions::from_mode(0o600))?;
            File::open(&parent)?.sync_all()?;
            let _ = validate_private_local_credential_file(&final_path)?;
            let reparsed = Self::read_private_file(&final_path)?;
            if reparsed.credential_id != self.credential_id || reparsed.secret != self.secret {
                return Err(invalid_local_credential(
                    "credential did not round-trip after durable creation",
                ));
            }
            Ok::<(), SnapshotProtectorError>(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&final_path);
            let _ = File::open(&parent).and_then(|directory| directory.sync_all());
            return Err(error);
        }
        Ok(local_credential_report(true))
    }

    pub fn validate_private_file(
        path: &Path,
    ) -> Result<LocalCredentialReport, SnapshotProtectorError> {
        let _ = Self::read_private_file(path)?;
        Ok(local_credential_report(false))
    }

    fn credential_id_encoded(&self) -> String {
        hex::encode(self.credential_id)
    }
}

impl SnapshotPassphrase {
    pub fn from_utf8(bytes: Vec<u8>) -> Result<Self, SnapshotProtectorError> {
        validate_passphrase_bytes(&bytes)?;
        Ok(Self { bytes })
    }

    pub fn read_stdin() -> Result<Self, SnapshotProtectorError> {
        let mut bytes = Zeroizing::new(Vec::with_capacity(MAXIMUM_PASSPHRASE_BYTES + 2));
        io::stdin()
            .lock()
            .take((MAXIMUM_PASSPHRASE_BYTES + 2) as u64)
            .read_until(b'\n', &mut bytes)?;
        if bytes.ends_with(b"\n") {
            bytes.pop();
            if bytes.ends_with(b"\r") {
                bytes.pop();
            }
        }
        validate_passphrase_bytes(&bytes)?;
        Ok(Self {
            bytes: bytes.to_vec(),
        })
    }

    fn expose(&self) -> &[u8] {
        &self.bytes
    }
}

pub fn wrap_new_snapshot_database_key(
    snapshot_id: &str,
    recovery_words: &SnapshotRecoveryWords,
) -> Result<(SnapshotKey, WrappedSnapshotKey), SnapshotProtectorError> {
    validate_snapshot_identity(snapshot_id)?;
    let mut database_key = [0u8; DATABASE_KEY_BYTES];
    getrandom::fill(&mut database_key).map_err(|_| SnapshotProtectorError::RandomGeneration)?;
    let key = SnapshotKey::from_bytes(database_key);
    let protector =
        wrap_snapshot_database_key_with_recovery_words(snapshot_id, &key, recovery_words)?;
    Ok((key, protector))
}

pub fn wrap_snapshot_database_key_with_recovery_words(
    snapshot_id: &str,
    database_key: &SnapshotKey,
    recovery_words: &SnapshotRecoveryWords,
) -> Result<WrappedSnapshotKey, SnapshotProtectorError> {
    validate_snapshot_identity(snapshot_id)?;
    let mut protector_id = [0u8; PROTECTOR_ID_BYTES];
    let mut salt = [0u8; PROTECTOR_SALT_BYTES];
    let mut nonce = [0u8; XCHACHA_NONCE_BYTES];
    getrandom::fill(&mut protector_id).map_err(|_| SnapshotProtectorError::RandomGeneration)?;
    getrandom::fill(&mut salt).map_err(|_| SnapshotProtectorError::RandomGeneration)?;
    getrandom::fill(&mut nonce).map_err(|_| SnapshotProtectorError::RandomGeneration)?;

    let protector_id = hex::encode(protector_id);
    let salt_encoded = URL_SAFE_NO_PAD.encode(salt);
    let nonce_encoded = URL_SAFE_NO_PAD.encode(nonce);
    let aad =
        recovery_words_protector_aad(snapshot_id, &protector_id, &salt_encoded, &nonce_encoded);
    let wrapping_key =
        derive_recovery_words_wrapping_key(snapshot_id, &protector_id, recovery_words, &salt)?;
    let cipher = XChaCha20Poly1305::new_from_slice(wrapping_key.as_ref())
        .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    let xnonce = XNonce::from(nonce);
    let ciphertext = cipher
        .encrypt(
            &xnonce,
            Payload {
                msg: database_key.expose_for_snapshot_operation(),
                aad: &aad,
            },
        )
        .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    Ok(WrappedSnapshotKey {
        format_version: WRAPPED_KEY_FORMAT_VERSION,
        protector_id,
        kind: RECOVERY_WORDS_PROTECTOR_KIND.into(),
        key_derivation: KEY_DERIVATION.into(),
        salt: salt_encoded,
        wrapping_cipher: WRAPPING_CIPHER.into(),
        nonce: nonce_encoded,
        wrapped_database_key: URL_SAFE_NO_PAD.encode(ciphertext),
        portable: true,
        credential_id: None,
    })
}

pub fn wrap_snapshot_database_key_with_local_credential(
    snapshot_id: &str,
    database_key: &SnapshotKey,
    credential: &SnapshotLocalCredential,
) -> Result<WrappedSnapshotKey, SnapshotProtectorError> {
    validate_snapshot_identity(snapshot_id)?;
    let mut protector_id = [0u8; PROTECTOR_ID_BYTES];
    let mut salt = [0u8; PROTECTOR_SALT_BYTES];
    let mut nonce = [0u8; XCHACHA_NONCE_BYTES];
    getrandom::fill(&mut protector_id).map_err(|_| SnapshotProtectorError::RandomGeneration)?;
    getrandom::fill(&mut salt).map_err(|_| SnapshotProtectorError::RandomGeneration)?;
    getrandom::fill(&mut nonce).map_err(|_| SnapshotProtectorError::RandomGeneration)?;

    let protector_id = hex::encode(protector_id);
    let credential_id = credential.credential_id_encoded();
    let salt_encoded = URL_SAFE_NO_PAD.encode(salt);
    let nonce_encoded = URL_SAFE_NO_PAD.encode(nonce);
    let aad = local_credential_protector_aad(
        snapshot_id,
        &protector_id,
        &credential_id,
        &salt_encoded,
        &nonce_encoded,
    );
    let wrapping_key = derive_local_credential_wrapping_key(
        snapshot_id,
        &protector_id,
        &credential_id,
        credential,
        &salt,
    )?;
    let cipher = XChaCha20Poly1305::new_from_slice(wrapping_key.as_ref())
        .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    let ciphertext = cipher
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: database_key.expose_for_snapshot_operation(),
                aad: &aad,
            },
        )
        .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    Ok(WrappedSnapshotKey {
        format_version: WRAPPED_KEY_FORMAT_VERSION,
        protector_id,
        kind: LOCAL_CREDENTIAL_PROTECTOR_KIND.into(),
        key_derivation: KEY_DERIVATION.into(),
        salt: salt_encoded,
        wrapping_cipher: WRAPPING_CIPHER.into(),
        nonce: nonce_encoded,
        wrapped_database_key: URL_SAFE_NO_PAD.encode(ciphertext),
        portable: false,
        credential_id: Some(credential_id),
    })
}

pub fn wrap_snapshot_database_key_with_passphrase(
    snapshot_id: &str,
    database_key: &SnapshotKey,
    passphrase: &SnapshotPassphrase,
) -> Result<WrappedSnapshotKey, SnapshotProtectorError> {
    validate_snapshot_identity(snapshot_id)?;
    validate_passphrase_bytes(passphrase.expose())?;
    let mut protector_id = [0u8; PROTECTOR_ID_BYTES];
    let mut salt = [0u8; PROTECTOR_SALT_BYTES];
    let mut nonce = [0u8; XCHACHA_NONCE_BYTES];
    getrandom::fill(&mut protector_id).map_err(|_| SnapshotProtectorError::RandomGeneration)?;
    getrandom::fill(&mut salt).map_err(|_| SnapshotProtectorError::RandomGeneration)?;
    getrandom::fill(&mut nonce).map_err(|_| SnapshotProtectorError::RandomGeneration)?;

    let protector_id = hex::encode(protector_id);
    let salt_encoded = URL_SAFE_NO_PAD.encode(salt);
    let nonce_encoded = URL_SAFE_NO_PAD.encode(nonce);
    let aad = passphrase_protector_aad(snapshot_id, &protector_id, &salt_encoded, &nonce_encoded);
    let wrapping_key = derive_passphrase_wrapping_key(passphrase, &salt)?;
    let cipher = XChaCha20Poly1305::new_from_slice(wrapping_key.as_ref())
        .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    let ciphertext = cipher
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: database_key.expose_for_snapshot_operation(),
                aad: &aad,
            },
        )
        .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    Ok(WrappedSnapshotKey {
        format_version: WRAPPED_KEY_FORMAT_VERSION,
        protector_id,
        kind: PASSPHRASE_PROTECTOR_KIND.into(),
        key_derivation: PASSPHRASE_KEY_DERIVATION.into(),
        salt: salt_encoded,
        wrapping_cipher: WRAPPING_CIPHER.into(),
        nonce: nonce_encoded,
        wrapped_database_key: URL_SAFE_NO_PAD.encode(ciphertext),
        portable: true,
        credential_id: None,
    })
}

pub fn unwrap_snapshot_database_key(
    snapshot_id: &str,
    protector: &WrappedSnapshotKey,
    recovery_words: &SnapshotRecoveryWords,
) -> Result<SnapshotKey, SnapshotProtectorError> {
    validate_snapshot_identity(snapshot_id)?;
    validate_wrapped_snapshot_key(protector)?;
    if protector.kind != RECOVERY_WORDS_PROTECTOR_KIND
        || !protector.portable
        || protector.credential_id.is_some()
    {
        return Err(SnapshotProtectorError::InvalidProtector);
    }
    let salt = decode_exact::<PROTECTOR_SALT_BYTES>(&protector.salt)?;
    let nonce = decode_exact::<XCHACHA_NONCE_BYTES>(&protector.nonce)?;
    let ciphertext = URL_SAFE_NO_PAD
        .decode(&protector.wrapped_database_key)
        .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    if ciphertext.len() != DATABASE_KEY_BYTES + XCHACHA_TAG_BYTES {
        return Err(SnapshotProtectorError::InvalidProtector);
    }
    let wrapping_key = derive_recovery_words_wrapping_key(
        snapshot_id,
        &protector.protector_id,
        recovery_words,
        &salt,
    )?;
    let aad = recovery_words_protector_aad(
        snapshot_id,
        &protector.protector_id,
        &protector.salt,
        &protector.nonce,
    );
    let cipher = XChaCha20Poly1305::new_from_slice(wrapping_key.as_ref())
        .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    let xnonce = XNonce::from(nonce);
    let mut plaintext = Zeroizing::new(
        cipher
            .decrypt(
                &xnonce,
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| SnapshotProtectorError::InvalidProtector)?,
    );
    if plaintext.len() != DATABASE_KEY_BYTES {
        return Err(SnapshotProtectorError::InvalidProtector);
    }
    let mut key = [0u8; DATABASE_KEY_BYTES];
    key.copy_from_slice(&plaintext);
    plaintext.zeroize();
    Ok(SnapshotKey::from_bytes(key))
}

pub fn unwrap_snapshot_database_key_with_local_credential(
    snapshot_id: &str,
    protector: &WrappedSnapshotKey,
    credential: &SnapshotLocalCredential,
) -> Result<SnapshotKey, SnapshotProtectorError> {
    validate_snapshot_identity(snapshot_id)?;
    validate_wrapped_snapshot_key(protector)?;
    let credential_id = credential.credential_id_encoded();
    if protector.kind != LOCAL_CREDENTIAL_PROTECTOR_KIND
        || protector.portable
        || protector.credential_id.as_deref() != Some(credential_id.as_str())
    {
        return Err(SnapshotProtectorError::InvalidProtector);
    }
    let salt = decode_exact::<PROTECTOR_SALT_BYTES>(&protector.salt)?;
    let nonce = decode_exact::<XCHACHA_NONCE_BYTES>(&protector.nonce)?;
    let ciphertext = URL_SAFE_NO_PAD
        .decode(&protector.wrapped_database_key)
        .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    if ciphertext.len() != DATABASE_KEY_BYTES + XCHACHA_TAG_BYTES {
        return Err(SnapshotProtectorError::InvalidProtector);
    }
    let wrapping_key = derive_local_credential_wrapping_key(
        snapshot_id,
        &protector.protector_id,
        &credential_id,
        credential,
        &salt,
    )?;
    let aad = local_credential_protector_aad(
        snapshot_id,
        &protector.protector_id,
        &credential_id,
        &protector.salt,
        &protector.nonce,
    );
    let cipher = XChaCha20Poly1305::new_from_slice(wrapping_key.as_ref())
        .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| SnapshotProtectorError::InvalidProtector)?,
    );
    if plaintext.len() != DATABASE_KEY_BYTES {
        return Err(SnapshotProtectorError::InvalidProtector);
    }
    let mut key = [0u8; DATABASE_KEY_BYTES];
    key.copy_from_slice(&plaintext);
    Ok(SnapshotKey::from_bytes(key))
}

pub fn unwrap_snapshot_database_key_with_passphrase(
    snapshot_id: &str,
    protector: &WrappedSnapshotKey,
    passphrase: &SnapshotPassphrase,
) -> Result<SnapshotKey, SnapshotProtectorError> {
    validate_snapshot_identity(snapshot_id)?;
    validate_wrapped_snapshot_key(protector)?;
    validate_passphrase_bytes(passphrase.expose())?;
    if protector.kind != PASSPHRASE_PROTECTOR_KIND
        || !protector.portable
        || protector.credential_id.is_some()
    {
        return Err(SnapshotProtectorError::InvalidProtector);
    }
    let salt = decode_exact::<PROTECTOR_SALT_BYTES>(&protector.salt)?;
    let nonce = decode_exact::<XCHACHA_NONCE_BYTES>(&protector.nonce)?;
    let ciphertext = URL_SAFE_NO_PAD
        .decode(&protector.wrapped_database_key)
        .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    if ciphertext.len() != DATABASE_KEY_BYTES + XCHACHA_TAG_BYTES {
        return Err(SnapshotProtectorError::InvalidProtector);
    }
    let wrapping_key = derive_passphrase_wrapping_key(passphrase, &salt)?;
    let aad = passphrase_protector_aad(
        snapshot_id,
        &protector.protector_id,
        &protector.salt,
        &protector.nonce,
    );
    let plaintext = Zeroizing::new(
        XChaCha20Poly1305::new_from_slice(wrapping_key.as_ref())
            .map_err(|_| SnapshotProtectorError::InvalidProtector)?
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| SnapshotProtectorError::InvalidProtector)?,
    );
    if plaintext.len() != DATABASE_KEY_BYTES {
        return Err(SnapshotProtectorError::InvalidProtector);
    }
    let mut key = [0u8; DATABASE_KEY_BYTES];
    key.copy_from_slice(&plaintext);
    Ok(SnapshotKey::from_bytes(key))
}

pub fn validate_wrapped_snapshot_key(
    protector: &WrappedSnapshotKey,
) -> Result<(), SnapshotProtectorError> {
    if protector.format_version != WRAPPED_KEY_FORMAT_VERSION
        || protector.protector_id.len() != PROTECTOR_ID_BYTES * 2
        || !protector
            .protector_id
            .bytes()
            .all(|value| value.is_ascii_hexdigit())
        || protector.wrapping_cipher != WRAPPING_CIPHER
    {
        return Err(SnapshotProtectorError::InvalidProtector);
    }
    match protector.kind.as_str() {
        RECOVERY_WORDS_PROTECTOR_KIND
            if protector.portable
                && protector.credential_id.is_none()
                && protector.key_derivation == KEY_DERIVATION => {}
        LOCAL_CREDENTIAL_PROTECTOR_KIND
            if !protector.portable && protector.key_derivation == KEY_DERIVATION =>
        {
            let Some(credential_id) = protector.credential_id.as_deref() else {
                return Err(SnapshotProtectorError::InvalidProtector);
            };
            if credential_id.len() != LOCAL_CREDENTIAL_ID_BYTES * 2
                || !credential_id.bytes().all(|value| value.is_ascii_hexdigit())
            {
                return Err(SnapshotProtectorError::InvalidProtector);
            }
        }
        PASSPHRASE_PROTECTOR_KIND
            if protector.portable
                && protector.credential_id.is_none()
                && protector.key_derivation == PASSPHRASE_KEY_DERIVATION => {}
        _ => return Err(SnapshotProtectorError::InvalidProtector),
    }
    decode_exact::<PROTECTOR_SALT_BYTES>(&protector.salt)?;
    decode_exact::<XCHACHA_NONCE_BYTES>(&protector.nonce)?;
    let ciphertext = URL_SAFE_NO_PAD
        .decode(&protector.wrapped_database_key)
        .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    if ciphertext.len() != DATABASE_KEY_BYTES + XCHACHA_TAG_BYTES {
        return Err(SnapshotProtectorError::InvalidProtector);
    }
    Ok(())
}

fn parse_recovery_kit_text(text: &str) -> Result<SnapshotRecoveryWords, SnapshotProtectorError> {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() != 4
        || lines[0] != RECOVERY_KIT_HEADER
        || lines[1] != format!("format: {RECOVERY_KIT_FORMAT_VERSION}")
        || lines[2] != format!("language: {RECOVERY_KIT_LANGUAGE}")
    {
        return Err(invalid_recovery_kit("header or format is invalid"));
    }
    let words = lines[3]
        .strip_prefix("words: ")
        .ok_or_else(|| invalid_recovery_kit("word record is missing"))?;
    SnapshotRecoveryWords::parse(words)
}

fn parse_local_credential_text(
    text: &str,
) -> Result<SnapshotLocalCredential, SnapshotProtectorError> {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() != 4
        || lines[0] != LOCAL_CREDENTIAL_HEADER
        || lines[1] != format!("format: {LOCAL_CREDENTIAL_FORMAT_VERSION}")
    {
        return Err(invalid_local_credential("header or format is invalid"));
    }
    let credential_id = lines[2]
        .strip_prefix("credential-id: ")
        .ok_or_else(|| invalid_local_credential("credential identity is missing"))?;
    if credential_id.len() != LOCAL_CREDENTIAL_ID_BYTES * 2
        || !credential_id.bytes().all(|value| value.is_ascii_hexdigit())
    {
        return Err(invalid_local_credential("credential identity is malformed"));
    }
    let secret = lines[3]
        .strip_prefix("secret: ")
        .ok_or_else(|| invalid_local_credential("credential secret is missing"))?;
    let mut decoded_id = hex::decode(credential_id)
        .map_err(|_| invalid_local_credential("credential identity is malformed"))?;
    let mut decoded_secret = URL_SAFE_NO_PAD
        .decode(secret)
        .map_err(|_| invalid_local_credential("credential secret is malformed"))?;
    if decoded_id.len() != LOCAL_CREDENTIAL_ID_BYTES
        || decoded_secret.len() != LOCAL_CREDENTIAL_SECRET_BYTES
    {
        decoded_id.zeroize();
        decoded_secret.zeroize();
        return Err(invalid_local_credential(
            "credential fields have invalid lengths",
        ));
    }
    let mut value = SnapshotLocalCredential {
        credential_id: [0u8; LOCAL_CREDENTIAL_ID_BYTES],
        secret: [0u8; LOCAL_CREDENTIAL_SECRET_BYTES],
    };
    value.credential_id.copy_from_slice(&decoded_id);
    value.secret.copy_from_slice(&decoded_secret);
    decoded_id.zeroize();
    decoded_secret.zeroize();
    Ok(value)
}

fn recovery_kit_report(file_created: bool) -> RecoveryKitReport {
    RecoveryKitReport {
        schema: RECOVERY_KIT_SCHEMA,
        format_version: RECOVERY_KIT_FORMAT_VERSION,
        word_count: 24,
        checksum_validated: true,
        portable: true,
        file_created,
    }
}

fn local_credential_report(file_created: bool) -> LocalCredentialReport {
    LocalCredentialReport {
        schema: LOCAL_CREDENTIAL_SCHEMA,
        format_version: LOCAL_CREDENTIAL_FORMAT_VERSION,
        local_convenience: true,
        portable: false,
        file_created,
    }
}

fn validate_new_recovery_kit_path(
    path: &Path,
) -> Result<(PathBuf, PathBuf), SnapshotProtectorError> {
    if fs::symlink_metadata(path).is_ok() {
        return Err(SnapshotProtectorError::UnsafePath(
            "recovery-kit output already exists".into(),
        ));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = validate_owner_only_directory(parent, "recovery-kit output parent")?;
    let name = path.file_name().ok_or_else(|| {
        SnapshotProtectorError::UnsafePath("recovery-kit output has no filename".into())
    })?;
    Ok((parent.clone(), parent.join(name)))
}

fn validate_new_local_credential_path(
    path: &Path,
) -> Result<(PathBuf, PathBuf), SnapshotProtectorError> {
    if fs::symlink_metadata(path).is_ok() {
        return Err(SnapshotProtectorError::UnsafePath(
            "local-credential output already exists".into(),
        ));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = validate_owner_only_directory(parent, "local-credential output parent")?;
    let name = path.file_name().ok_or_else(|| {
        SnapshotProtectorError::UnsafePath("local-credential output has no filename".into())
    })?;
    Ok((parent.clone(), parent.join(name)))
}

fn validate_private_recovery_kit_file(path: &Path) -> Result<PathBuf, SnapshotProtectorError> {
    let path = resolve_private_protector_path(path, "recovery-kit parent")?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| {
        SnapshotProtectorError::UnsafePath("recovery-kit file is unavailable".into())
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
    {
        return Err(SnapshotProtectorError::UnsafePath(
            "recovery kit must be a current-user-owned owner-only single-link regular file".into(),
        ));
    }
    Ok(path)
}

fn validate_private_local_credential_file(path: &Path) -> Result<PathBuf, SnapshotProtectorError> {
    let path = resolve_private_protector_path(path, "local-credential parent")?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| {
        SnapshotProtectorError::UnsafePath("local-credential file is unavailable".into())
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
    {
        return Err(SnapshotProtectorError::UnsafePath(
            "local credential must be a current-user-owned owner-only single-link regular file"
                .into(),
        ));
    }
    Ok(path)
}

fn resolve_private_protector_path(
    path: &Path,
    parent_description: &str,
) -> Result<PathBuf, SnapshotProtectorError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let parent = validate_owner_only_directory(parent, parent_description)?;
    let name = path.file_name().ok_or_else(|| {
        SnapshotProtectorError::UnsafePath("protector path has no filename".into())
    })?;
    Ok(parent.join(name))
}

fn validate_owner_only_directory(
    path: &Path,
    description: &str,
) -> Result<PathBuf, SnapshotProtectorError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| SnapshotProtectorError::UnsafePath(format!("{description} is unavailable")))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(SnapshotProtectorError::UnsafePath(format!(
            "{description} must be a current-user-owned owner-only real directory"
        )));
    }
    Ok(path.canonicalize()?)
}

fn derive_recovery_words_wrapping_key(
    snapshot_id: &str,
    protector_id: &str,
    recovery_words: &SnapshotRecoveryWords,
    salt: &[u8; PROTECTOR_SALT_BYTES],
) -> Result<Zeroizing<[u8; 32]>, SnapshotProtectorError> {
    let mut key = Zeroizing::new([0u8; 32]);
    let mut info = Vec::new();
    append_aad_field(&mut info, b"greenbubbles-snapshot-wrapping-key-v1");
    append_aad_field(&mut info, snapshot_id.as_bytes());
    append_aad_field(&mut info, protector_id.as_bytes());
    Hkdf::<Sha256>::new(Some(salt), recovery_words.entropy())
        .expand(&info, key.as_mut())
        .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    Ok(key)
}

fn derive_local_credential_wrapping_key(
    snapshot_id: &str,
    protector_id: &str,
    credential_id: &str,
    credential: &SnapshotLocalCredential,
    salt: &[u8; PROTECTOR_SALT_BYTES],
) -> Result<Zeroizing<[u8; 32]>, SnapshotProtectorError> {
    let mut key = Zeroizing::new([0u8; 32]);
    let mut info = Vec::new();
    append_aad_field(&mut info, b"greenbubbles-snapshot-local-wrapping-key-v1");
    append_aad_field(&mut info, snapshot_id.as_bytes());
    append_aad_field(&mut info, protector_id.as_bytes());
    append_aad_field(&mut info, credential_id.as_bytes());
    Hkdf::<Sha256>::new(Some(salt), &credential.secret)
        .expand(&info, key.as_mut())
        .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    Ok(key)
}

fn derive_passphrase_wrapping_key(
    passphrase: &SnapshotPassphrase,
    salt: &[u8; PROTECTOR_SALT_BYTES],
) -> Result<Zeroizing<[u8; 32]>, SnapshotProtectorError> {
    validate_passphrase_bytes(passphrase.expose())?;
    let parameters = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_TIME_COST,
        ARGON2_PARALLELISM,
        Some(32),
    )
    .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, parameters);
    let mut key = Zeroizing::new([0u8; 32]);
    argon2
        .hash_password_into(passphrase.expose(), salt, key.as_mut())
        .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    Ok(key)
}

fn recovery_words_protector_aad(
    snapshot_id: &str,
    protector_id: &str,
    salt: &str,
    nonce: &str,
) -> Vec<u8> {
    let mut aad = Vec::new();
    append_aad_field(&mut aad, b"greenbubbles-snapshot-key-protector-v1");
    append_aad_field(&mut aad, snapshot_id.as_bytes());
    append_aad_field(&mut aad, protector_id.as_bytes());
    append_aad_field(&mut aad, RECOVERY_WORDS_PROTECTOR_KIND.as_bytes());
    append_aad_field(&mut aad, KEY_DERIVATION.as_bytes());
    append_aad_field(&mut aad, WRAPPING_CIPHER.as_bytes());
    append_aad_field(&mut aad, salt.as_bytes());
    append_aad_field(&mut aad, nonce.as_bytes());
    aad
}

fn local_credential_protector_aad(
    snapshot_id: &str,
    protector_id: &str,
    credential_id: &str,
    salt: &str,
    nonce: &str,
) -> Vec<u8> {
    let mut aad = Vec::new();
    append_aad_field(&mut aad, b"greenbubbles-snapshot-local-key-protector-v1");
    append_aad_field(&mut aad, snapshot_id.as_bytes());
    append_aad_field(&mut aad, protector_id.as_bytes());
    append_aad_field(&mut aad, LOCAL_CREDENTIAL_PROTECTOR_KIND.as_bytes());
    append_aad_field(&mut aad, KEY_DERIVATION.as_bytes());
    append_aad_field(&mut aad, WRAPPING_CIPHER.as_bytes());
    append_aad_field(&mut aad, credential_id.as_bytes());
    append_aad_field(&mut aad, salt.as_bytes());
    append_aad_field(&mut aad, nonce.as_bytes());
    append_aad_field(&mut aad, b"nonportable");
    aad
}

fn passphrase_protector_aad(
    snapshot_id: &str,
    protector_id: &str,
    salt: &str,
    nonce: &str,
) -> Vec<u8> {
    let mut aad = Vec::new();
    append_aad_field(
        &mut aad,
        b"greenbubbles-snapshot-passphrase-key-protector-v1",
    );
    append_aad_field(&mut aad, snapshot_id.as_bytes());
    append_aad_field(&mut aad, protector_id.as_bytes());
    append_aad_field(&mut aad, PASSPHRASE_PROTECTOR_KIND.as_bytes());
    append_aad_field(&mut aad, PASSPHRASE_KEY_DERIVATION.as_bytes());
    append_aad_field(&mut aad, WRAPPING_CIPHER.as_bytes());
    append_aad_field(&mut aad, salt.as_bytes());
    append_aad_field(&mut aad, nonce.as_bytes());
    append_aad_field(&mut aad, b"portable-secondary");
    aad
}

fn append_aad_field(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u32).to_be_bytes());
    output.extend_from_slice(value);
}

fn decode_exact<const SIZE: usize>(value: &str) -> Result<[u8; SIZE], SnapshotProtectorError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| SnapshotProtectorError::InvalidProtector)?;
    if decoded.len() != SIZE {
        return Err(SnapshotProtectorError::InvalidProtector);
    }
    let mut result = [0u8; SIZE];
    result.copy_from_slice(&decoded);
    Ok(result)
}

fn validate_snapshot_identity(snapshot_id: &str) -> Result<(), SnapshotProtectorError> {
    if snapshot_id.len() != 64 || !snapshot_id.bytes().all(|value| value.is_ascii_hexdigit()) {
        return Err(SnapshotProtectorError::InvalidProtector);
    }
    Ok(())
}

fn invalid_recovery_kit(reason: &str) -> SnapshotProtectorError {
    SnapshotProtectorError::InvalidRecoveryKit(reason.into())
}

fn invalid_local_credential(reason: &str) -> SnapshotProtectorError {
    SnapshotProtectorError::InvalidLocalCredential(reason.into())
}

fn validate_passphrase_bytes(bytes: &[u8]) -> Result<(), SnapshotProtectorError> {
    if !(MINIMUM_PASSPHRASE_BYTES..=MAXIMUM_PASSPHRASE_BYTES).contains(&bytes.len())
        || bytes.contains(&0)
        || std::str::from_utf8(bytes).is_err()
    {
        return Err(SnapshotProtectorError::InvalidPassphrase(
            "expected 12 to 1024 bytes of non-NUL UTF-8".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_words_wrap_one_random_database_key_and_bind_snapshot_identity() {
        let words = SnapshotRecoveryWords::generate().unwrap();
        let snapshot_a = "a".repeat(64);
        let snapshot_b = "b".repeat(64);
        let (key, protector) = wrap_new_snapshot_database_key(&snapshot_a, &words).unwrap();
        let unwrapped = unwrap_snapshot_database_key(&snapshot_a, &protector, &words).unwrap();
        assert_eq!(
            key.expose_for_snapshot_operation(),
            unwrapped.expose_for_snapshot_operation()
        );
        assert!(unwrap_snapshot_database_key(&snapshot_b, &protector, &words).is_err());
        let wrong_words = SnapshotRecoveryWords::generate().unwrap();
        assert!(unwrap_snapshot_database_key(&snapshot_a, &protector, &wrong_words).is_err());
    }

    #[test]
    fn argon2id_passphrase_is_a_secondary_authenticated_protector() {
        let words = SnapshotRecoveryWords::generate().unwrap();
        let snapshot = "c".repeat(64);
        let (key, recovery) = wrap_new_snapshot_database_key(&snapshot, &words).unwrap();
        let passphrase =
            SnapshotPassphrase::from_utf8(b"correct horse battery staple for snapshots".to_vec())
                .unwrap();
        let protector =
            wrap_snapshot_database_key_with_passphrase(&snapshot, &key, &passphrase).unwrap();
        assert_eq!(protector.kind, "argon2idPassphraseV1");
        assert_eq!(protector.key_derivation, "argon2idV19-m65536-t3-p1");
        assert!(protector.portable);
        assert_ne!(
            protector.wrapped_database_key,
            recovery.wrapped_database_key
        );
        let unwrapped =
            unwrap_snapshot_database_key_with_passphrase(&snapshot, &protector, &passphrase)
                .unwrap();
        assert_eq!(
            key.expose_for_snapshot_operation(),
            unwrapped.expose_for_snapshot_operation()
        );

        let wrong =
            SnapshotPassphrase::from_utf8(b"wrong horse battery staple for snapshots!!".to_vec())
                .unwrap();
        assert!(
            unwrap_snapshot_database_key_with_passphrase(&snapshot, &protector, &wrong).is_err()
        );
        assert!(SnapshotPassphrase::from_utf8(b"too short".to_vec()).is_err());
    }

    #[test]
    fn recovery_kit_is_private_checksummed_and_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join("kit.txt");
        let report = SnapshotRecoveryWords::write_new_private_file(&path).unwrap();
        assert_eq!(report.word_count, 24);
        assert!(report.checksum_validated);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        SnapshotRecoveryWords::read_private_file(&path).unwrap();
        assert!(SnapshotRecoveryWords::write_new_private_file(&path).is_err());
    }

    #[test]
    fn local_credential_wraps_the_same_key_without_becoming_portable() {
        let words = SnapshotRecoveryWords::generate().unwrap();
        let credential = SnapshotLocalCredential::generate().unwrap();
        let snapshot_id = "c".repeat(64);
        let (database_key, portable) =
            wrap_new_snapshot_database_key(&snapshot_id, &words).unwrap();
        let local = wrap_snapshot_database_key_with_local_credential(
            &snapshot_id,
            &database_key,
            &credential,
        )
        .unwrap();
        assert!(portable.portable);
        assert!(!local.portable);
        assert_eq!(local.kind, LOCAL_CREDENTIAL_PROTECTOR_KIND);
        assert!(local.credential_id.is_some());
        let unwrapped =
            unwrap_snapshot_database_key_with_local_credential(&snapshot_id, &local, &credential)
                .unwrap();
        assert_eq!(
            database_key.expose_for_snapshot_operation(),
            unwrapped.expose_for_snapshot_operation()
        );
        let wrong_credential = SnapshotLocalCredential::generate().unwrap();
        assert!(unwrap_snapshot_database_key_with_local_credential(
            &snapshot_id,
            &local,
            &wrong_credential
        )
        .is_err());
    }

    #[test]
    fn local_credential_file_is_private_durable_and_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join(".local-unlock");
        let report = SnapshotLocalCredential::write_new_private_file(&path).unwrap();
        assert!(report.local_convenience);
        assert!(!report.portable);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        SnapshotLocalCredential::read_private_file(&path).unwrap();
        assert!(SnapshotLocalCredential::write_new_private_file(&path).is_err());
    }

    #[test]
    fn recovery_protector_authenticates_every_mutable_envelope_field() {
        let words = SnapshotRecoveryWords::generate().unwrap();
        let snapshot_id = "d".repeat(64);
        let (_, protector) = wrap_new_snapshot_database_key(&snapshot_id, &words).unwrap();

        let mut mutations = Vec::new();
        let mut changed = protector.clone();
        let replacement = if changed.protector_id.starts_with('0') {
            "1"
        } else {
            "0"
        };
        changed.protector_id.replace_range(..1, replacement);
        mutations.push(changed);
        let mut changed = protector.clone();
        changed.salt = flip_encoded_byte(&changed.salt);
        mutations.push(changed);
        let mut changed = protector.clone();
        changed.nonce = flip_encoded_byte(&changed.nonce);
        mutations.push(changed);
        let mut changed = protector.clone();
        changed.wrapped_database_key = flip_encoded_byte(&changed.wrapped_database_key);
        mutations.push(changed);

        for changed in mutations {
            assert!(unwrap_snapshot_database_key(&snapshot_id, &changed, &words).is_err());
        }
    }

    fn flip_encoded_byte(value: &str) -> String {
        let mut decoded = URL_SAFE_NO_PAD.decode(value).unwrap();
        decoded[0] ^= 1;
        URL_SAFE_NO_PAD.encode(decoded)
    }
}
