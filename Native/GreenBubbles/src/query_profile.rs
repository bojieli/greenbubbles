use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::snapshot_protector::SnapshotPassphrase;

pub const QUERY_PROFILE_SCHEMA: &str = "greenbubbles.query-profiles.v1";
pub const QUERY_PROFILE_FORMAT_VERSION: u32 = 1;
pub const QUERY_PROFILE_ENVIRONMENT_VARIABLE: &str = "GREENBUBBLES_QUERY_PROFILES_FILE";

/// Implicit live source used when no profile file and no explicit source exist.
///
/// `greenbubbles-acquire capture` writes the account secret here. Ordinary
/// query commands read that file and the most recently written WeChat
/// `db_storage` directory, so a first-time user does not create a profile.
pub const DEFAULT_LIVE_PROFILE_NAME: &str = "live";
pub const DEFAULT_LIVE_CREDENTIAL_DIRECTORY: &str = ".greenbubbles-acquire";
pub const DEFAULT_LIVE_CREDENTIAL_FILE: &str = "passphrase.txt";

const DEFAULT_CONFIGURATION_DIRECTORY: &str = ".greenbubbles";
const DEFAULT_CONFIGURATION_FILE: &str = "query-profiles.json";
const DEFAULT_SETTINGS_FILE: &str = "config.toml";
pub const QUERY_SETTINGS_ENVIRONMENT_VARIABLE: &str = "GREENBUBBLES_CONFIG_FILE";
const MAXIMUM_CONFIGURATION_BYTES: u64 = 64 * 1024;
const MAXIMUM_PROFILE_COUNT: usize = 64;
const MAXIMUM_PROFILE_NAME_BYTES: usize = 64;
const MAXIMUM_KEY_FILE_BYTES: u64 = 66;
const MAXIMUM_PASSPHRASE_FILE_BYTES: u64 = 1_026;

#[derive(Debug, Error)]
pub enum QueryProfileError {
    #[error("query-profile path is unavailable: {0}")]
    Unavailable(String),
    #[error("unsafe query-profile path: {0}")]
    UnsafePath(String),
    #[error("invalid query-profile configuration: {0}")]
    InvalidConfiguration(String),
    #[error("invalid private query credential: {0}")]
    InvalidCredential(String),
    #[error("query-profile I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("query-profile JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct QueryProfileStore {
    pub schema: String,
    pub format_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,
    pub profiles: BTreeMap<String, QueryProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct QueryProfile {
    pub source_root: PathBuf,
    pub access: QueryProfileAccess,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct QueryCredentialFileAccess {
    pub credential_file: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueryDecryptedAccess {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", deny_unknown_fields)]
pub enum QueryProfileAccess {
    #[serde(rename = "liveWeChatKeyFile")]
    LiveWeChatKeyFile(QueryCredentialFileAccess),
    #[serde(rename = "snapshotLocalCredential")]
    SnapshotLocalCredential(QueryCredentialFileAccess),
    #[serde(rename = "snapshotRecoveryKit")]
    SnapshotRecoveryKit(QueryCredentialFileAccess),
    #[serde(rename = "snapshotPassphraseFile")]
    SnapshotPassphraseFile(QueryCredentialFileAccess),
    #[serde(rename = "snapshotRawKeyFile")]
    SnapshotRawKeyFile(QueryCredentialFileAccess),
    #[serde(rename = "decrypted")]
    Decrypted(QueryDecryptedAccess),
}

impl QueryProfileStore {
    pub fn load_default() -> Result<(PathBuf, Self), QueryProfileError> {
        let path = default_query_profile_path()?;
        let store = Self::load(&path)?;
        Ok((path, store))
    }

    pub fn load(path: &Path) -> Result<Self, QueryProfileError> {
        let bytes = read_private_file(
            path,
            MAXIMUM_CONFIGURATION_BYTES,
            "query-profile configuration",
        )?;
        let store: Self = serde_json::from_slice(&bytes)?;
        store.validate()?;
        Ok(store)
    }

    pub fn validate(&self) -> Result<(), QueryProfileError> {
        if self.schema != QUERY_PROFILE_SCHEMA {
            return Err(invalid_configuration("unsupported schema"));
        }
        if self.format_version != QUERY_PROFILE_FORMAT_VERSION {
            return Err(invalid_configuration("unsupported format version"));
        }
        if self.profiles.is_empty() || self.profiles.len() > MAXIMUM_PROFILE_COUNT {
            return Err(invalid_configuration(
                "profile count must be between 1 and 64",
            ));
        }
        for (name, profile) in &self.profiles {
            validate_profile_name(name)?;
            validate_absolute_non_root_path(&profile.source_root, "sourceRoot")?;
            if let Some(path) = profile.access.credential_file() {
                validate_absolute_non_root_path(path, "credentialFile")?;
            }
        }
        if let Some(default_profile) = &self.default_profile {
            validate_profile_name(default_profile)?;
            if !self.profiles.contains_key(default_profile) {
                return Err(invalid_configuration(
                    "defaultProfile does not name a configured profile",
                ));
            }
        }
        Ok(())
    }

    pub fn select(
        &self,
        requested: Option<&str>,
    ) -> Result<(String, &QueryProfile), QueryProfileError> {
        let name = match requested {
            Some(name) => name,
            None => self.default_profile.as_deref().ok_or_else(|| {
                invalid_configuration("no defaultProfile is configured; select one with --profile")
            })?,
        };
        validate_profile_name(name)?;
        self.profiles
            .get(name)
            .map(|profile| (name.to_string(), profile))
            .ok_or_else(|| invalid_configuration("selected profile does not exist"))
    }

    pub fn set_default(&mut self, name: &str) -> Result<(), QueryProfileError> {
        validate_profile_name(name)?;
        if !self.profiles.contains_key(name) {
            return Err(invalid_configuration("selected profile does not exist"));
        }
        self.default_profile = Some(name.to_string());
        self.validate()
    }

    pub fn replace_private_file(&self, path: &Path) -> Result<(), QueryProfileError> {
        self.validate()?;
        let final_path = resolve_private_file_path(path, "query-profile configuration")?;
        validate_private_file_metadata(
            &final_path,
            MAXIMUM_CONFIGURATION_BYTES,
            "query-profile configuration",
        )?;

        let mut bytes = Zeroizing::new(serde_json::to_vec_pretty(self)?);
        bytes.push(b'\n');
        if bytes.len() as u64 > MAXIMUM_CONFIGURATION_BYTES {
            return Err(invalid_configuration(
                "serialized configuration exceeds the fixed size limit",
            ));
        }
        let parent = final_path.parent().ok_or_else(|| {
            QueryProfileError::UnsafePath("configuration path has no parent".into())
        })?;
        let file_name = final_path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                QueryProfileError::UnsafePath("configuration filename is invalid".into())
            })?;

        let mut temporary_path = None;
        let mut temporary_file = None;
        for _ in 0..8 {
            let mut nonce = [0_u8; 16];
            getrandom::fill(&mut nonce).map_err(|_| {
                QueryProfileError::Unavailable("secure random generation failed".into())
            })?;
            let candidate = parent.join(format!(".{file_name}.{}.tmp", hex::encode(nonce)));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(&candidate)
            {
                Ok(file) => {
                    temporary_path = Some(candidate);
                    temporary_file = Some(file);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        let temporary_path = temporary_path.ok_or_else(|| {
            QueryProfileError::Unavailable("could not allocate a private temporary file".into())
        })?;
        let result = (|| {
            let mut file = temporary_file.expect("temporary file accompanies its path");
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::set_permissions(&temporary_path, fs::Permissions::from_mode(0o600))?;
            fs::rename(&temporary_path, &final_path)?;
            File::open(parent)?.sync_all()?;
            let round_trip = Self::load(&final_path)?;
            if &round_trip != self {
                return Err(invalid_configuration(
                    "configuration did not round-trip after replacement",
                ));
            }
            Ok::<(), QueryProfileError>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary_path);
            let _ = File::open(parent).and_then(|directory| directory.sync_all());
        }
        result
    }
}

impl QueryProfileAccess {
    pub const fn mode_name(&self) -> &'static str {
        match self {
            Self::LiveWeChatKeyFile(_) => "liveWeChatKeyFile",
            Self::SnapshotLocalCredential(_) => "snapshotLocalCredential",
            Self::SnapshotRecoveryKit(_) => "snapshotRecoveryKit",
            Self::SnapshotPassphraseFile(_) => "snapshotPassphraseFile",
            Self::SnapshotRawKeyFile(_) => "snapshotRawKeyFile",
            Self::Decrypted(_) => "decrypted",
        }
    }

    pub fn credential_file(&self) -> Option<&Path> {
        match self {
            Self::LiveWeChatKeyFile(access)
            | Self::SnapshotLocalCredential(access)
            | Self::SnapshotRecoveryKit(access)
            | Self::SnapshotPassphraseFile(access)
            | Self::SnapshotRawKeyFile(access) => Some(access.credential_file.as_path()),
            Self::Decrypted(_) => None,
        }
    }
}

pub fn default_live_credential_path() -> Result<PathBuf, QueryProfileError> {
    let home = current_user_home()?;
    Ok(home
        .join(DEFAULT_LIVE_CREDENTIAL_DIRECTORY)
        .join(DEFAULT_LIVE_CREDENTIAL_FILE))
}

/// The one WeChat database the signed-in account is using now.
///
/// WeChat can leave several complete `db_storage` directories behind after an
/// account change. Only one of them is live. A directory qualifies when it is
/// a real, current-user-owned directory containing `contact`, `session`, and
/// `message`. The live one is the directory whose database files were written
/// most recently, not the directory whose own timestamp is newest. A leftover
/// account whose files are more than 14 days older is ignored. Two accounts
/// written within that window are ambiguous, and the command asks for an
/// explicit `source.root` instead of guessing.
pub fn discover_default_live_source_root() -> Result<PathBuf, QueryProfileError> {
    let home = current_user_home()?;
    let mut candidates = Vec::new();
    candidates.push(
        home.join("Library/Containers/com.tencent.xinWeChat/Data/Documents/xwechat_files"),
    );
    let groups = home.join("Library/Group Containers");
    if let Ok(entries) = fs::read_dir(&groups) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy().to_ascii_lowercase();
            if name.contains("wechat") || name.contains("xinwechat") {
                candidates.push(entry.path().join("xwechat_files"));
            }
        }
    }

    let mut found = Vec::new();
    for files_root in candidates {
        let Ok(accounts) = fs::read_dir(&files_root) else {
            continue;
        };
        for account in accounts.flatten() {
            let database_root = account.path().join("db_storage");
            if !is_usable_live_database_root(&database_root) {
                continue;
            }
            let Some(modified) = database_root_recency(&database_root) else {
                continue;
            };
            found.push((modified, database_root));
        }
    }
    found.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    let Some((newest, path)) = found.first() else {
        return Err(QueryProfileError::Unavailable(
            "no live WeChat database was found".into(),
        ));
    };
    if let Some((older, _)) = found.get(1) {
        const AMBIGUOUS_WINDOW_SECONDS: i64 = 14 * 24 * 60 * 60;
        if newest.saturating_sub(*older) < AMBIGUOUS_WINDOW_SECONDS {
            return Err(QueryProfileError::Unavailable(
                "more than one WeChat account was written recently; set source.root in ~/.greenbubbles/config.toml to the account you are signed in to".into(),
            ));
        }
    }
    Ok(path.clone())
}

fn current_user_home() -> Result<PathBuf, QueryProfileError> {
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| QueryProfileError::Unavailable("HOME is not set".into()))?;
    if !home.is_absolute() {
        return Err(QueryProfileError::UnsafePath(
            "HOME must be an absolute path".into(),
        ));
    }
    Ok(home)
}

fn is_usable_live_database_root(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return false;
    }
    ["contact", "session", "message"]
        .iter()
        .all(|name| path.join(name).is_dir())
}

fn database_root_recency(path: &Path) -> Option<i64> {
    // Directory timestamps stay behind after an account switch. The files
    // WeChat is writing — databases, journals, and shared-memory files — are
    // the evidence that this account is the one in use.
    let mut newest = None;
    for component in ["contact", "session", "message"] {
        let directory = path.join(component);
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let modified = metadata.mtime();
            newest = Some(newest.map_or(modified, |current: i64| current.max(modified)));
        }
    }
    newest
}

/// Optional everyday settings, in the same style as a coding agent's config.
///
/// The file stores paths only. It never stores a key, passphrase, or recovery
/// words. A missing file means "use the installed WeChat database and the
/// passphrase file written by capture."
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuerySettings {
    pub source_root: Option<PathBuf>,
    pub passphrase_file: Option<PathBuf>,
    pub default_profile: Option<String>,
}

pub fn default_query_settings_path() -> Result<PathBuf, QueryProfileError> {
    if let Some(path) = env::var_os(QUERY_SETTINGS_ENVIRONMENT_VARIABLE) {
        let path = PathBuf::from(path);
        validate_absolute_non_root_path(&path, QUERY_SETTINGS_ENVIRONMENT_VARIABLE)?;
        return Ok(path);
    }
    Ok(current_user_home()?
        .join(DEFAULT_CONFIGURATION_DIRECTORY)
        .join(DEFAULT_SETTINGS_FILE))
}

pub fn profile_uses_placeholder_source(profile: &QueryProfile) -> bool {
    profile
        .source_root
        .components()
        .any(|component| component.as_os_str() == "ABSOLUTE")
}

pub fn load_query_settings() -> Result<QuerySettings, QueryProfileError> {
    let path = default_query_settings_path()?;
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(QuerySettings::default()),
        Err(_) => Err(QueryProfileError::UnsafePath(
            "settings file is unavailable".into(),
        )),
        Ok(_) => parse_query_settings(&read_private_file(
            &path,
            MAXIMUM_CONFIGURATION_BYTES,
            "query settings",
        )?),
    }
}

fn parse_query_settings(bytes: &[u8]) -> Result<QuerySettings, QueryProfileError> {
    let value: toml::Value = toml::from_slice(bytes)
        .map_err(|_| invalid_configuration("settings file is not valid TOML"))?;
    let table = value
        .as_table()
        .ok_or_else(|| invalid_configuration("settings file must be a TOML table"))?;
    if table.keys().any(|key| key != "source" && key != "profile") {
        return Err(invalid_configuration(
            "settings file only accepts [source] and [profile]",
        ));
    }
    let mut settings = QuerySettings::default();
    if let Some(source) = table.get("source") {
        let source = source
            .as_table()
            .ok_or_else(|| invalid_configuration("[source] must be a table"))?;
        for key in source.keys() {
            if key != "root" && key != "passphrase_file" {
                return Err(invalid_configuration(
                    "[source] only accepts root and passphrase_file",
                ));
            }
        }
        if let Some(root) = source.get("root") {
            let root = required_settings_string(root, "source.root")?;
            let path = PathBuf::from(root);
            validate_absolute_non_root_path(&path, "source.root")?;
            settings.source_root = Some(path);
        }
        if let Some(passphrase_file) = source.get("passphrase_file") {
            let passphrase_file = required_settings_string(passphrase_file, "source.passphrase_file")?;
            let path = PathBuf::from(passphrase_file);
            validate_absolute_non_root_path(&path, "source.passphrase_file")?;
            settings.passphrase_file = Some(path);
        }
    }
    if let Some(profile) = table.get("profile") {
        let profile = profile
            .as_table()
            .ok_or_else(|| invalid_configuration("[profile] must be a table"))?;
        if profile.keys().any(|key| key != "default") {
            return Err(invalid_configuration("[profile] only accepts default"));
        }
        if let Some(name) = profile.get("default") {
            let name = required_settings_string(name, "profile.default")?;
            validate_profile_name(name)?;
            settings.default_profile = Some(name.to_string());
        }
    }
    Ok(settings)
}

fn required_settings_string<'a>(
    value: &'a toml::Value,
    field: &str,
) -> Result<&'a str, QueryProfileError> {
    value.as_str().filter(|text| !text.is_empty()).ok_or_else(|| {
        invalid_configuration(&format!("{field} must be a non-empty string"))
    })
}

pub fn default_query_profile_path() -> Result<PathBuf, QueryProfileError> {
    if let Some(path) = env::var_os(QUERY_PROFILE_ENVIRONMENT_VARIABLE) {
        let path = PathBuf::from(path);
        validate_absolute_non_root_path(&path, QUERY_PROFILE_ENVIRONMENT_VARIABLE)?;
        return Ok(path);
    }
    Ok(current_user_home()?
        .join(DEFAULT_CONFIGURATION_DIRECTORY)
        .join(DEFAULT_CONFIGURATION_FILE))
}

pub fn read_private_32_byte_credential(
    path: &Path,
) -> Result<Zeroizing<[u8; 32]>, QueryProfileError> {
    let mut bytes = read_private_file(path, MAXIMUM_KEY_FILE_BYTES, "query credential")?;
    remove_one_line_ending(&mut bytes);
    if bytes.contains(&b'\n') || bytes.contains(&b'\r') {
        return Err(invalid_credential(
            "key file must contain exactly one bounded value",
        ));
    }
    let mut value = Zeroizing::new([0_u8; 32]);
    if bytes.len() == 64 && bytes.iter().all(u8::is_ascii_hexdigit) {
        hex::decode_to_slice(bytes.as_slice(), value.as_mut())
            .map_err(|_| invalid_credential("key file contains invalid hexadecimal"))?;
    } else if bytes.len() == 32 {
        value.copy_from_slice(&bytes);
    } else {
        return Err(invalid_credential(
            "key file must contain 64 hexadecimal characters or exactly 32 raw bytes",
        ));
    }
    Ok(value)
}

pub fn read_private_snapshot_passphrase(
    path: &Path,
) -> Result<SnapshotPassphrase, QueryProfileError> {
    let mut bytes = read_private_file(
        path,
        MAXIMUM_PASSPHRASE_FILE_BYTES,
        "snapshot passphrase credential",
    )?;
    remove_one_line_ending(&mut bytes);
    if bytes.contains(&b'\n') || bytes.contains(&b'\r') {
        return Err(invalid_credential(
            "snapshot passphrase file must contain exactly one UTF-8 line",
        ));
    }
    SnapshotPassphrase::from_utf8(bytes.to_vec())
        .map_err(|_| invalid_credential("snapshot passphrase is outside accepted limits"))
}

fn read_private_file(
    path: &Path,
    maximum_bytes: u64,
    description: &str,
) -> Result<Zeroizing<Vec<u8>>, QueryProfileError> {
    let path = resolve_private_file_path(path, description)?;
    validate_private_file_metadata(&path, maximum_bytes, description)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)?;
    let metadata = file.metadata()?;
    validate_open_file_metadata(&metadata, maximum_bytes, description)?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(metadata.len() as usize));
    file.take(maximum_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > maximum_bytes {
        return Err(QueryProfileError::UnsafePath(format!(
            "{description} size is outside safe limits"
        )));
    }
    Ok(bytes)
}

fn resolve_private_file_path(path: &Path, description: &str) -> Result<PathBuf, QueryProfileError> {
    if !path.is_absolute() {
        return Err(QueryProfileError::UnsafePath(format!(
            "{description} path must be absolute"
        )));
    }
    let parent = path.parent().ok_or_else(|| {
        QueryProfileError::UnsafePath(format!("{description} path has no parent"))
    })?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| {
        QueryProfileError::UnsafePath(format!("{description} parent is unavailable"))
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(QueryProfileError::UnsafePath(format!(
            "{description} parent must be a current-user-owned owner-only real directory"
        )));
    }
    let parent = parent.canonicalize()?;
    let file_name = path.file_name().ok_or_else(|| {
        QueryProfileError::UnsafePath(format!("{description} path has no filename"))
    })?;
    Ok(parent.join(file_name))
}

fn validate_private_file_metadata(
    path: &Path,
    maximum_bytes: u64,
    description: &str,
) -> Result<(), QueryProfileError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| QueryProfileError::UnsafePath(format!("{description} file is unavailable")))?;
    if metadata.file_type().is_symlink() {
        return Err(QueryProfileError::UnsafePath(format!(
            "{description} must not be a symbolic link"
        )));
    }
    validate_open_file_metadata(&metadata, maximum_bytes, description)
}

fn validate_open_file_metadata(
    metadata: &fs::Metadata,
    maximum_bytes: u64,
    description: &str,
) -> Result<(), QueryProfileError> {
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
    {
        return Err(QueryProfileError::UnsafePath(format!(
            "{description} must be a current-user-owned owner-only single-link regular file"
        )));
    }
    if metadata.len() == 0 || metadata.len() > maximum_bytes {
        return Err(QueryProfileError::UnsafePath(format!(
            "{description} size is outside safe limits"
        )));
    }
    Ok(())
}

fn validate_profile_name(name: &str) -> Result<(), QueryProfileError> {
    if name.is_empty()
        || name.len() > MAXIMUM_PROFILE_NAME_BYTES
        || matches!(name, "." | "..")
        || !name
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'.' | b'_' | b'-'))
    {
        return Err(invalid_configuration(
            "profile names use 1..64 ASCII letters, digits, '.', '_', or '-'",
        ));
    }
    Ok(())
}

fn validate_absolute_non_root_path(path: &Path, field: &str) -> Result<(), QueryProfileError> {
    if !path.is_absolute() || path.parent().is_none() {
        return Err(invalid_configuration(&format!(
            "{field} must be an absolute non-root path"
        )));
    }
    Ok(())
}

fn remove_one_line_ending(bytes: &mut Vec<u8>) {
    if bytes.ends_with(b"\n") {
        bytes.pop();
        if bytes.ends_with(b"\r") {
            bytes.pop();
        }
    }
}

fn invalid_configuration(reason: &str) -> QueryProfileError {
    QueryProfileError::InvalidConfiguration(reason.into())
}

fn invalid_credential(reason: &str) -> QueryProfileError {
    QueryProfileError::InvalidCredential(reason.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_schema_rejects_unknown_fields_and_unsafe_names() {
        let unknown = serde_json::json!({
            "schema": QUERY_PROFILE_SCHEMA,
            "formatVersion": 1,
            "defaultProfile": "live",
            "profiles": {
                "live": {
                    "sourceRoot": "/private/source",
                    "access": {"mode": "decrypted"},
                    "secret": "must-not-be-accepted"
                }
            }
        });
        assert!(serde_json::from_value::<QueryProfileStore>(unknown).is_err());

        let secret_in_access = serde_json::json!({
            "schema": QUERY_PROFILE_SCHEMA,
            "formatVersion": 1,
            "defaultProfile": "live",
            "profiles": {
                "live": {
                    "sourceRoot": "/private/source",
                    "access": {
                        "mode": "decrypted",
                        "passphrase": "must-not-be-accepted"
                    }
                }
            }
        });
        assert!(serde_json::from_value::<QueryProfileStore>(secret_in_access).is_err());

        let store = QueryProfileStore {
            schema: QUERY_PROFILE_SCHEMA.into(),
            format_version: QUERY_PROFILE_FORMAT_VERSION,
            default_profile: Some("../live".into()),
            profiles: BTreeMap::from([(
                "../live".into(),
                QueryProfile {
                    source_root: PathBuf::from("/private/source"),
                    access: QueryProfileAccess::Decrypted(QueryDecryptedAccess::default()),
                },
            )]),
        };
        assert!(store.validate().is_err());
    }

    #[test]
    fn private_key_reader_rejects_permissions_and_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let key = directory.path().join("key");
        fs::write(&key, format!("{}\n", hex::encode([0xAB_u8; 32]))).unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(*read_private_32_byte_credential(&key).unwrap(), [0xAB; 32]);

        fs::set_permissions(&key, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(read_private_32_byte_credential(&key).is_err());
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();

        let link = directory.path().join("key-link");
        std::os::unix::fs::symlink(&key, &link).unwrap();
        assert!(read_private_32_byte_credential(&link).is_err());
    }

    #[test]
    fn settings_file_accepts_paths_and_rejects_secrets() {
        let accepted = parse_query_settings(
            br#"
            [source]
            root = "/private/wechat/db_storage"
            passphrase_file = "/private/keys/wechat.txt"

            [profile]
            default = "archive"
            "#,
        )
        .unwrap();
        assert_eq!(
            accepted.source_root.unwrap(),
            PathBuf::from("/private/wechat/db_storage")
        );
        assert_eq!(accepted.default_profile.as_deref(), Some("archive"));

        assert!(parse_query_settings(b"passphrase = \"secret\"\n").is_err());
        assert!(parse_query_settings(b"[source]\nroot = \"relative/db_storage\"\n").is_err());
    }

    #[test]
    fn live_discovery_selects_the_newest_complete_database_root() {
        let home = tempfile::tempdir().unwrap();
        fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let files = home
            .path()
            .join("Library/Containers/com.tencent.xinWeChat/Data/Documents/xwechat_files");
        let older = files.join("account-older/db_storage");
        let newer = files.join("account-newer/db_storage");
        let incomplete = files.join("account-incomplete/db_storage");
        for root in [&older, &newer] {
            for component in ["contact", "session", "message"] {
                fs::create_dir_all(root.join(component)).unwrap();
            }
        }
        fs::create_dir_all(incomplete.join("contact")).unwrap();
        let stale = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 24 * 60 * 60);
        for component in ["contact", "session", "message"] {
            let file = older.join(component).join("stale.db");
            fs::write(&file, b"old").unwrap();
            filetime::set_file_mtime(&file, filetime::FileTime::from_system_time(stale)).unwrap();
        }
        let later = std::time::SystemTime::now();
        for component in ["contact", "session", "message"] {
            let file = newer.join(component).join("live.db");
            fs::write(&file, b"new").unwrap();
            filetime::set_file_mtime(&file, filetime::FileTime::from_system_time(later)).unwrap();
        }

        let previous = env::var_os("HOME");
        env::set_var("HOME", home.path());
        let discovered = discover_default_live_source_root();
        assert_eq!(discovered.unwrap(), newer);

        // A second recently written account is not a guess.
        let recent_other = files.join("account-also-live/db_storage");
        for component in ["contact", "session", "message"] {
            fs::create_dir_all(recent_other.join(component)).unwrap();
            fs::write(recent_other.join(component).join("live.db"), b"also").unwrap();
        }
        let ambiguous = discover_default_live_source_root();
        match previous {
            Some(value) => env::set_var("HOME", value),
            None => env::remove_var("HOME"),
        }
        assert!(ambiguous.is_err());
    }

    #[test]
    fn configuration_reader_requires_private_real_files() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let configuration = directory.path().join("query-profiles.json");
        let store = QueryProfileStore {
            schema: QUERY_PROFILE_SCHEMA.into(),
            format_version: QUERY_PROFILE_FORMAT_VERSION,
            default_profile: Some("plain".into()),
            profiles: BTreeMap::from([(
                "plain".into(),
                QueryProfile {
                    source_root: PathBuf::from("/private/source"),
                    access: QueryProfileAccess::Decrypted(QueryDecryptedAccess::default()),
                },
            )]),
        };
        fs::write(&configuration, serde_json::to_vec(&store).unwrap()).unwrap();
        fs::set_permissions(&configuration, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(QueryProfileStore::load(&configuration).unwrap(), store);

        fs::set_permissions(&configuration, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(QueryProfileStore::load(&configuration).is_err());
        fs::set_permissions(&configuration, fs::Permissions::from_mode(0o600)).unwrap();

        let link = directory.path().join("query-profiles-link.json");
        std::os::unix::fs::symlink(&configuration, &link).unwrap();
        assert!(QueryProfileStore::load(&link).is_err());
    }
}
