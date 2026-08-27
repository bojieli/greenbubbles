use std::io::{self, BufRead};

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::RestoreError;

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct DatabasePassphrase([u8; 32]);

impl DatabasePassphrase {
    pub fn from_bytes(value: [u8; 32]) -> Self {
        Self(value)
    }

    pub fn read_stdin() -> Result<Self, RestoreError> {
        let mut input = String::new();
        io::stdin().lock().read_line(&mut input)?;
        let trimmed = input.trim();
        let mut decoded = if trimmed.len() == 64 && trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
            hex::decode(trimmed).map_err(|_| RestoreError::InvalidPassphrase)?
        } else {
            trimmed.as_bytes().to_vec()
        };
        if decoded.len() != 32 {
            decoded.zeroize();
            input.zeroize();
            return Err(RestoreError::InvalidPassphrase);
        }
        let mut value = [0u8; 32];
        value.copy_from_slice(&decoded);
        decoded.zeroize();
        input.zeroize();
        Ok(Self(value))
    }

    pub fn expose_for_database_operation(&self) -> &[u8; 32] {
        &self.0
    }
}
