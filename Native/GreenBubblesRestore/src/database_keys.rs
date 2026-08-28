use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Read, Take};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use serde_json::Value;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::RestoreError;

const MAXIMUM_KEY_EXPORT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Zeroize, ZeroizeOnDrop)]
struct DirectDatabaseKey {
    encryption_key: [u8; 32],
    salt: [u8; 16],
}

pub struct DatabaseKeySet {
    keys: BTreeMap<String, DirectDatabaseKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DatabaseKeyMatchMethod {
    ExactPathAndSalt,
    ExactPathAuthentication,
    UniqueSalt,
    UniqueAuthentication,
}

impl DatabaseKeyMatchMethod {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::ExactPathAndSalt => "exactPathAndSalt",
            Self::ExactPathAuthentication => "exactPathAuthentication",
            Self::UniqueSalt => "uniqueSaltRelocation",
            Self::UniqueAuthentication => "uniquePageAuthentication",
        }
    }
}

pub(crate) struct DatabaseKeyAuthentication<'a> {
    pub(crate) encryption_key: Option<&'a [u8; 32]>,
    pub(crate) method: Option<DatabaseKeyMatchMethod>,
    pub(crate) exported_key_count: usize,
    pub(crate) exact_path_entry: bool,
    pub(crate) matching_salt_entry_count: usize,
    pub(crate) authenticated_entry_count: usize,
}

impl DatabaseKeyAuthentication<'_> {
    pub(crate) fn association_error(&self, source_set_id: &str) -> RestoreError {
        RestoreError::DatabaseKeyAssociation {
            set_id: source_set_id.to_string(),
            exported_key_count: self.exported_key_count,
            exact_path_entry: self.exact_path_entry,
            matching_salt_entry_count: self.matching_salt_entry_count,
            authenticated_entry_count: self.authenticated_entry_count,
        }
    }
}

impl DatabaseKeySet {
    pub fn load(path: &Path) -> Result<Self, RestoreError> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| invalid("unable to open the exported-key file safely"))?;
        let before = file
            .metadata()
            .map_err(|_| invalid("unable to inspect the exported-key file"))?;
        if !before.is_file() || before.nlink() != 1 || before.uid() != unsafe { libc::geteuid() } {
            return Err(invalid(
                "the exported-key input must be one current-user-owned regular file",
            ));
        }
        if before.mode() & 0o077 != 0 {
            return Err(invalid(
                "the exported-key file permissions must deny group and other access",
            ));
        }
        if before.len() == 0 || before.len() > MAXIMUM_KEY_EXPORT_BYTES {
            return Err(invalid(
                "the exported-key file size is outside the safe limit",
            ));
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(before.len() as usize));
        let mut limited: Take<&mut std::fs::File> =
            file.by_ref().take(MAXIMUM_KEY_EXPORT_BYTES + 1);
        limited.read_to_end(&mut bytes)?;
        if bytes.len() as u64 != before.len() {
            return Err(invalid("the exported-key file changed while it was read"));
        }
        let after = file
            .metadata()
            .map_err(|_| invalid("unable to re-inspect the exported-key file"))?;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
        {
            return Err(invalid("the exported-key file changed while it was read"));
        }

        let mut document: Value =
            serde_json::from_slice(&bytes).map_err(|_| invalid("the input is not valid JSON"))?;
        let result = Self::parse(&document);
        zeroize_strings(&mut document);
        result
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub(crate) fn authenticate_database(
        &self,
        logical_path: &str,
        first_page: &[u8],
    ) -> DatabaseKeyAuthentication<'_> {
        if first_page.len() < wx_decrypt::MACOS_4_1_7_31.page_size {
            return DatabaseKeyAuthentication {
                encryption_key: None,
                method: None,
                exported_key_count: self.keys.len(),
                exact_path_entry: false,
                matching_salt_entry_count: 0,
                authenticated_entry_count: 0,
            };
        }
        let mut database_salt = [0_u8; 16];
        database_salt.copy_from_slice(&first_page[..16]);
        let normalized = normalize_path(logical_path).ok();
        let exact_path_entry = normalized
            .as_ref()
            .is_some_and(|path| self.keys.contains_key(path));
        let matching_salt_entry_count = self
            .keys
            .values()
            .filter(|candidate| candidate.salt == database_salt)
            .count();
        let authenticated = self
            .keys
            .iter()
            .filter(|(_, candidate)| {
                wx_decrypt::validate_enc_key(
                    first_page,
                    &candidate.encryption_key,
                    &database_salt,
                    &wx_decrypt::MACOS_4_1_7_31,
                )
            })
            .collect::<Vec<_>>();
        let authenticated_entry_count = authenticated.len();

        let selected = normalized
            .as_ref()
            .and_then(|path| {
                authenticated
                    .iter()
                    .copied()
                    .find(|(candidate_path, _)| *candidate_path == path)
            })
            .map(|(_, key)| {
                let method = if key.salt == database_salt {
                    DatabaseKeyMatchMethod::ExactPathAndSalt
                } else {
                    DatabaseKeyMatchMethod::ExactPathAuthentication
                };
                (key, method)
            })
            .or_else(|| {
                (authenticated.len() == 1).then(|| {
                    let (_, key) = authenticated[0];
                    let method = if key.salt == database_salt {
                        DatabaseKeyMatchMethod::UniqueSalt
                    } else {
                        DatabaseKeyMatchMethod::UniqueAuthentication
                    };
                    (key, method)
                })
            });

        DatabaseKeyAuthentication {
            encryption_key: selected.map(|(key, _)| &key.encryption_key),
            method: selected.map(|(_, method)| method),
            exported_key_count: self.keys.len(),
            exact_path_entry,
            matching_salt_entry_count,
            authenticated_entry_count,
        }
    }

    fn parse(document: &Value) -> Result<Self, RestoreError> {
        let object = document
            .as_object()
            .ok_or_else(|| invalid("the top-level value must be an object"))?;
        let mut keys = BTreeMap::new();
        for (path, value) in object {
            if path == "_db_dir" {
                if !value.is_string() {
                    return Err(invalid("the optional metadata has an invalid type"));
                }
                continue;
            }
            if path.starts_with('_') {
                return Err(invalid("the input contains unknown metadata"));
            }
            let normalized = normalize_path(path)?;
            let entry = value
                .as_object()
                .ok_or_else(|| invalid("a database key entry must be an object"))?;
            if entry
                .keys()
                .any(|field| !matches!(field.as_str(), "enc_key" | "salt" | "size_mb"))
            {
                return Err(invalid("a database key entry contains an unknown field"));
            }
            if entry.get("size_mb").is_some_and(|value| !value.is_number()) {
                return Err(invalid("database size metadata must be numeric"));
            }
            let encryption_key = decode_hex::<32>(
                entry.get("enc_key"),
                "a database key must contain 64 hexadecimal characters",
            )?;
            let salt = decode_hex::<16>(
                entry.get("salt"),
                "a database salt must contain 32 hexadecimal characters",
            )?;
            if keys
                .insert(
                    normalized,
                    DirectDatabaseKey {
                        encryption_key,
                        salt,
                    },
                )
                .is_some()
            {
                return Err(invalid(
                    "multiple entries normalize to the same database path",
                ));
            }
        }
        if keys.is_empty() {
            return Err(invalid("the input contains no database keys"));
        }
        Ok(Self { keys })
    }
}

fn decode_hex<const N: usize>(
    value: Option<&Value>,
    message: &'static str,
) -> Result<[u8; N], RestoreError> {
    let encoded = value
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(message))?;
    if encoded.len() != N * 2 || !encoded.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid(message));
    }
    let mut decoded = Zeroizing::new(hex::decode(encoded).map_err(|_| invalid(message))?);
    let mut output = [0_u8; N];
    output.copy_from_slice(&decoded);
    decoded.zeroize();
    Ok(output)
}

fn normalize_path(path: &str) -> Result<String, RestoreError> {
    let normalized = path.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.contains('\0')
        || normalized
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == ".." || part.contains(':'))
    {
        return Err(invalid("a database key path is not a safe relative path"));
    }
    Ok(normalized)
}

fn zeroize_strings(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(zeroize_strings),
        Value::Object(values) => values.values_mut().for_each(zeroize_strings),
        _ => {}
    }
}

fn invalid(reason: &str) -> RestoreError {
    RestoreError::InvalidDatabaseKeyExport(reason.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn normalizes_paths_ignores_source_root_metadata_and_reports_safe_mismatch_counts() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("keys.json");
        write_private(
            &path,
            serde_json::json!({
                "message\\message_0.db": {
                    "enc_key": "11".repeat(32),
                    "salt": "22".repeat(16),
                    "size_mb": 2.5
                },
                "_db_dir": "/never/trusted"
            }),
        );
        let keys = DatabaseKeySet::load(&path).unwrap();
        assert_eq!(keys.len(), 1);
        assert!(!keys.is_empty());
        let mut unauthenticated_page = vec![0_u8; wx_decrypt::MACOS_4_1_7_31.page_size];
        unauthenticated_page[..16].fill(0x22);
        let authentication =
            keys.authenticate_database("relocated/database.db", &unauthenticated_page);
        assert!(authentication.encryption_key.is_none());
        assert_eq!(authentication.exported_key_count, 1);
        assert!(!authentication.exact_path_entry);
        assert_eq!(authentication.matching_salt_entry_count, 1);
        assert_eq!(authentication.authenticated_entry_count, 0);
    }

    #[test]
    fn rejects_unsafe_duplicate_malformed_and_public_inputs() {
        let fixture = tempfile::tempdir().unwrap();
        for (name, value) in [
            (
                "unsafe.json",
                serde_json::json!({
                    "../message.db": {"enc_key": "11".repeat(32), "salt": "22".repeat(16)}
                }),
            ),
            (
                "duplicate.json",
                serde_json::json!({
                    "message/message.db": {"enc_key": "11".repeat(32), "salt": "22".repeat(16)},
                    "message\\message.db": {"enc_key": "33".repeat(32), "salt": "44".repeat(16)}
                }),
            ),
            (
                "malformed.json",
                serde_json::json!({
                    "message.db": {"enc_key": "not-a-key", "salt": "22".repeat(16)}
                }),
            ),
        ] {
            let path = fixture.path().join(name);
            write_private(&path, value);
            assert!(DatabaseKeySet::load(&path).is_err());
        }

        let public = fixture.path().join("public.json");
        write_private(
            &public,
            serde_json::json!({
                "message.db": {"enc_key": "11".repeat(32), "salt": "22".repeat(16)}
            }),
        );
        fs::set_permissions(&public, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(DatabaseKeySet::load(&public).is_err());
    }

    fn write_private(path: &Path, value: Value) {
        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
