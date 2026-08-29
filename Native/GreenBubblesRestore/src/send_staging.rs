//! Single-use attachment staging with descriptor-level revalidation.
//!
//! An attachment's bytes never appear on screen, so the on-screen gate can
//! prove *which* file was staged but never that its contents are the approved
//! contents. That half of the verification happens here, off screen, and it is
//! the half that actually protects the bytes.
//!
//! The obvious implementation — hash the user's file, then hand its path to the
//! helper — has a time-of-check-to-time-of-use hole: the file can be replaced
//! between the hash and the moment WeChat reads it. This module closes it by
//! copying the approved file into a single-use, owner-only directory, hashing
//! *the copy*, re-opening that copy and confirming it is the same inode before
//! hashing it a second time, and putting only the staged path into the
//! capability. Replacing the user's original afterwards changes nothing,
//! because the original is never referenced again.
//!
//! The reviewed type set is an allow list, not a deny list: an extension nobody
//! reviewed is refused rather than guessed at, which is what keeps executables,
//! bundles, and media the adapter has no gate for out of the send path.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::action::ActionCapability;
use crate::archive::ensure_private_directory;
use crate::connector::DraftAttachment;
use crate::send_contract::{
    ActionAttachment, SendFailureCode, MAXIMUM_ATTACHMENT_BYTES, MAXIMUM_DISPLAY_FILE_NAME_BYTES,
};
use crate::RestoreError;

/// Bytes copied per read while staging and hashing.
const STAGING_CHUNK_BYTES: usize = 1024 * 1024;

/// One reviewed attachment type: an extension, its uniform type identifier, and
/// whether the client treats it as an image.
struct ReviewedType {
    extension: &'static str,
    uniform_type_identifier: &'static str,
    image: bool,
}

/// The reviewed set. Deliberately narrow. Video and audio are absent because
/// the client would send them as media messages, which is a third semantic this
/// adapter has no gate for; executables and bundles are absent because nothing
/// about them has been reviewed.
const REVIEWED_TYPES: &[ReviewedType] = &[
    ReviewedType {
        extension: "png",
        uniform_type_identifier: "public.png",
        image: true,
    },
    ReviewedType {
        extension: "jpg",
        uniform_type_identifier: "public.jpeg",
        image: true,
    },
    ReviewedType {
        extension: "jpeg",
        uniform_type_identifier: "public.jpeg",
        image: true,
    },
    ReviewedType {
        extension: "gif",
        uniform_type_identifier: "com.compuserve.gif",
        image: true,
    },
    ReviewedType {
        extension: "heic",
        uniform_type_identifier: "public.heic",
        image: true,
    },
    ReviewedType {
        extension: "webp",
        uniform_type_identifier: "org.webmproject.webp",
        image: true,
    },
    ReviewedType {
        extension: "tiff",
        uniform_type_identifier: "public.tiff",
        image: true,
    },
    ReviewedType {
        extension: "bmp",
        uniform_type_identifier: "com.microsoft.bmp",
        image: true,
    },
    ReviewedType {
        extension: "pdf",
        uniform_type_identifier: "com.adobe.pdf",
        image: false,
    },
    ReviewedType {
        extension: "txt",
        uniform_type_identifier: "public.plain-text",
        image: false,
    },
    ReviewedType {
        extension: "md",
        uniform_type_identifier: "net.daringfireball.markdown",
        image: false,
    },
    ReviewedType {
        extension: "csv",
        uniform_type_identifier: "public.comma-separated-values-text",
        image: false,
    },
    ReviewedType {
        extension: "json",
        uniform_type_identifier: "public.json",
        image: false,
    },
    ReviewedType {
        extension: "log",
        uniform_type_identifier: "public.plain-text",
        image: false,
    },
    ReviewedType {
        extension: "rtf",
        uniform_type_identifier: "public.rtf",
        image: false,
    },
    ReviewedType {
        extension: "zip",
        uniform_type_identifier: "public.zip-archive",
        image: false,
    },
    ReviewedType {
        extension: "docx",
        uniform_type_identifier: "org.openxmlformats.wordprocessingml.document",
        image: false,
    },
    ReviewedType {
        extension: "xlsx",
        uniform_type_identifier: "org.openxmlformats.spreadsheetml.sheet",
        image: false,
    },
    ReviewedType {
        extension: "pptx",
        uniform_type_identifier: "org.openxmlformats.presentationml.presentation",
        image: false,
    },
];

/// The reviewed type for a display file name, if any.
fn reviewed_type(display_file_name: &str) -> Option<&'static ReviewedType> {
    let extension = display_file_name.rsplit_once('.')?.1.to_ascii_lowercase();
    REVIEWED_TYPES
        .iter()
        .find(|reviewed| reviewed.extension == extension)
}

/// Whether a capability may carry a file of this name, and its type identifier.
pub fn reviewed_uniform_type_identifier(
    capability: ActionCapability,
    display_file_name: &str,
) -> Result<String, SendFailureCode> {
    let reviewed =
        reviewed_type(display_file_name).ok_or(SendFailureCode::UnsupportedAttachmentType)?;
    match capability {
        // An image send must actually be an image; a file send may carry an
        // image, which is how "send the original, uncompressed" is expressed.
        ActionCapability::ImageSend if !reviewed.image => {
            Err(SendFailureCode::UnsupportedAttachmentType)
        }
        ActionCapability::ImageSend | ActionCapability::FileSend => {
            Ok(reviewed.uniform_type_identifier.to_string())
        }
        _ => Err(SendFailureCode::AttachmentInvalid),
    }
}

/// A staged attachment, ready to be bound into a capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StagedAttachment {
    pub staging_directory: PathBuf,
    pub staged_path: PathBuf,
    pub display_file_name: String,
    pub byte_count: u64,
    pub sha256: String,
    pub uniform_type_identifier: String,
    /// True when the client transmits these exact bytes. False for an image
    /// send, where the client re-encodes and the recipient receives a
    /// derivative, which the audit trail must never call a byte-for-byte match.
    pub bytes_preserved_in_transit: bool,
}

impl StagedAttachment {
    /// The bound form handed to the helper.
    pub fn as_action_attachment(&self) -> ActionAttachment {
        ActionAttachment {
            staging_directory: self.staging_directory.display().to_string(),
            staged_path: self.staged_path.display().to_string(),
            display_file_name: self.display_file_name.clone(),
            byte_count: self.byte_count,
            sha256: self.sha256.clone(),
            uniform_type_identifier: self.uniform_type_identifier.clone(),
        }
    }
}

/// Copies the approved file into a single-use staging directory and proves the
/// staged copy matches the digest the draft approved.
///
/// `source` is the file the owner named. It is read once and never referenced
/// again; everything downstream sees only the staged copy.
pub fn stage_attachment(
    source: &Path,
    staging_root: &Path,
    expected: &DraftAttachment,
    capability: ActionCapability,
) -> Result<StagedAttachment, RestoreError> {
    let display_file_name = expected.display_file_name.clone();
    if display_file_name.is_empty()
        || display_file_name.len() > MAXIMUM_DISPLAY_FILE_NAME_BYTES
        || display_file_name.contains('/')
        || display_file_name.contains('\0')
        || matches!(display_file_name.as_str(), "." | "..")
    {
        return Err(failure(SendFailureCode::AttachmentInvalid));
    }
    let uniform_type_identifier =
        reviewed_uniform_type_identifier(capability, &display_file_name).map_err(failure)?;

    let mut source_file = open_source(source)?;
    let source_length = source_file.metadata()?.len();
    if source_length == 0 || source_length > MAXIMUM_ATTACHMENT_BYTES {
        return Err(failure(SendFailureCode::AttachmentInvalid));
    }

    let staging_directory = create_staging_directory(staging_root)?;
    let staged_path = staging_directory.join(&display_file_name);
    let staged = (|| -> Result<StagedAttachment, RestoreError> {
        let mut staged_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&staged_path)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; STAGING_CHUNK_BYTES];
        let mut copied = 0_u64;
        loop {
            let read = source_file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            copied = copied.saturating_add(read as u64);
            if copied > MAXIMUM_ATTACHMENT_BYTES {
                return Err(failure(SendFailureCode::AttachmentInvalid));
            }
            hasher.update(&buffer[..read]);
            staged_file.write_all(&buffer[..read])?;
        }
        staged_file.sync_all()?;
        let written_identity = staged_file.metadata()?;
        drop(staged_file);
        File::open(&staging_directory)?.sync_all()?;
        let written_digest = hex::encode(hasher.finalize());

        // Re-open the staged copy and confirm it is still the very inode that
        // was just written before hashing it a second time. This is what makes
        // "the revalidated object and the staged object are the same object" a
        // checked fact rather than an assumption.
        let (verified_digest, verified_length) =
            reread_and_digest(&staged_path, &written_identity)?;
        if verified_digest != written_digest || verified_length != copied {
            return Err(failure(SendFailureCode::AttachmentStagingFailed));
        }
        if verified_digest != expected.sha256 {
            return Err(failure(SendFailureCode::AttachmentDigestMismatch));
        }
        if expected.byte_count.is_some_and(|count| count != copied) {
            return Err(failure(SendFailureCode::AttachmentDigestMismatch));
        }
        Ok(StagedAttachment {
            staging_directory: staging_directory.clone(),
            staged_path: staged_path.clone(),
            display_file_name,
            byte_count: copied,
            sha256: verified_digest,
            uniform_type_identifier,
            bytes_preserved_in_transit: capability.preserves_bytes(),
        })
    })();
    if staged.is_err() {
        // Never leave a partially staged file behind for a later run to find.
        discard_staging_directory(&staging_directory, staging_root);
    }
    staged
}

/// Removes a staging directory and the single file it holds. Refuses to touch
/// anything that is not a direct child of the staging root, so a malformed or
/// hostile capability cannot turn cleanup into deletion elsewhere.
pub fn discard_staging_directory(staging_directory: &Path, staging_root: &Path) {
    if staging_directory.parent() != Some(staging_root) {
        return;
    }
    let Ok(metadata) = fs::symlink_metadata(staging_directory) else {
        return;
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return;
    }
    if let Ok(entries) = fs::read_dir(staging_directory) {
        for entry in entries.flatten() {
            let path = entry.path();
            if fs::symlink_metadata(&path).is_ok_and(|value| value.is_file()) {
                let _ = fs::remove_file(path);
            }
        }
    }
    let _ = fs::remove_dir(staging_directory);
}

/// Opens the owner's file for reading, refusing anything that is not a plain,
/// non-symlink, current-user file that no other account can rewrite.
fn open_source(source: &Path) -> Result<File, RestoreError> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(RestoreError::Integrity(
            "an attachment source must be a current-user regular file that only its owner can write"
                .to_string(),
        ));
    }
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(source)?)
}

/// Creates the single-use directory that will hold exactly one staged file.
fn create_staging_directory(staging_root: &Path) -> Result<PathBuf, RestoreError> {
    if !staging_root.try_exists()? {
        fs::create_dir_all(staging_root)?;
        fs::set_permissions(staging_root, fs::Permissions::from_mode(0o700))?;
    }
    ensure_private_directory(staging_root)?;
    let mut name = [0_u8; 16];
    getrandom::fill(&mut name)
        .map_err(|_| RestoreError::Integrity("the system refused random bytes".to_string()))?;
    let directory = staging_root.join(hex::encode(name));
    // `create_dir` fails if the path exists, so the directory is provably new.
    fs::create_dir(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    Ok(directory)
}

/// Re-opens a staged file, confirms its identity, and digests it again.
fn reread_and_digest(
    staged_path: &Path,
    written: &fs::Metadata,
) -> Result<(String, u64), RestoreError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(staged_path)?;
    let metadata = file.metadata()?;
    if metadata.dev() != written.dev()
        || metadata.ino() != written.ino()
        || metadata.nlink() != 1
        || metadata.len() != written.len()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(failure(SendFailureCode::AttachmentStagingFailed));
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; STAGING_CHUNK_BYTES];
    let mut length = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        length = length.saturating_add(read as u64);
        hasher.update(&buffer[..read]);
    }
    Ok((hex::encode(hasher.finalize()), length))
}

fn failure(code: SendFailureCode) -> RestoreError {
    RestoreError::Integrity(format!(
        "attachment staging refused the request: {} ({})",
        serde_json::to_string(&code).unwrap_or_else(|_| "\"unknown\"".to_string()),
        code.operator_action()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::DraftAttachment;
    use crate::model::{ArtifactKind, ArtifactRole};
    use tempfile::TempDir;

    fn private_directory() -> TempDir {
        let directory = TempDir::new().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    fn source(directory: &Path, name: &str, contents: &[u8]) -> PathBuf {
        let path = directory.join(name);
        fs::write(&path, contents).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    }

    fn expectation(name: &str, contents: &[u8]) -> DraftAttachment {
        DraftAttachment {
            artifact_id: "artifact".to_string(),
            kind: ArtifactKind::Document,
            role: ArtifactRole::FilePayload,
            digest_kind: "sourceSha256".to_string(),
            sha256: hex::encode(Sha256::digest(contents)),
            byte_count: Some(contents.len() as u64),
            display_file_name: name.to_string(),
        }
    }

    #[test]
    fn staging_copies_the_file_and_proves_the_copy_matches_the_approved_digest() {
        let root = private_directory();
        let contents = b"quarterly numbers".repeat(64);
        let path = source(root.path(), "quarterly.pdf", &contents);
        let staged = stage_attachment(
            &path,
            &root.path().join("staging"),
            &expectation("quarterly.pdf", &contents),
            ActionCapability::FileSend,
        )
        .unwrap();
        assert_eq!(staged.sha256, hex::encode(Sha256::digest(&contents)));
        assert_eq!(staged.byte_count, contents.len() as u64);
        assert_eq!(staged.uniform_type_identifier, "com.adobe.pdf");
        assert!(staged.bytes_preserved_in_transit);
        assert_eq!(fs::read(&staged.staged_path).unwrap(), contents);
        assert_eq!(
            fs::metadata(&staged.staged_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        assert_eq!(
            fs::metadata(&staged.staging_directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        assert_eq!(
            staged.staged_path.parent(),
            Some(staged.staging_directory.as_path())
        );
    }

    #[test]
    fn replacing_the_source_after_staging_cannot_change_what_is_sent() {
        let root = private_directory();
        let approved = b"the approved bytes".to_vec();
        let path = source(root.path(), "note.txt", &approved);
        let staged = stage_attachment(
            &path,
            &root.path().join("staging"),
            &expectation("note.txt", &approved),
            ActionCapability::FileSend,
        )
        .unwrap();
        // The classic time-of-check-to-time-of-use swap.
        fs::write(&path, b"something else entirely").unwrap();
        assert_eq!(fs::read(&staged.staged_path).unwrap(), approved);
        assert_eq!(staged.sha256, hex::encode(Sha256::digest(&approved)));
    }

    #[test]
    fn a_file_that_no_longer_matches_the_draft_is_refused_and_leaves_nothing_behind() {
        let root = private_directory();
        let staging = root.path().join("staging");
        let path = source(root.path(), "note.txt", b"current contents");
        let error = stage_attachment(
            &path,
            &staging,
            &expectation("note.txt", b"what the draft approved"),
            ActionCapability::FileSend,
        )
        .unwrap_err();
        assert!(error.to_string().contains("attachmentDigestMismatch"));
        let leftovers = fs::read_dir(&staging).unwrap().count();
        assert_eq!(
            leftovers, 0,
            "a refused staging attempt left a directory behind"
        );
    }

    #[test]
    fn only_reviewed_types_may_be_staged_and_images_must_really_be_images() {
        let root = private_directory();
        let staging = root.path().join("staging");
        for (name, capability, permitted) in [
            ("photo.png", ActionCapability::ImageSend, true),
            ("photo.png", ActionCapability::FileSend, true),
            ("report.pdf", ActionCapability::ImageSend, false),
            ("report.pdf", ActionCapability::FileSend, true),
            ("clip.mp4", ActionCapability::FileSend, false),
            ("installer.dmg", ActionCapability::FileSend, false),
            ("script.sh", ActionCapability::FileSend, false),
            ("nodots", ActionCapability::FileSend, false),
        ] {
            let contents = b"payload".to_vec();
            let path = source(root.path(), name, &contents);
            let result =
                stage_attachment(&path, &staging, &expectation(name, &contents), capability);
            assert_eq!(result.is_ok(), permitted, "{name} under {capability:?}");
            if let Ok(staged) = result {
                discard_staging_directory(&staged.staging_directory, &staging);
            }
        }
    }

    #[test]
    fn an_image_send_records_that_the_recipient_gets_a_derivative() {
        let root = private_directory();
        let contents = b"not really a png".to_vec();
        let path = source(root.path(), "photo.png", &contents);
        let staged = stage_attachment(
            &path,
            &root.path().join("staging"),
            &expectation("photo.png", &contents),
            ActionCapability::ImageSend,
        )
        .unwrap();
        assert!(!staged.bytes_preserved_in_transit);
        assert_eq!(staged.uniform_type_identifier, "public.png");
    }

    #[test]
    fn a_symlinked_or_group_writable_source_is_refused() {
        let root = private_directory();
        let staging = root.path().join("staging");
        let contents = b"payload".to_vec();
        let real = source(root.path(), "real.txt", &contents);
        let link = root.path().join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(stage_attachment(
            &link,
            &staging,
            &expectation("link.txt", &contents),
            ActionCapability::FileSend
        )
        .is_err());
        let loose = source(root.path(), "loose.txt", &contents);
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(stage_attachment(
            &loose,
            &staging,
            &expectation("loose.txt", &contents),
            ActionCapability::FileSend
        )
        .is_err());
    }

    #[test]
    fn discarding_refuses_to_delete_outside_the_staging_root() {
        let root = private_directory();
        let staging = root.path().join("staging");
        fs::create_dir_all(&staging).unwrap();
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
        let elsewhere = root.path().join("precious");
        fs::create_dir(&elsewhere).unwrap();
        fs::write(elsewhere.join("keep.txt"), b"keep").unwrap();
        discard_staging_directory(&elsewhere, &staging);
        assert!(elsewhere.join("keep.txt").exists());
    }

    #[test]
    fn a_staged_directory_is_removed_when_it_is_discarded() {
        let root = private_directory();
        let staging = root.path().join("staging");
        let contents = b"payload".to_vec();
        let path = source(root.path(), "note.txt", &contents);
        let staged = stage_attachment(
            &path,
            &staging,
            &expectation("note.txt", &contents),
            ActionCapability::FileSend,
        )
        .unwrap();
        discard_staging_directory(&staged.staging_directory, &staging);
        assert!(!staged.staging_directory.exists());
        assert_eq!(fs::read_dir(&staging).unwrap().count(), 0);
    }
}
