use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::RestoreError;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotManifest {
    pub manifest_format_version: u32,
    pub snapshot_id: String,
    pub created_at: String,
    pub source_fingerprint: String,
    #[serde(default)]
    pub client_build: Option<ClientBuildFingerprint>,
    pub entries: Vec<SnapshotEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientBuildFingerprint {
    pub format_version: u32,
    pub bundle_identifier: String,
    pub marketing_version: String,
    pub build_version: String,
    pub executable_sha256: String,
    pub signing_identifier: String,
    pub team_identifier: String,
    pub code_directory_sha256: String,
    pub architectures: Vec<String>,
    pub hardened_runtime: bool,
    pub signature_valid: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClientBuildCompatibilityState {
    SupportedPinned,
    Unsupported,
    Missing,
    LegacySyntheticFixture,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientBuildCompatibilityEvidence {
    pub state: ClientBuildCompatibilityState,
    pub production_compatible: bool,
    pub supported_profile_id: String,
    pub mismatched_fields: Vec<String>,
    pub observed: Option<ClientBuildFingerprint>,
}

impl Default for ClientBuildCompatibilityEvidence {
    fn default() -> Self {
        Self {
            state: ClientBuildCompatibilityState::LegacySyntheticFixture,
            production_compatible: false,
            supported_profile_id: SUPPORTED_PROFILE_ID.to_string(),
            mismatched_fields: Vec::new(),
            observed: None,
        }
    }
}

const SUPPORTED_PROFILE_ID: &str = "wechat-macos-4.1.12-269365";
const SUPPORTED_EXECUTABLE_SHA256: &str =
    "2c61ba7f64c2b98e897553cd226364642a1eb213b5b7f74556c6fc2efc363e32";
const SUPPORTED_CODE_DIRECTORY_SHA256: &str =
    "fa11b242567cbe161e2b332139dbc459c534b85f3855a8603614252bf908106e";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotEntry {
    pub source: PathReference,
    pub source_set_id: String,
    pub logical_path: String,
    pub relative_path: String,
    pub role: SnapshotFileRole,
    pub fingerprint: SourceFileFingerprint,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathReference {
    pub opaque_id: String,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFileFingerprint {
    pub device_id: u64,
    pub file_id: u64,
    pub byte_count: i64,
    pub modified_seconds: i64,
    pub modified_nanoseconds: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SnapshotFileRole {
    Database,
    WriteAheadLog,
    SharedMemory,
}

impl SnapshotManifest {
    pub fn load(snapshot_dir: &Path) -> Result<Self, RestoreError> {
        let manifest_path = snapshot_dir.join("manifest.json");
        let data = fs::read(&manifest_path)
            .map_err(|e| RestoreError::Manifest(format!("{}: {e}", manifest_path.display())))?;
        let manifest: Self = serde_json::from_slice(&data)?;
        if !matches!(manifest.manifest_format_version, 1 | 2) {
            return Err(RestoreError::Manifest(format!(
                "unsupported format version {}",
                manifest.manifest_format_version
            )));
        }
        if manifest.manifest_format_version == 1 && manifest.client_build.is_some() {
            return Err(RestoreError::Manifest(
                "format-1 snapshot cannot contain client-build evidence".to_string(),
            ));
        }
        if let Some(build) = &manifest.client_build {
            build.validate()?;
        }
        for entry in &manifest.entries {
            let _ = entry.resolved_path(snapshot_dir)?;
            validate_logical_path(&entry.logical_path)?;
        }
        Ok(manifest)
    }

    pub fn database_entries(&self) -> impl Iterator<Item = &SnapshotEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.role == SnapshotFileRole::Database)
    }

    pub fn sidecar(&self, set_id: &str, role: SnapshotFileRole) -> Option<&SnapshotEntry> {
        self.entries
            .iter()
            .find(|entry| entry.source_set_id == set_id && entry.role == role)
    }

    pub fn client_build_compatibility(&self) -> ClientBuildCompatibilityEvidence {
        if self.manifest_format_version == 1 {
            return ClientBuildCompatibilityEvidence::default();
        }
        let Some(observed) = self.client_build.clone() else {
            return ClientBuildCompatibilityEvidence {
                state: ClientBuildCompatibilityState::Missing,
                production_compatible: false,
                supported_profile_id: SUPPORTED_PROFILE_ID.to_string(),
                mismatched_fields: Vec::new(),
                observed: None,
            };
        };
        let expected = supported_client_build();
        let mut mismatched_fields = Vec::new();
        compare_field(
            &mut mismatched_fields,
            "formatVersion",
            observed.format_version == expected.format_version,
        );
        compare_field(
            &mut mismatched_fields,
            "bundleIdentifier",
            observed.bundle_identifier == expected.bundle_identifier,
        );
        compare_field(
            &mut mismatched_fields,
            "marketingVersion",
            observed.marketing_version == expected.marketing_version,
        );
        compare_field(
            &mut mismatched_fields,
            "buildVersion",
            observed.build_version == expected.build_version,
        );
        compare_field(
            &mut mismatched_fields,
            "executableSHA256",
            observed
                .executable_sha256
                .eq_ignore_ascii_case(&expected.executable_sha256),
        );
        compare_field(
            &mut mismatched_fields,
            "signingIdentifier",
            observed.signing_identifier == expected.signing_identifier,
        );
        compare_field(
            &mut mismatched_fields,
            "teamIdentifier",
            observed.team_identifier == expected.team_identifier,
        );
        compare_field(
            &mut mismatched_fields,
            "codeDirectorySHA256",
            observed
                .code_directory_sha256
                .eq_ignore_ascii_case(&expected.code_directory_sha256),
        );
        let mut observed_architectures = observed.architectures.clone();
        observed_architectures.sort();
        observed_architectures.dedup();
        compare_field(
            &mut mismatched_fields,
            "architectures",
            observed_architectures == expected.architectures,
        );
        compare_field(
            &mut mismatched_fields,
            "hardenedRuntime",
            observed.hardened_runtime == expected.hardened_runtime,
        );
        compare_field(
            &mut mismatched_fields,
            "signatureValid",
            observed.signature_valid == expected.signature_valid,
        );
        let production_compatible = mismatched_fields.is_empty();
        ClientBuildCompatibilityEvidence {
            state: if production_compatible {
                ClientBuildCompatibilityState::SupportedPinned
            } else {
                ClientBuildCompatibilityState::Unsupported
            },
            production_compatible,
            supported_profile_id: SUPPORTED_PROFILE_ID.to_string(),
            mismatched_fields,
            observed: Some(observed),
        }
    }
}

impl ClientBuildFingerprint {
    fn validate(&self) -> Result<(), RestoreError> {
        let valid_hash =
            |value: &str| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
        if self.format_version != 1
            || self.bundle_identifier.is_empty()
            || self.marketing_version.is_empty()
            || self.build_version.is_empty()
            || !valid_hash(&self.executable_sha256)
            || self.signing_identifier.is_empty()
            || self.team_identifier.is_empty()
            || !valid_hash(&self.code_directory_sha256)
            || self.architectures.is_empty()
        {
            return Err(RestoreError::Manifest(
                "client-build fingerprint is incomplete or malformed".to_string(),
            ));
        }
        Ok(())
    }
}

fn supported_client_build() -> ClientBuildFingerprint {
    ClientBuildFingerprint {
        format_version: 1,
        bundle_identifier: "com.tencent.xinWeChat".to_string(),
        marketing_version: "4.1.12".to_string(),
        build_version: "269365".to_string(),
        executable_sha256: SUPPORTED_EXECUTABLE_SHA256.to_string(),
        signing_identifier: "com.tencent.xinWeChat".to_string(),
        team_identifier: "5A4RE8SF68".to_string(),
        code_directory_sha256: SUPPORTED_CODE_DIRECTORY_SHA256.to_string(),
        architectures: vec!["arm64".to_string(), "x86_64".to_string()],
        hardened_runtime: true,
        signature_valid: true,
    }
}

fn compare_field(mismatches: &mut Vec<String>, field: &str, matches: bool) {
    if !matches {
        mismatches.push(field.to_string());
    }
}

impl SnapshotEntry {
    pub fn resolved_path(&self, snapshot_dir: &Path) -> Result<PathBuf, RestoreError> {
        let relative = Path::new(&self.relative_path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(RestoreError::UnsafePath(self.relative_path.clone()));
        }
        let result = snapshot_dir.join(relative);
        if !result.starts_with(snapshot_dir) {
            return Err(RestoreError::UnsafePath(self.relative_path.clone()));
        }
        Ok(result)
    }
}

fn validate_logical_path(value: &str) -> Result<(), RestoreError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(RestoreError::UnsafePath(value.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(format_version: u32, build: Option<ClientBuildFingerprint>) -> SnapshotManifest {
        SnapshotManifest {
            manifest_format_version: format_version,
            snapshot_id: "synthetic-snapshot".to_string(),
            created_at: "2026-08-27T00:00:00Z".to_string(),
            source_fingerprint: "synthetic-source".to_string(),
            client_build: build,
            entries: Vec::new(),
        }
    }

    #[test]
    fn exact_pinned_profile_is_production_compatible() {
        let evidence = manifest(2, Some(supported_client_build())).client_build_compatibility();
        assert_eq!(
            evidence.state,
            ClientBuildCompatibilityState::SupportedPinned
        );
        assert!(evidence.production_compatible);
        assert!(evidence.mismatched_fields.is_empty());
    }

    #[test]
    fn version_team_and_hash_drift_are_independently_reported() {
        fn assert_drift(expected_field: &str, mutate: fn(&mut ClientBuildFingerprint)) {
            let mut build = supported_client_build();
            mutate(&mut build);
            let evidence = manifest(2, Some(build)).client_build_compatibility();
            assert_eq!(evidence.state, ClientBuildCompatibilityState::Unsupported);
            assert!(!evidence.production_compatible);
            assert_eq!(evidence.mismatched_fields, [expected_field]);
        }
        assert_drift("marketingVersion", |build| {
            build.marketing_version = "4.1.13".to_string();
        });
        assert_drift("teamIdentifier", |build| {
            build.team_identifier = "DIFFERENT1".to_string();
        });
        assert_drift("executableSHA256", |build| {
            build.executable_sha256 = "0".repeat(64);
        });
    }

    #[test]
    fn missing_current_evidence_and_legacy_fixtures_are_distinct() {
        let missing = manifest(2, None).client_build_compatibility();
        assert_eq!(missing.state, ClientBuildCompatibilityState::Missing);
        assert!(!missing.production_compatible);

        let legacy = manifest(1, None).client_build_compatibility();
        assert_eq!(
            legacy.state,
            ClientBuildCompatibilityState::LegacySyntheticFixture
        );
        assert!(!legacy.production_compatible);
    }

    #[test]
    fn malformed_evidence_is_rejected_before_use() {
        let directory = tempfile::tempdir().unwrap();
        let mut build = supported_client_build();
        build.code_directory_sha256 = "not-a-hash".to_string();
        let path = directory.path().join("manifest.json");
        fs::write(path, serde_json::to_vec(&manifest(2, Some(build))).unwrap()).unwrap();
        let error = SnapshotManifest::load(directory.path()).unwrap_err();
        assert!(error.to_string().contains("incomplete or malformed"));
    }
}
