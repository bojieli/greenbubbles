use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::{Duration, Instant};

use wx_decrypt::error::DecryptError;
use wx_decrypt::kdf::derive_mac_key;
use wx_decrypt::page::decrypt_page;
use wx_decrypt::CryptoParams;
use zeroize::Zeroizing;

use crate::ProgressState;

const WAL_HEADER_SIZE: usize = 32;
const WAL_FRAME_HEADER_SIZE: usize = 24;
const WAL_MAGIC_BE: u32 = 0x377f_0682;
const WAL_MAGIC_LE: u32 = 0x377f_0683;
const MAX_PAGE_NUMBER: u32 = 1_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WalProgressStage {
    Scan,
    Apply,
}

#[derive(Clone, Copy)]
struct WalHeader {
    salt1: u32,
    salt2: u32,
}

#[derive(Clone, Copy)]
struct WalFrameHeader {
    page_number: u32,
    commit_size: u32,
    salt1: u32,
    salt2: u32,
}

impl WalHeader {
    fn parse(buffer: &[u8; WAL_HEADER_SIZE]) -> Result<Self, DecryptError> {
        let magic = u32::from_be_bytes(buffer[0..4].try_into().expect("fixed WAL magic"));
        if magic != WAL_MAGIC_BE && magic != WAL_MAGIC_LE {
            return Err(DecryptError::InvalidWalHeader {
                reason: format!("bad magic: 0x{magic:08x}"),
            });
        }
        Ok(Self {
            salt1: u32::from_be_bytes(buffer[16..20].try_into().expect("fixed WAL salt")),
            salt2: u32::from_be_bytes(buffer[20..24].try_into().expect("fixed WAL salt")),
        })
    }
}

impl WalFrameHeader {
    fn parse(buffer: &[u8; WAL_FRAME_HEADER_SIZE]) -> Self {
        Self {
            page_number: u32::from_be_bytes(
                buffer[0..4].try_into().expect("fixed WAL page number"),
            ),
            commit_size: u32::from_be_bytes(
                buffer[4..8].try_into().expect("fixed WAL commit size"),
            ),
            salt1: u32::from_be_bytes(buffer[8..12].try_into().expect("fixed WAL salt")),
            salt2: u32::from_be_bytes(buffer[12..16].try_into().expect("fixed WAL salt")),
        }
    }

    fn is_valid(self, wal: WalHeader) -> bool {
        self.page_number > 0
            && self.page_number <= MAX_PAGE_NUMBER
            && self.salt1 == wal.salt1
            && self.salt2 == wal.salt2
    }
}

/// Applies committed encrypted WAL frames while reporting the actual scan and
/// frame-processing positions. This mirrors SQLite's last-commit visibility
/// rule and keeps all cryptographic work inside GreenBubbles.
pub(crate) fn apply_encrypted_wal_with_progress<F>(
    wal_path: &Path,
    decrypted_database_path: &Path,
    encryption_key: &[u8; 32],
    salt: &[u8; 16],
    parameters: &CryptoParams,
    mut progress: F,
) -> Result<usize, DecryptError>
where
    F: FnMut(WalProgressStage, ProgressState, u64, u64, u64),
{
    let mut wal_file = open_regular_file(wal_path, false)?;
    let wal_length = wal_file.metadata()?.len();
    progress(
        WalProgressStage::Scan,
        ProgressState::Started,
        0,
        wal_length,
        0,
    );
    if wal_length < WAL_HEADER_SIZE as u64 {
        progress(
            WalProgressStage::Scan,
            ProgressState::Completed,
            wal_length,
            wal_length,
            0,
        );
        progress(
            WalProgressStage::Apply,
            ProgressState::Started,
            0,
            wal_length,
            0,
        );
        progress(
            WalProgressStage::Apply,
            ProgressState::Completed,
            wal_length,
            wal_length,
            0,
        );
        return Ok(0);
    }

    let mut header_buffer = [0_u8; WAL_HEADER_SIZE];
    wal_file.read_exact(&mut header_buffer)?;
    let wal_header = WalHeader::parse(&header_buffer)?;
    let frame_size = WAL_FRAME_HEADER_SIZE.saturating_add(parameters.page_size);
    let total_frames = (wal_length as usize).saturating_sub(WAL_HEADER_SIZE) / frame_size;
    let mut frame_header_buffer = [0_u8; WAL_FRAME_HEADER_SIZE];
    let mut last_commit = None;
    let mut scan_throttle = ProgressThrottle::new(wal_length);

    for frame_index in 0..total_frames {
        let header_offset = WAL_HEADER_SIZE.saturating_add(frame_index.saturating_mul(frame_size));
        wal_file.seek(SeekFrom::Start(header_offset as u64))?;
        wal_file.read_exact(&mut frame_header_buffer)?;
        let frame_header = WalFrameHeader::parse(&frame_header_buffer);
        if frame_header.is_valid(wal_header) && frame_header.commit_size > 0 {
            last_commit = Some(frame_index);
        }
        let completed = processed_wal_bytes(frame_index, frame_size, wal_length);
        if scan_throttle.should_emit(completed) {
            progress(
                WalProgressStage::Scan,
                ProgressState::Advanced,
                completed,
                wal_length,
                frame_index as u64 + 1,
            );
        }
    }
    progress(
        WalProgressStage::Scan,
        ProgressState::Completed,
        wal_length,
        wal_length,
        total_frames as u64,
    );
    progress(
        WalProgressStage::Apply,
        ProgressState::Started,
        0,
        wal_length,
        0,
    );

    let Some(last_commit) = last_commit else {
        progress(
            WalProgressStage::Apply,
            ProgressState::Completed,
            wal_length,
            wal_length,
            0,
        );
        return Ok(0);
    };

    let mac_key = Zeroizing::new(derive_mac_key(encryption_key, salt, parameters));
    let mut database_file = open_regular_file(decrypted_database_path, true)?;
    let mut page_buffer = vec![0_u8; parameters.page_size];
    let mut patched_frames = 0_u64;
    let mut apply_throttle = ProgressThrottle::new(wal_length);
    wal_file.seek(SeekFrom::Start(WAL_HEADER_SIZE as u64))?;

    for frame_index in 0..=last_commit {
        wal_file.read_exact(&mut frame_header_buffer)?;
        let frame_header = WalFrameHeader::parse(&frame_header_buffer);
        wal_file.read_exact(&mut page_buffer)?;
        if frame_header.is_valid(wal_header) && page_buffer.iter().any(|byte| *byte != 0) {
            let page_number = frame_header.page_number - 1;
            let decrypted = decrypt_page(
                &page_buffer,
                encryption_key,
                &mac_key,
                page_number,
                parameters,
            )?;
            let offset = page_number as u64 * parameters.page_size as u64;
            database_file.seek(SeekFrom::Start(offset))?;
            if page_number == 0 {
                database_file.write_all(b"SQLite format 3\0")?;
            }
            database_file.write_all(&decrypted)?;
            patched_frames = patched_frames.saturating_add(1);
        }
        let completed = processed_wal_bytes(frame_index, frame_size, wal_length);
        if apply_throttle.should_emit(completed) {
            progress(
                WalProgressStage::Apply,
                ProgressState::Advanced,
                completed,
                wal_length,
                patched_frames,
            );
        }
    }
    database_file.flush()?;
    progress(
        WalProgressStage::Apply,
        ProgressState::Completed,
        wal_length,
        wal_length,
        patched_frames,
    );
    Ok(patched_frames as usize)
}

fn open_regular_file(path: &Path, writable: bool) -> Result<File, DecryptError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(writable)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(DecryptError::InvalidWalHeader {
            reason: "WAL input or decrypted database is not a safe regular file".to_string(),
        });
    }
    Ok(file)
}

fn processed_wal_bytes(frame_index: usize, frame_size: usize, total: u64) -> u64 {
    (WAL_HEADER_SIZE as u64)
        .saturating_add((frame_index as u64 + 1).saturating_mul(frame_size as u64))
        .min(total)
}

struct ProgressThrottle {
    next_byte: u64,
    byte_increment: u64,
    last_report: Instant,
}

impl ProgressThrottle {
    fn new(total: u64) -> Self {
        let byte_increment = (total / 100).max(8 * 1024 * 1024).max(1);
        Self {
            next_byte: byte_increment,
            byte_increment,
            last_report: Instant::now(),
        }
    }

    fn should_emit(&mut self, completed: u64) -> bool {
        if completed < self.next_byte && self.last_report.elapsed() < Duration::from_millis(500) {
            return false;
        }
        self.next_byte = completed.saturating_add(self.byte_increment);
        self.last_report = Instant::now();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_throttle_reports_by_bytes_or_elapsed_time() {
        let mut throttle = ProgressThrottle::new(1_000_000_000);
        assert!(!throttle.should_emit(1));
        assert!(throttle.should_emit(10_000_000));
    }

    #[test]
    fn processed_bytes_never_exceed_a_trailing_partial_wal() {
        assert_eq!(processed_wal_bytes(9, 4_120, 41_200), 41_200);
    }
}
