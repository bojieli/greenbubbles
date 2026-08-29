use std::io::{self, BufRead, Read};

use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::RestoreError;

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct DatabasePassphrase([u8; 32]);

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct ReplicaKey([u8; 32]);

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SnapshotKey([u8; 32]);

// A hexadecimal secret followed by CRLF is the largest accepted input line.
const MAXIMUM_SECRET_LINE_BYTES: usize = 66;

impl DatabasePassphrase {
    pub fn from_bytes(value: [u8; 32]) -> Self {
        Self(value)
    }

    pub fn read_stdin() -> Result<Self, RestoreError> {
        read_32_byte_secret(invalid_database_passphrase).map(Self)
    }

    pub fn expose_for_database_operation(&self) -> &[u8; 32] {
        &self.0
    }
}

impl ReplicaKey {
    pub fn from_bytes(value: [u8; 32]) -> Self {
        Self(value)
    }

    pub fn read_stdin() -> Result<Self, RestoreError> {
        read_32_byte_secret(invalid_replica_key).map(Self)
    }

    pub(crate) fn expose_for_replica_operation(&self) -> &[u8; 32] {
        &self.0
    }
}

impl SnapshotKey {
    pub fn from_bytes(value: [u8; 32]) -> Self {
        Self(value)
    }

    pub fn read_stdin() -> Result<Self, RestoreError> {
        read_32_byte_secret(invalid_snapshot_key).map(Self)
    }

    pub fn expose_for_snapshot_operation(&self) -> &[u8; 32] {
        &self.0
    }
}

fn read_32_byte_secret(invalid: fn() -> RestoreError) -> Result<[u8; 32], RestoreError> {
    let mut input = Zeroizing::new(Vec::with_capacity(MAXIMUM_SECRET_LINE_BYTES + 1));
    io::stdin()
        .lock()
        .take((MAXIMUM_SECRET_LINE_BYTES + 1) as u64)
        .read_until(b'\n', &mut input)?;
    decode_32_byte_secret_line(&input, invalid)
}

fn decode_32_byte_secret_line(
    input: &[u8],
    invalid: fn() -> RestoreError,
) -> Result<[u8; 32], RestoreError> {
    if input.len() > MAXIMUM_SECRET_LINE_BYTES {
        return Err(invalid());
    }
    let text = std::str::from_utf8(input).map_err(|_| invalid())?;
    let trimmed = text.trim();
    let mut decoded = if trimmed.len() == 64 && trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
        hex::decode(trimmed).map_err(|_| invalid())?
    } else {
        trimmed.as_bytes().to_vec()
    };
    if decoded.len() != 32 {
        decoded.zeroize();
        return Err(invalid());
    }
    let mut value = [0u8; 32];
    value.copy_from_slice(&decoded);
    decoded.zeroize();
    Ok(value)
}

fn invalid_database_passphrase() -> RestoreError {
    RestoreError::InvalidPassphrase
}

fn invalid_replica_key() -> RestoreError {
    RestoreError::InvalidReplicaKey
}

fn invalid_snapshot_key() -> RestoreError {
    RestoreError::InvalidSnapshotKey
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_bounded_raw_and_hex_secrets() {
        assert_eq!(
            decode_32_byte_secret_line(b"12345678901234567890123456789012\n", invalid_replica_key)
                .unwrap(),
            *b"12345678901234567890123456789012"
        );
        let encoded = format!("{}\r\n", "31".repeat(32));
        assert_eq!(
            decode_32_byte_secret_line(encoded.as_bytes(), invalid_replica_key).unwrap(),
            [0x31; 32]
        );
    }

    #[test]
    fn rejects_an_oversized_or_non_utf8_secret_line() {
        assert!(matches!(
            decode_32_byte_secret_line(
                &vec![b'1'; MAXIMUM_SECRET_LINE_BYTES + 1],
                invalid_replica_key
            ),
            Err(RestoreError::InvalidReplicaKey)
        ));
        assert!(matches!(
            decode_32_byte_secret_line(&[0xff; 32], invalid_database_passphrase),
            Err(RestoreError::InvalidPassphrase)
        ));
    }
}
