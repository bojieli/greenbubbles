use std::io::{self, BufRead};

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::RestoreError;

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct DatabasePassphrase([u8; 32]);

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct ReplicaKey([u8; 32]);

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

fn read_32_byte_secret(invalid: fn() -> RestoreError) -> Result<[u8; 32], RestoreError> {
    let mut input = String::new();
    io::stdin().lock().read_line(&mut input)?;
    let trimmed = input.trim();
    let mut decoded = if trimmed.len() == 64 && trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
        hex::decode(trimmed).map_err(|_| invalid())?
    } else {
        trimmed.as_bytes().to_vec()
    };
    if decoded.len() != 32 {
        decoded.zeroize();
        input.zeroize();
        return Err(invalid());
    }
    let mut value = [0u8; 32];
    value.copy_from_slice(&decoded);
    decoded.zeroize();
    input.zeroize();
    Ok(value)
}

fn invalid_database_passphrase() -> RestoreError {
    RestoreError::InvalidPassphrase
}

fn invalid_replica_key() -> RestoreError {
    RestoreError::InvalidReplicaKey
}
