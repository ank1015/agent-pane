use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
};
use zeroize::Zeroizing;

pub const CURRENT_KEY_VERSION: i32 = 1;

#[derive(Clone)]
pub struct CredentialVault {
    key: Arc<Zeroizing<[u8; 32]>>,
}

impl std::fmt::Debug for CredentialVault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CredentialVault")
            .field("key", &"[REDACTED]")
            .finish()
    }
}

impl CredentialVault {
    pub fn from_base64(encoded: &str) -> Result<Self, VaultError> {
        let decoded = Zeroizing::new(
            STANDARD
                .decode(encoded.trim())
                .map_err(|_| VaultError::InvalidKeyEncoding)?,
        );
        let key = Zeroizing::new(
            decoded
                .as_slice()
                .try_into()
                .map_err(|_| VaultError::InvalidKeyLength)?,
        );
        Ok(Self { key: Arc::new(key) })
    }

    pub fn encrypt(&self, context: &str, value: &[u8]) -> Result<EncryptedCredential, VaultError> {
        let cipher = XChaCha20Poly1305::new(self.key.as_ref().as_ref().into());
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: value,
                    aad: context.as_bytes(),
                },
            )
            .map_err(|_| VaultError::EncryptionFailed)?;
        Ok(EncryptedCredential {
            ciphertext,
            nonce: nonce.to_vec(),
            key_version: CURRENT_KEY_VERSION,
        })
    }

    pub fn decrypt(
        &self,
        context: &str,
        ciphertext: &[u8],
        nonce: &[u8],
        key_version: i32,
    ) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        if key_version != CURRENT_KEY_VERSION {
            return Err(VaultError::UnsupportedKeyVersion(key_version));
        }
        let nonce: &[u8; 24] = nonce.try_into().map_err(|_| VaultError::InvalidNonce)?;
        let cipher = XChaCha20Poly1305::new(self.key.as_ref().as_ref().into());
        cipher
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad: context.as_bytes(),
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| VaultError::DecryptionFailed)
    }
}

pub struct EncryptedCredential {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub key_version: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("vault key must be valid base64")]
    InvalidKeyEncoding,
    #[error("vault key must decode to exactly 32 bytes")]
    InvalidKeyLength,
    #[error("credential encryption failed")]
    EncryptionFailed,
    #[error("credential decryption failed")]
    DecryptionFailed,
    #[error("credential nonce has an invalid length")]
    InvalidNonce,
    #[error("credential key version {0} is unsupported")]
    UnsupportedKeyVersion(i32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_encrypted_and_bound_to_the_account() {
        let vault = CredentialVault::from_base64(&STANDARD.encode([4_u8; 32])).unwrap();
        let encrypted = vault.encrypt("e2b-account:one", b"secret").unwrap();
        assert_ne!(encrypted.ciphertext, b"secret");
        assert_eq!(
            vault
                .decrypt(
                    "e2b-account:one",
                    &encrypted.ciphertext,
                    &encrypted.nonce,
                    encrypted.key_version,
                )
                .unwrap()
                .as_slice(),
            b"secret"
        );
        assert!(
            vault
                .decrypt(
                    "e2b-account:two",
                    &encrypted.ciphertext,
                    &encrypted.nonce,
                    encrypted.key_version,
                )
                .is_err()
        );
    }
}
