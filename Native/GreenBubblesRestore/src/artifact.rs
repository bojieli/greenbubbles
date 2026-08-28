use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use base64::Engine;
use rusqlite::{types::ValueRef, Connection, OpenFlags};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::{
    ArtifactAvailability, ArtifactDecodeState, ArtifactKind, ArtifactRole, CanonicalArtifact,
    CanonicalMessage, MessageArtifactReference, PreparedCatalog, PreparedDatabase, RestoreError,
    TypedPayload,
};

#[derive(Debug, Clone)]
struct ResourceRecord {
    source_set_id: String,
    source_logical_path: String,
    source_table_id: String,
    source_table_name: String,
    source_row_id: i64,
    local_id: Option<i64>,
    server_id: Option<i64>,
    packed_info: Vec<u8>,
}

#[derive(Debug, Clone)]
struct VoiceLocator {
    database_path: PathBuf,
    source_set_id: String,
    source_logical_path: String,
    source_table_id: String,
    source_table_name: String,
    source_row_id: i64,
    local_id: Option<i64>,
    server_id: Option<i64>,
    voice_column: String,
}

type ResourceIndex = HashMap<i64, Vec<ResourceRecord>>;
type VoiceIndex = HashMap<i64, Vec<VoiceLocator>>;

#[derive(Debug, Default)]
struct MediaFileIndex {
    by_md5: HashMap<String, Vec<PathBuf>>,
    by_name: HashMap<String, Vec<PathBuf>>,
}

pub struct ArtifactResolver {
    account_root: Option<PathBuf>,
    output_directory: PathBuf,
    resource_by_local: HashMap<i64, Vec<ResourceRecord>>,
    resource_by_server: HashMap<i64, Vec<ResourceRecord>>,
    voice_by_local: HashMap<i64, Vec<VoiceLocator>>,
    voice_by_server: HashMap<i64, Vec<VoiceLocator>>,
    file_index: MediaFileIndex,
    resource_index_incomplete: bool,
    voice_index_incomplete: bool,
    file_index_incomplete: bool,
    v2_image_key: Option<[u8; 16]>,
    artifacts: BTreeMap<String, CanonicalArtifact>,
    verified_file_cache: HashMap<String, String>,
    deferred: bool,
}

impl ArtifactResolver {
    pub fn new(
        catalog: &PreparedCatalog,
        account_root: Option<&Path>,
        output_directory: &Path,
        defer_media: bool,
    ) -> Result<Self, RestoreError> {
        let account_root = match account_root {
            Some(path) => {
                let canonical = fs::canonicalize(path)?;
                if !canonical.is_dir()
                    || !canonical.join("db_storage").is_dir()
                    || !canonical.join("msg").is_dir()
                {
                    return Err(RestoreError::Integrity(
                        "the supplied account root is not a single WeChat account directory"
                            .to_string(),
                    ));
                }
                validate_account_binding(catalog, &canonical)?;
                Some(canonical)
            }
            None => None,
        };

        let (resource_by_local, resource_by_server, resource_index_incomplete) = if defer_media {
            (HashMap::new(), HashMap::new(), false)
        } else {
            load_resource_index(catalog)
        };
        let (voice_by_local, voice_by_server, voice_index_incomplete) = if defer_media {
            (HashMap::new(), HashMap::new(), false)
        } else {
            load_voice_index(catalog)
        };
        let (file_index, file_index_incomplete) = if defer_media {
            (MediaFileIndex::default(), false)
        } else {
            account_root
                .as_deref()
                .map(build_file_index)
                .unwrap_or_default()
        };
        let v2_image_key = if defer_media {
            None
        } else {
            account_root
                .as_deref()
                .and_then(|root| wx_media::derive_v2_key_from_dir(root).ok())
        };

        let derived = output_directory.join("derived");
        create_owner_only_directory(&derived)?;

        Ok(Self {
            account_root,
            output_directory: output_directory.to_path_buf(),
            resource_by_local,
            resource_by_server,
            voice_by_local,
            voice_by_server,
            file_index,
            resource_index_incomplete,
            voice_index_incomplete,
            file_index_incomplete,
            v2_image_key,
            artifacts: BTreeMap::new(),
            verified_file_cache: HashMap::new(),
            deferred: defer_media,
        })
    }

    pub fn resolve_message(
        &mut self,
        message: &CanonicalMessage,
    ) -> Result<Vec<MessageArtifactReference>, RestoreError> {
        let Some((kind, default_role)) = media_descriptor(message) else {
            return Ok(Vec::new());
        };

        if self.deferred {
            let identity = format!(
                "deferred:{}:{kind:?}:{default_role:?}",
                message.canonical_id
            );
            let artifact_id = opaque_id(identity.as_bytes());
            self.artifacts
                .entry(artifact_id.clone())
                .or_insert_with(|| CanonicalArtifact {
                    artifact_id: artifact_id.clone(),
                    kind,
                    role: default_role,
                    roles: BTreeSet::from([default_role]),
                    availability: ArtifactAvailability::MetadataMissing,
                    source_md5: None,
                    source_local_path: None,
                    account_relative_path: None,
                    source_byte_count: None,
                    source_device_id: None,
                    source_file_id: None,
                    source_modified_seconds: None,
                    source_modified_nanoseconds: None,
                    source_sha256: None,
                    detected_format: None,
                    materialized_local_path: None,
                    decoded_local_path: None,
                    decoded_byte_count: None,
                    decoded_sha256: None,
                    decoded_format: None,
                    decode_state: ArtifactDecodeState::NotRequired,
                    verification_detail: Some(
                        "media resolution was explicitly deferred from the text restoration pass"
                            .to_string(),
                    ),
                    source_resource_set_id: None,
                    source_resource_logical_path: None,
                    source_resource_table_id: None,
                    source_resource_table_name: None,
                    source_resource_row_id: None,
                });
            return Ok(vec![MessageArtifactReference {
                artifact_id,
                role: default_role,
                preferred: true,
            }]);
        }

        if kind == ArtifactKind::Voice {
            return self.resolve_voice(message);
        }

        let resources = self.resource_candidates(message);
        let mut md5s = BTreeSet::new();
        collect_md5s_from_typed(&message.typed_payload, &mut md5s);
        if let Some(packed) = message
            .packed_info_base64
            .as_deref()
            .and_then(|value| base64::engine::general_purpose::STANDARD.decode(value).ok())
        {
            collect_md5s(&packed, &mut md5s);
        }
        if let Some(content) = message
            .content_base64
            .as_deref()
            .and_then(|value| base64::engine::general_purpose::STANDARD.decode(value).ok())
        {
            collect_md5s(&content, &mut md5s);
        }
        for resource in &resources {
            collect_md5s(&resource.packed_info, &mut md5s);
        }

        let title = typed_string_field(&message.typed_payload, "title");
        let mut candidates = BTreeSet::new();
        for md5 in &md5s {
            if let Some(paths) = self.file_index.by_md5.get(md5) {
                candidates.extend(paths.iter().cloned());
            }
        }
        if let Some(title) = title.as_deref() {
            if let Some(paths) = self.file_index.by_name.get(&title.to_ascii_lowercase()) {
                candidates.extend(paths.iter().cloned());
            }
        }

        if candidates.is_empty() {
            let metadata_index_gap =
                self.resource_index_incomplete && md5s.is_empty() && title.is_none();
            let availability = if self.account_root.is_none() {
                ArtifactAvailability::AccountRootUnavailable
            } else if metadata_index_gap || self.file_index_incomplete {
                ArtifactAvailability::MetadataMissing
            } else if md5s.is_empty() && title.is_none() {
                ArtifactAvailability::MetadataMissing
            } else {
                ArtifactAvailability::NotDownloaded
            };
            let md5 = md5s.first().cloned();
            let resource = (resources.len() == 1).then(|| &resources[0]);
            let identity = format!(
                "missing:{}:{kind:?}:{default_role:?}:{}:{}",
                message.canonical_id,
                md5.as_deref().unwrap_or(""),
                title.as_deref().unwrap_or("")
            );
            let artifact_id = opaque_id(identity.as_bytes());
            self.artifacts
                .entry(artifact_id.clone())
                .or_insert_with(|| CanonicalArtifact {
                    artifact_id: artifact_id.clone(),
                    kind,
                    role: default_role,
                    roles: BTreeSet::from([default_role]),
                    availability,
                    source_md5: md5,
                    source_local_path: None,
                    account_relative_path: None,
                    source_byte_count: None,
                    source_device_id: None,
                    source_file_id: None,
                    source_modified_seconds: None,
                    source_modified_nanoseconds: None,
                    source_sha256: None,
                    detected_format: None,
                    materialized_local_path: None,
                    decoded_local_path: None,
                    decoded_byte_count: None,
                    decoded_sha256: None,
                    decoded_format: None,
                    decode_state: ArtifactDecodeState::NotRequired,
                    verification_detail: Some(match availability {
                        ArtifactAvailability::AccountRootUnavailable => {
                            "account root was not supplied, so local availability was not checked"
                                .to_string()
                        }
                        ArtifactAvailability::MetadataMissing => {
                            if metadata_index_gap {
                                "one or more optional resource metadata tables were unavailable; the message was retained without a local resource key"
                                    .to_string()
                            } else if self.file_index_incomplete {
                                "the optional media-file inventory was incomplete; the message was retained without claiming that the artifact is absent"
                                    .to_string()
                            } else {
                                "message has a media type but no local resource key was decoded"
                                    .to_string()
                            }
                        }
                        _ => "resource metadata exists but no matching local file was found"
                            .to_string(),
                    }),
                    source_resource_set_id: resource.map(|value| value.source_set_id.clone()),
                    source_resource_logical_path: resource
                        .map(|value| value.source_logical_path.clone()),
                    source_resource_table_id: resource.map(|value| value.source_table_id.clone()),
                    source_resource_table_name: resource
                        .map(|value| value.source_table_name.clone()),
                    source_resource_row_id: resource.map(|value| value.source_row_id),
                });
            self.note_reference_role(&artifact_id, default_role);
            return Ok(vec![MessageArtifactReference {
                artifact_id,
                role: default_role,
                preferred: true,
            }]);
        }

        let candidate_count = candidates.len();
        let mut references = Vec::new();
        for path in candidates {
            let role = role_for_path(&path, kind, default_role);
            let source_md5 = md5s
                .iter()
                .find(|md5| path_matches_md5(&path, md5))
                .cloned();
            let resource = resources
                .iter()
                .find(|resource| resource_matches_path(resource, &path))
                .or_else(|| (resources.len() == 1).then(|| &resources[0]));
            let artifact_id = match self.verify_path(
                &path,
                kind,
                role,
                source_md5.clone(),
                resource,
            ) {
                Ok(artifact_id) => artifact_id,
                Err(_) => self.record_unavailable_path(
                    &path,
                    kind,
                    role,
                    ArtifactAvailability::Corrupt,
                    source_md5,
                    resource,
                    "media candidate could not be read and was omitted without rejecting its message",
                ),
            };
            self.note_reference_role(&artifact_id, role);
            references.push(MessageArtifactReference {
                artifact_id,
                role,
                preferred: false,
            });
        }
        references.sort_by_key(|value| (role_rank(value.role), value.artifact_id.clone()));
        if let Some(first) = references.first_mut() {
            first.preferred = true;
        }

        if candidate_count > 1 {
            let mut role_counts: HashMap<ArtifactRole, usize> = HashMap::new();
            for reference in &references {
                *role_counts.entry(reference.role).or_default() += 1;
            }
            for reference in &references {
                if role_counts
                    .get(&reference.role)
                    .copied()
                    .unwrap_or_default()
                    > 1
                {
                    if let Some(artifact) = self.artifacts.get_mut(&reference.artifact_id) {
                        artifact.availability = ArtifactAvailability::Ambiguous;
                        artifact.verification_detail = Some(
                            "multiple downloaded candidates have the same semantic role; all were retained"
                                .to_string(),
                        );
                    }
                }
            }
        }
        Ok(references)
    }

    pub fn artifacts(&self) -> impl Iterator<Item = &CanonicalArtifact> {
        self.artifacts.values()
    }

    fn resource_candidates(&self, message: &CanonicalMessage) -> Vec<ResourceRecord> {
        let mut result = message
            .server_id
            .and_then(|id| self.resource_by_server.get(&id))
            .cloned()
            .unwrap_or_default();
        if result.is_empty() {
            result = message
                .local_id
                .and_then(|id| self.resource_by_local.get(&id))
                .cloned()
                .unwrap_or_default();
        }
        result.sort_by_key(|value| (value.source_set_id.clone(), value.source_row_id));
        result
    }

    fn resolve_voice(
        &mut self,
        message: &CanonicalMessage,
    ) -> Result<Vec<MessageArtifactReference>, RestoreError> {
        let mut locators = message
            .server_id
            .and_then(|id| self.voice_by_server.get(&id))
            .cloned()
            .unwrap_or_default();
        if locators.is_empty() {
            locators = message
                .local_id
                .and_then(|id| self.voice_by_local.get(&id))
                .cloned()
                .unwrap_or_default();
        }
        locators.sort_by_key(|value| (value.source_set_id.clone(), value.source_row_id));

        if locators.is_empty() {
            let identity = format!("missing:{}:voice", message.canonical_id);
            let artifact_id = opaque_id(identity.as_bytes());
            let availability = if self.voice_index_incomplete {
                ArtifactAvailability::MetadataMissing
            } else {
                ArtifactAvailability::NotDownloaded
            };
            self.artifacts
                .entry(artifact_id.clone())
                .or_insert(CanonicalArtifact {
                    artifact_id: artifact_id.clone(),
                    kind: ArtifactKind::Voice,
                    role: ArtifactRole::VoicePayload,
                    roles: BTreeSet::from([ArtifactRole::VoicePayload]),
                    availability,
                    source_md5: None,
                    source_local_path: None,
                    account_relative_path: None,
                    source_byte_count: None,
                    source_device_id: None,
                    source_file_id: None,
                    source_modified_seconds: None,
                    source_modified_nanoseconds: None,
                    source_sha256: None,
                    detected_format: None,
                    materialized_local_path: None,
                    decoded_local_path: None,
                    decoded_byte_count: None,
                    decoded_sha256: None,
                    decoded_format: None,
                    decode_state: ArtifactDecodeState::NotRequired,
                    verification_detail: Some(if self.voice_index_incomplete {
                        "one or more optional VoiceInfo tables were unavailable; the message was retained without a voice payload"
                            .to_string()
                    } else {
                        "no matching VoiceInfo payload was present in the authorized snapshot"
                            .to_string()
                    }),
                    source_resource_set_id: None,
                    source_resource_logical_path: None,
                    source_resource_table_id: None,
                    source_resource_table_name: None,
                    source_resource_row_id: None,
                });
            self.note_reference_role(&artifact_id, ArtifactRole::VoicePayload);
            return Ok(vec![MessageArtifactReference {
                artifact_id,
                role: ArtifactRole::VoicePayload,
                preferred: true,
            }]);
        }

        let ambiguous = locators.len() > 1;
        let mut result = Vec::new();
        for locator in locators {
            let connection = match Connection::open_with_flags(
                &locator.database_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            ) {
                Ok(connection) => connection,
                Err(_) => {
                    let artifact_id = self.record_unavailable_voice(
                        message,
                        &locator,
                        "voice payload database became unavailable; the message was retained",
                    );
                    result.push(MessageArtifactReference {
                        artifact_id,
                        role: ArtifactRole::VoicePayload,
                        preferred: false,
                    });
                    continue;
                }
            };
            if connection.execute_batch("PRAGMA query_only = ON").is_err() {
                let artifact_id = self.record_unavailable_voice(
                    message,
                    &locator,
                    "voice payload database could not enter read-only mode; the message was retained",
                );
                result.push(MessageArtifactReference {
                    artifact_id,
                    role: ArtifactRole::VoicePayload,
                    preferred: false,
                });
                continue;
            }
            let sql = format!(
                "SELECT {} FROM VoiceInfo WHERE rowid = ?1",
                quote_identifier(&locator.voice_column)
            );
            let data: Vec<u8> =
                match connection.query_row(&sql, [locator.source_row_id], |row| row.get(0)) {
                    Ok(data) => data,
                    Err(_) => {
                        let artifact_id = self.record_unavailable_voice(
                        message,
                        &locator,
                        "voice payload row was unavailable or malformed; the message was retained",
                    );
                        result.push(MessageArtifactReference {
                            artifact_id,
                            role: ArtifactRole::VoicePayload,
                            preferred: false,
                        });
                        continue;
                    }
                };
            let sha256 = hex::encode(Sha256::digest(&data));
            let identity = format!(
                "voice:{}:{}:{}",
                locator.source_set_id, locator.source_row_id, sha256
            );
            let artifact_id = opaque_id(identity.as_bytes());
            let relative = PathBuf::from("derived")
                .join("voice")
                .join(format!("{artifact_id}.silk"));
            let destination = self.output_directory.join(&relative);
            write_owner_only_once(&destination, &data)?;
            let detected = detect_format(&data, Some("silk"));
            let mut artifact = CanonicalArtifact {
                artifact_id: artifact_id.clone(),
                kind: ArtifactKind::Voice,
                role: ArtifactRole::VoicePayload,
                roles: BTreeSet::from([ArtifactRole::VoicePayload]),
                availability: if ambiguous {
                    ArtifactAvailability::Ambiguous
                } else {
                    ArtifactAvailability::MaterializedFromDatabase
                },
                source_md5: None,
                source_local_path: None,
                account_relative_path: None,
                source_byte_count: Some(data.len() as u64),
                source_device_id: None,
                source_file_id: None,
                source_modified_seconds: None,
                source_modified_nanoseconds: None,
                source_sha256: Some(sha256),
                detected_format: Some(detected),
                materialized_local_path: Some(destination.display().to_string()),
                decoded_local_path: None,
                decoded_byte_count: None,
                decoded_sha256: None,
                decoded_format: None,
                decode_state: ArtifactDecodeState::Unsupported,
                verification_detail: Some(if ambiguous {
                    "multiple VoiceInfo rows matched; every lossless payload was materialized"
                        .to_string()
                } else {
                    "lossless SILK payload materialized from the snapshot".to_string()
                }),
                source_resource_set_id: Some(locator.source_set_id),
                source_resource_logical_path: Some(locator.source_logical_path),
                source_resource_table_id: Some(locator.source_table_id),
                source_resource_table_name: Some(locator.source_table_name),
                source_resource_row_id: Some(locator.source_row_id),
            };
            match wx_media::transcode_silk_to_ogg_opus(&data) {
                Ok(decoded) => {
                    let decoded_relative = PathBuf::from("derived")
                        .join("voice")
                        .join(format!("{artifact_id}.{}", decoded.ext));
                    let decoded_destination = self.output_directory.join(decoded_relative);
                    write_owner_only_once(&decoded_destination, &decoded.data)?;
                    artifact.decoded_local_path = Some(decoded_destination.display().to_string());
                    artifact.decoded_byte_count = Some(decoded.data.len() as u64);
                    artifact.decoded_sha256 = Some(hex::encode(Sha256::digest(&decoded.data)));
                    artifact.decoded_format = Some(decoded.ext.to_string());
                    artifact.decode_state = ArtifactDecodeState::Decoded;
                    artifact.verification_detail = Some(
                        "lossless SILK source retained and an Ogg Opus derivative was verified"
                            .to_string(),
                    );
                }
                Err(error) => {
                    artifact.decode_state = ArtifactDecodeState::Failed;
                    artifact.verification_detail = Some(format!(
                        "lossless SILK source retained; playable derivative failed: {error}"
                    ));
                }
            }
            self.artifacts
                .entry(artifact_id.clone())
                .or_insert(artifact);
            self.note_reference_role(&artifact_id, ArtifactRole::VoicePayload);
            result.push(MessageArtifactReference {
                artifact_id,
                role: ArtifactRole::VoicePayload,
                preferred: false,
            });
        }
        if let Some(first) = result.first_mut() {
            first.preferred = true;
        }
        Ok(result)
    }

    fn record_unavailable_voice(
        &mut self,
        message: &CanonicalMessage,
        locator: &VoiceLocator,
        reason: &str,
    ) -> String {
        let identity = format!(
            "corrupt:voice:{}:{}:{}",
            message.canonical_id, locator.source_set_id, locator.source_row_id
        );
        let artifact_id = opaque_id(identity.as_bytes());
        self.artifacts
            .entry(artifact_id.clone())
            .or_insert(CanonicalArtifact {
                artifact_id: artifact_id.clone(),
                kind: ArtifactKind::Voice,
                role: ArtifactRole::VoicePayload,
                roles: BTreeSet::from([ArtifactRole::VoicePayload]),
                availability: ArtifactAvailability::Corrupt,
                source_md5: None,
                source_local_path: None,
                account_relative_path: None,
                source_byte_count: None,
                source_device_id: None,
                source_file_id: None,
                source_modified_seconds: None,
                source_modified_nanoseconds: None,
                source_sha256: None,
                detected_format: None,
                materialized_local_path: None,
                decoded_local_path: None,
                decoded_byte_count: None,
                decoded_sha256: None,
                decoded_format: None,
                decode_state: ArtifactDecodeState::Failed,
                verification_detail: Some(reason.to_string()),
                source_resource_set_id: Some(locator.source_set_id.clone()),
                source_resource_logical_path: Some(locator.source_logical_path.clone()),
                source_resource_table_id: Some(locator.source_table_id.clone()),
                source_resource_table_name: Some(locator.source_table_name.clone()),
                source_resource_row_id: Some(locator.source_row_id),
            });
        self.note_reference_role(&artifact_id, ArtifactRole::VoicePayload);
        artifact_id
    }

    fn verify_path(
        &mut self,
        path: &Path,
        kind: ArtifactKind,
        role: ArtifactRole,
        source_md5: Option<String>,
        resource: Option<&ResourceRecord>,
    ) -> Result<String, RestoreError> {
        let Some(root) = self.account_root.as_deref() else {
            return Err(RestoreError::UnsafePath(path.display().to_string()));
        };
        let cache_key = format!("{}:{kind:?}:{role:?}", path.display());
        if let Some(existing) = self.verified_file_cache.get(&cache_key) {
            return Ok(existing.clone());
        }

        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(self.record_unavailable_path(
                    path,
                    kind,
                    role,
                    ArtifactAvailability::Deleted,
                    source_md5,
                    resource,
                    "indexed candidate disappeared before it could be verified",
                ));
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Ok(self.record_unsafe_path(
                path,
                kind,
                role,
                source_md5,
                resource,
                "candidate is not a regular non-symlink file",
            ));
        }
        let canonical = fs::canonicalize(path)?;
        if !canonical.starts_with(root) {
            return Ok(self.record_unsafe_path(
                path,
                kind,
                role,
                source_md5,
                resource,
                "candidate resolves outside the authorized account root",
            ));
        }
        let relative = canonical
            .strip_prefix(root)
            .map_err(|_| RestoreError::UnsafePath(path.display().to_string()))?;
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&canonical)
        {
            Ok(file) => file,
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                return Ok(self.record_unsafe_path(
                    path,
                    kind,
                    role,
                    source_md5,
                    resource,
                    "candidate became a symlink before it could be opened",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(self.record_unavailable_path(
                    path,
                    kind,
                    role,
                    ArtifactAvailability::Deleted,
                    source_md5,
                    resource,
                    "candidate disappeared before its read-only descriptor could be opened",
                ));
            }
            Err(error) => return Err(error.into()),
        };
        let before = file.metadata()?;
        let mut prefix = vec![0_u8; 64];
        let prefix_count = file.read(&mut prefix)?;
        prefix.truncate(prefix_count);
        file.seek(SeekFrom::Start(0))?;
        let mut hasher = Sha256::new();
        let encoded_image = canonical.extension().is_some_and(|value| value == "dat")
            && matches!(kind, ArtifactKind::Image | ArtifactKind::AnimatedImage);
        let mut image_data = None;
        let byte_count = if encoded_image {
            let mut data = Vec::with_capacity(before.len().min(usize::MAX as u64) as usize);
            file.read_to_end(&mut data)?;
            hasher.update(&data);
            let count = data.len() as u64;
            image_data = Some(data);
            count
        } else {
            let mut buffer = vec![0_u8; 128 * 1024];
            let mut count = 0_u64;
            loop {
                let read_count = file.read(&mut buffer)?;
                if read_count == 0 {
                    break;
                }
                hasher.update(&buffer[..read_count]);
                count += read_count as u64;
            }
            count
        };
        let after = file.metadata()?;
        if !same_file_version(&before, &after) || byte_count != before.len() {
            return Err(RestoreError::Integrity(
                "a media source changed while it was being read; restoration was aborted"
                    .to_string(),
            ));
        }
        let source_sha256 = hex::encode(hasher.finalize());
        let identity = format!("file:{}:{}", relative.display(), source_sha256);
        let artifact_id = opaque_id(identity.as_bytes());
        let extension = canonical.extension().and_then(|value| value.to_str());
        let detected_format = detect_format(&prefix, extension);
        let mut artifact = CanonicalArtifact {
            artifact_id: artifact_id.clone(),
            kind,
            role,
            roles: BTreeSet::from([role]),
            availability: ArtifactAvailability::Downloaded,
            source_md5,
            source_local_path: Some(canonical.display().to_string()),
            account_relative_path: Some(relative.display().to_string()),
            source_byte_count: Some(byte_count),
            source_device_id: Some(before.dev()),
            source_file_id: Some(before.ino()),
            source_modified_seconds: Some(before.mtime()),
            source_modified_nanoseconds: Some(before.mtime_nsec()),
            source_sha256: Some(source_sha256),
            detected_format: Some(detected_format),
            materialized_local_path: None,
            decoded_local_path: None,
            decoded_byte_count: None,
            decoded_sha256: None,
            decoded_format: None,
            decode_state: ArtifactDecodeState::NotRequired,
            verification_detail: Some(
                "regular file verified beneath the authorized account root".to_string(),
            ),
            source_resource_set_id: resource.map(|value| value.source_set_id.clone()),
            source_resource_logical_path: resource.map(|value| value.source_logical_path.clone()),
            source_resource_table_id: resource.map(|value| value.source_table_id.clone()),
            source_resource_table_name: resource.map(|value| value.source_table_name.clone()),
            source_resource_row_id: resource.map(|value| value.source_row_id),
        };
        if let Some(data) = image_data.as_deref() {
            self.decode_image(data, &artifact_id, &mut artifact)?;
        }
        match self.artifacts.get_mut(&artifact_id) {
            Some(existing) => {
                // The same physical file is being verified again under another
                // role (its identity is content-based, so the id matches).
                // Keep the first entry's provenance, union the roles, and
                // retain any decode state the new pass does not improve on.
                existing.roles.insert(role);
                if existing.decoded_local_path.is_none() {
                    existing.decoded_local_path = artifact.decoded_local_path;
                    existing.decoded_byte_count = artifact.decoded_byte_count;
                    existing.decoded_sha256 = artifact.decoded_sha256;
                    existing.decoded_format = artifact.decoded_format;
                }
                if matches!(existing.decode_state, ArtifactDecodeState::NotRequired) {
                    existing.decode_state = artifact.decode_state;
                }
            }
            None => {
                self.artifacts.insert(artifact_id.clone(), artifact);
            }
        }
        self.verified_file_cache
            .insert(cache_key, artifact_id.clone());
        Ok(artifact_id)
    }

    fn decode_image(
        &self,
        data: &[u8],
        artifact_id: &str,
        artifact: &mut CanonicalArtifact,
    ) -> Result<(), RestoreError> {
        let format = wx_media::detect_dat_format(data);
        let options = wx_media::DatDecryptOptions {
            v2_aes_key: self.v2_image_key,
            xor_key: None,
        };
        if matches!(format, Some(wx_media::DatFormat::V2)) && self.v2_image_key.is_none() {
            artifact.decode_state = ArtifactDecodeState::KeyUnavailable;
            artifact.verification_detail = Some(
                "encrypted V2 source was retained, but its per-account image key was unavailable"
                    .to_string(),
            );
            return Ok(());
        }
        match wx_media::decrypt_dat(data, &options) {
            Ok(decoded) => {
                let relative = PathBuf::from("derived")
                    .join("images")
                    .join(format!("{artifact_id}.{}", decoded.ext));
                let destination = self.output_directory.join(relative);
                write_owner_only_once(&destination, &decoded.data)?;
                artifact.decoded_local_path = Some(destination.display().to_string());
                artifact.decoded_byte_count = Some(decoded.data.len() as u64);
                artifact.decoded_sha256 = Some(hex::encode(Sha256::digest(&decoded.data)));
                artifact.decoded_format = Some(decoded.ext);
                artifact.decode_state = ArtifactDecodeState::Decoded;
                artifact.verification_detail = Some(format!(
                    "encrypted source retained and a {:?} lossless decoded derivative was verified",
                    decoded.format
                ));
            }
            Err(error) => {
                artifact.decode_state = ArtifactDecodeState::Failed;
                artifact.verification_detail = Some(format!(
                    "encrypted source retained; image decoding failed without changing the source: {error}"
                ));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn record_unsafe_path(
        &mut self,
        path: &Path,
        kind: ArtifactKind,
        role: ArtifactRole,
        source_md5: Option<String>,
        resource: Option<&ResourceRecord>,
        reason: &str,
    ) -> String {
        self.record_unavailable_path(
            path,
            kind,
            role,
            ArtifactAvailability::UnsafePath,
            source_md5,
            resource,
            reason,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_unavailable_path(
        &mut self,
        path: &Path,
        kind: ArtifactKind,
        role: ArtifactRole,
        availability: ArtifactAvailability,
        source_md5: Option<String>,
        resource: Option<&ResourceRecord>,
        reason: &str,
    ) -> String {
        let identity = format!(
            "unavailable:{availability:?}:{}:{kind:?}:{role:?}",
            path.display()
        );
        let artifact_id = opaque_id(identity.as_bytes());
        self.artifacts
            .entry(artifact_id.clone())
            .or_insert(CanonicalArtifact {
                artifact_id: artifact_id.clone(),
                kind,
                role,
                roles: BTreeSet::from([role]),
                availability,
                source_md5,
                source_local_path: None,
                account_relative_path: None,
                source_byte_count: None,
                source_device_id: None,
                source_file_id: None,
                source_modified_seconds: None,
                source_modified_nanoseconds: None,
                source_sha256: None,
                detected_format: None,
                materialized_local_path: None,
                decoded_local_path: None,
                decoded_byte_count: None,
                decoded_sha256: None,
                decoded_format: None,
                decode_state: ArtifactDecodeState::NotRequired,
                verification_detail: Some(reason.to_string()),
                source_resource_set_id: resource.map(|value| value.source_set_id.clone()),
                source_resource_logical_path: resource
                    .map(|value| value.source_logical_path.clone()),
                source_resource_table_id: resource.map(|value| value.source_table_id.clone()),
                source_resource_table_name: resource.map(|value| value.source_table_name.clone()),
                source_resource_row_id: resource.map(|value| value.source_row_id),
            });
        self.note_reference_role(&artifact_id, role);
        artifact_id
    }

    /// Records that a referencing message uses this artifact in the given
    /// role. Artifact identity is content-based, so one physical file can
    /// legitimately serve several roles across messages.
    fn note_reference_role(&mut self, artifact_id: &str, role: ArtifactRole) {
        if let Some(artifact) = self.artifacts.get_mut(artifact_id) {
            artifact.roles.insert(role);
        }
    }
}

fn load_resource_index(catalog: &PreparedCatalog) -> (ResourceIndex, ResourceIndex, bool) {
    let mut by_local: HashMap<i64, Vec<ResourceRecord>> = HashMap::new();
    let mut by_server: HashMap<i64, Vec<ResourceRecord>> = HashMap::new();
    let mut incomplete = false;
    for database in &catalog.databases {
        let Some(table) = database
            .tables
            .iter()
            .find(|name| name.eq_ignore_ascii_case("MessageResourceInfo"))
        else {
            continue;
        };
        let connection = match readonly_connection(database) {
            Ok(connection) => connection,
            Err(_) => {
                incomplete = true;
                continue;
            }
        };
        let columns = match table_columns(&connection, table) {
            Ok(columns) => columns,
            Err(_) => {
                incomplete = true;
                continue;
            }
        };
        let packed_index = find_column(
            &columns,
            &["packed_info", "packed_info_data", "message_packed_info"],
        );
        let Some(packed_index) = packed_index else {
            incomplete = true;
            continue;
        };
        let local_index = find_column(&columns, &["local_id", "message_local_id"]);
        let server_index = find_column(
            &columns,
            &["svr_id", "server_id", "message_svr_id", "message_server_id"],
        );
        let sql = format!("SELECT rowid, * FROM {}", quote_identifier(table));
        let mut statement = match connection.prepare(&sql) {
            Ok(statement) => statement,
            Err(_) => {
                incomplete = true;
                continue;
            }
        };
        let mut rows = match statement.query([]) {
            Ok(rows) => rows,
            Err(_) => {
                incomplete = true;
                continue;
            }
        };
        loop {
            let row = match rows.next() {
                Ok(Some(row)) => row,
                Ok(None) => break,
                Err(_) => {
                    incomplete = true;
                    break;
                }
            };
            let packed_info = get_bytes(row.get_ref(packed_index + 1).ok());
            if packed_info.is_empty() {
                continue;
            }
            let record = ResourceRecord {
                source_set_id: database.source_set_id.clone(),
                source_logical_path: database.logical_path.clone(),
                source_table_id: source_table_id(table),
                source_table_name: table.clone(),
                source_row_id: get_i64(row.get_ref(0).ok()).unwrap_or_default(),
                local_id: local_index.and_then(|index| get_i64(row.get_ref(index + 1).ok())),
                server_id: server_index.and_then(|index| get_i64(row.get_ref(index + 1).ok())),
                packed_info,
            };
            if let Some(id) = record.local_id {
                by_local.entry(id).or_default().push(record.clone());
            }
            if let Some(id) = record.server_id {
                by_server.entry(id).or_default().push(record);
            }
        }
    }
    (by_local, by_server, incomplete)
}

fn load_voice_index(catalog: &PreparedCatalog) -> (VoiceIndex, VoiceIndex, bool) {
    let mut by_local: HashMap<i64, Vec<VoiceLocator>> = HashMap::new();
    let mut by_server: HashMap<i64, Vec<VoiceLocator>> = HashMap::new();
    let mut incomplete = false;
    for database in &catalog.databases {
        let Some(table) = database
            .tables
            .iter()
            .find(|name| name.eq_ignore_ascii_case("VoiceInfo"))
        else {
            continue;
        };
        let connection = match readonly_connection(database) {
            Ok(connection) => connection,
            Err(_) => {
                incomplete = true;
                continue;
            }
        };
        let columns = match table_columns(&connection, table) {
            Ok(columns) => columns,
            Err(_) => {
                incomplete = true;
                continue;
            }
        };
        let Some(voice_index) = find_column(&columns, &["voice_data", "voice_buf", "data"]) else {
            incomplete = true;
            continue;
        };
        let local_index = find_column(&columns, &["local_id", "message_local_id"]);
        let server_index = find_column(
            &columns,
            &["svr_id", "server_id", "message_svr_id", "message_server_id"],
        );
        let selected = [local_index, server_index]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        if selected.is_empty() {
            incomplete = true;
            continue;
        }
        let sql = format!("SELECT rowid, * FROM {}", quote_identifier(table));
        let mut statement = match connection.prepare(&sql) {
            Ok(statement) => statement,
            Err(_) => {
                incomplete = true;
                continue;
            }
        };
        let mut rows = match statement.query([]) {
            Ok(rows) => rows,
            Err(_) => {
                incomplete = true;
                continue;
            }
        };
        loop {
            let row = match rows.next() {
                Ok(Some(row)) => row,
                Ok(None) => break,
                Err(_) => {
                    incomplete = true;
                    break;
                }
            };
            let locator = VoiceLocator {
                database_path: database.path.clone(),
                source_set_id: database.source_set_id.clone(),
                source_logical_path: database.logical_path.clone(),
                source_table_id: source_table_id(table),
                source_table_name: table.clone(),
                source_row_id: get_i64(row.get_ref(0).ok()).unwrap_or_default(),
                local_id: local_index.and_then(|index| get_i64(row.get_ref(index + 1).ok())),
                server_id: server_index.and_then(|index| get_i64(row.get_ref(index + 1).ok())),
                voice_column: columns[voice_index].clone(),
            };
            if let Some(id) = locator.local_id {
                by_local.entry(id).or_default().push(locator.clone());
            }
            if let Some(id) = locator.server_id {
                by_server.entry(id).or_default().push(locator);
            }
        }
    }
    (by_local, by_server, incomplete)
}

fn build_file_index(account_root: &Path) -> (MediaFileIndex, bool) {
    let mut result = MediaFileIndex::default();
    let mut incomplete = false;
    let roots = [
        account_root.join("msg"),
        account_root.join("business/emoticon"),
        account_root.join("business/InputTemp"),
    ];
    for root in roots.iter().filter(|root| root.is_dir()) {
        for entry in WalkDir::new(root).follow_links(false) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    // WeChat mutates this tree while it runs: a candidate that
                    // vanishes between directory read and stat is simply
                    // absent, the same outcome as if it had been deleted
                    // before the walk began.
                    let vanished = error
                        .io_error()
                        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
                        || error.path().is_some_and(|path| !path.exists());
                    if vanished {
                        continue;
                    }
                    // Media discovery is a data surface, not an authorization
                    // boundary. Preserve every candidate already observed and
                    // let unresolved messages retain typed artifact gaps.
                    incomplete = true;
                    continue;
                }
            };
            if !entry.file_type().is_file() && !entry.file_type().is_symlink() {
                continue;
            }
            let path = entry.path().to_path_buf();
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            result
                .by_name
                .entry(name.clone())
                .or_default()
                .push(path.clone());
            for md5 in extract_hex32(name.as_bytes()) {
                result.by_md5.entry(md5).or_default().push(path.clone());
            }
        }
    }
    for paths in result.by_md5.values_mut() {
        paths.sort();
        paths.dedup();
    }
    for paths in result.by_name.values_mut() {
        paths.sort();
        paths.dedup();
    }
    (result, incomplete)
}

fn validate_account_binding(
    catalog: &PreparedCatalog,
    account_root: &Path,
) -> Result<(), RestoreError> {
    for entry in catalog.manifest.database_entries() {
        let expected_path = account_root.join("db_storage").join(&entry.logical_path);
        let expected_id = hex::encode(Sha256::digest(expected_path.to_string_lossy().as_bytes()));
        if entry.source.opaque_id != expected_id[..24] {
            return Err(RestoreError::Integrity(
                "the supplied account root does not match the snapshot source scope".to_string(),
            ));
        }
    }
    Ok(())
}

fn media_descriptor(message: &CanonicalMessage) -> Option<(ArtifactKind, ArtifactRole)> {
    match (message.logical_type?, message.sub_type.unwrap_or_default()) {
        (3, _) => Some((ArtifactKind::Image, ArtifactRole::Original)),
        (34, _) => Some((ArtifactKind::Voice, ArtifactRole::VoicePayload)),
        (43, _) => Some((ArtifactKind::Video, ArtifactRole::VideoPayload)),
        (47, _) => Some((ArtifactKind::AnimatedImage, ArtifactRole::StickerPayload)),
        (49, 2 | 8) => Some((ArtifactKind::Image, ArtifactRole::Original)),
        (49, 3) => Some((ArtifactKind::Voice, ArtifactRole::VoicePayload)),
        (49, 4) => Some((ArtifactKind::Video, ArtifactRole::VideoPayload)),
        (49, 6) => Some((ArtifactKind::Document, ArtifactRole::FilePayload)),
        (49, 51 | 63) => Some((ArtifactKind::Video, ArtifactRole::VideoPayload)),
        (49, 74) => Some((ArtifactKind::Document, ArtifactRole::FilePayload)),
        _ => None,
    }
}

fn collect_md5s_from_typed(payload: &TypedPayload, result: &mut BTreeSet<String>) {
    if let TypedPayload::Decoded(value) = payload {
        collect_md5s_from_json(value, result);
    }
}

fn collect_md5s_from_json(value: &serde_json::Value, result: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::String(value) => collect_md5s(value.as_bytes(), result),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_md5s_from_json(value, result);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values() {
                collect_md5s_from_json(value, result);
            }
        }
        _ => {}
    }
}

fn collect_md5s(value: &[u8], result: &mut BTreeSet<String>) {
    if let Some(md5) = wx_media::extract_md5_from_packed_info(value) {
        result.insert(md5.to_ascii_lowercase());
    }
    result.extend(extract_hex32(value));
}

fn extract_hex32(value: &[u8]) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0;
    while start + 32 <= value.len() {
        let candidate = &value[start..start + 32];
        let left_boundary = start == 0 || !value[start - 1].is_ascii_hexdigit();
        let right_boundary = start + 32 == value.len() || !value[start + 32].is_ascii_hexdigit();
        if left_boundary && right_boundary && candidate.iter().all(u8::is_ascii_hexdigit) {
            result.push(String::from_utf8_lossy(candidate).to_ascii_lowercase());
            start += 32;
        } else {
            start += 1;
        }
    }
    result
}

fn typed_string_field(payload: &TypedPayload, requested: &str) -> Option<String> {
    fn search(value: &serde_json::Value, requested: &str) -> Option<String> {
        match value {
            serde_json::Value::Object(values) => {
                for (key, value) in values {
                    if key.eq_ignore_ascii_case(requested) {
                        if let Some(value) = value.as_str() {
                            if !value.is_empty() {
                                return Some(value.to_string());
                            }
                        }
                    }
                    if let Some(value) = search(value, requested) {
                        return Some(value);
                    }
                }
                None
            }
            serde_json::Value::Array(values) => {
                values.iter().find_map(|value| search(value, requested))
            }
            _ => None,
        }
    }
    match payload {
        TypedPayload::Decoded(value) => search(value, requested),
        TypedPayload::Unknown { .. } => None,
    }
}

fn path_matches_md5(path: &Path, md5: &str) -> bool {
    path.file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase().contains(md5))
        .unwrap_or(false)
}

fn resource_matches_path(resource: &ResourceRecord, path: &Path) -> bool {
    let mut md5s = BTreeSet::new();
    collect_md5s(&resource.packed_info, &mut md5s);
    md5s.iter().any(|md5| path_matches_md5(path, md5))
}

fn role_for_path(path: &Path, kind: ArtifactKind, default: ArtifactRole) -> ArtifactRole {
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name.contains("_h.") {
        return ArtifactRole::HighResolution;
    }
    if name.contains("_t.") {
        return ArtifactRole::Thumbnail;
    }
    if kind == ArtifactKind::Video {
        return match path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "jpg" | "jpeg" | "png" | "webp" => ArtifactRole::VideoPoster,
            _ => ArtifactRole::VideoPayload,
        };
    }
    default
}

fn role_rank(role: ArtifactRole) -> u8 {
    match role {
        ArtifactRole::HighResolution => 0,
        ArtifactRole::Original
        | ArtifactRole::VoicePayload
        | ArtifactRole::VideoPayload
        | ArtifactRole::FilePayload
        | ArtifactRole::StickerPayload => 1,
        ArtifactRole::Thumbnail | ArtifactRole::VideoPoster => 2,
        ArtifactRole::Auxiliary => 3,
        ArtifactRole::Unknown => 4,
    }
}

fn readonly_connection(database: &PreparedDatabase) -> Result<Connection, RestoreError> {
    let connection = Connection::open_with_flags(&database.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.execute_batch("PRAGMA query_only = ON")?;
    Ok(connection)
}

fn table_columns(connection: &Connection, table: &str) -> Result<Vec<String>, RestoreError> {
    let sql = format!("PRAGMA table_info({})", quote_identifier(table));
    let mut statement = connection.prepare(&sql)?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(columns)
}

fn find_column(columns: &[String], aliases: &[&str]) -> Option<usize> {
    columns.iter().position(|column| {
        aliases
            .iter()
            .any(|alias| column.eq_ignore_ascii_case(alias))
    })
}

fn get_i64(value: Option<ValueRef<'_>>) -> Option<i64> {
    match value? {
        ValueRef::Integer(value) => Some(value),
        ValueRef::Real(value) => Some(value as i64),
        ValueRef::Text(value) => std::str::from_utf8(value).ok()?.parse().ok(),
        _ => None,
    }
}

fn get_bytes(value: Option<ValueRef<'_>>) -> Vec<u8> {
    match value {
        Some(ValueRef::Blob(value)) | Some(ValueRef::Text(value)) => value.to_vec(),
        Some(ValueRef::Integer(value)) => value.to_string().into_bytes(),
        Some(ValueRef::Real(value)) => value.to_string().into_bytes(),
        _ => Vec::new(),
    }
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn opaque_id(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    hex::encode(&digest[..16])
}

fn source_table_id(table: &str) -> String {
    hex::encode(Sha256::digest(table.as_bytes()))
}

fn detect_format(prefix: &[u8], extension: Option<&str>) -> String {
    if prefix.starts_with(b"\x07\x08V1\x08\x07") {
        return "wechat-dat-v1".to_string();
    }
    if prefix.starts_with(b"\x07\x08V2\x08\x07") {
        return "wechat-dat-v2".to_string();
    }
    if prefix.starts_with(b"\x02#!SILK_V3") || prefix.starts_with(b"#!SILK_V3") {
        return "tencent-silk-v3".to_string();
    }
    if prefix.starts_with(&[0xff, 0xd8, 0xff]) {
        return "jpeg".to_string();
    }
    if prefix.starts_with(&[0x89, b'P', b'N', b'G']) {
        return "png".to_string();
    }
    if prefix.starts_with(b"GIF8") {
        return "gif".to_string();
    }
    if prefix.len() >= 12 && prefix.starts_with(b"RIFF") && &prefix[8..12] == b"WEBP" {
        return "webp".to_string();
    }
    if prefix.len() >= 8 && &prefix[4..8] == b"ftyp" {
        return "mp4".to_string();
    }
    extension
        .filter(|value| !value.is_empty())
        .map(|value| format!("extension-{}", value.to_ascii_lowercase()))
        .unwrap_or_else(|| "unknown".to_string())
}

fn same_file_version(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
}

fn create_owner_only_directory(path: &Path) -> Result<(), RestoreError> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn write_owner_only_once(path: &Path, data: &[u8]) -> Result<(), RestoreError> {
    if path.is_file() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath(path.display().to_string()))?;
    create_owner_only_directory(parent)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}
