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
    pub entries: Vec<SnapshotEntry>,
}

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
        if manifest.manifest_format_version != 1 {
            return Err(RestoreError::Manifest(format!(
                "unsupported format version {}",
                manifest.manifest_format_version
            )));
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
