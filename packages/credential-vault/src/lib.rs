use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
};
use zeroize::Zeroizing;

pub const CURRENT_ENCRYPTION_KEY_VERSION: i32 = 1;

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
    pub fn from_base64(encoded_key: &str) -> Result<Self, VaultError> {
        let decoded = Zeroizing::new(
            STANDARD
                .decode(encoded_key.trim())
                .map_err(|_| VaultError::InvalidMasterKeyEncoding)?,
        );
        let key: Zeroizing<[u8; 32]> = Zeroizing::new(
            decoded
                .as_slice()
                .try_into()
                .map_err(|_| VaultError::InvalidMasterKeyLength)?,
        );
        Ok(Self { key: Arc::new(key) })
    }

    pub fn encrypt(&self, context: &str, plaintext: &[u8]) -> Result<EncryptedSecret, VaultError> {
        let cipher = XChaCha20Poly1305::new(self.key.as_ref().as_ref().into());
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let encrypted_payload = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: context.as_bytes(),
                },
            )
            .map_err(|_| VaultError::EncryptionFailed)?;
        Ok(EncryptedSecret {
            encrypted_payload,
            nonce: nonce.to_vec(),
            encryption_key_version: CURRENT_ENCRYPTION_KEY_VERSION,
        })
    }

    pub fn decrypt(
        &self,
        context: &str,
        encrypted_payload: &[u8],
        nonce: &[u8],
        encryption_key_version: i32,
    ) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        if encryption_key_version != CURRENT_ENCRYPTION_KEY_VERSION {
            return Err(VaultError::UnsupportedKeyVersion(encryption_key_version));
        }
        let nonce: &[u8; 24] = nonce.try_into().map_err(|_| VaultError::InvalidNonce)?;
        let cipher = XChaCha20Poly1305::new(self.key.as_ref().as_ref().into());
        cipher
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: encrypted_payload,
                    aad: context.as_bytes(),
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| VaultError::DecryptionFailed)
    }
}

pub struct EncryptedSecret {
    pub encrypted_payload: Vec<u8>,
    pub nonce: Vec<u8>,
    pub encryption_key_version: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("vault key must be valid base64")]
    InvalidMasterKeyEncoding,
    #[error("vault key must decode to exactly 32 bytes")]
    InvalidMasterKeyLength,
    #[error("vault encryption failed")]
    EncryptionFailed,
    #[error("vault decryption failed; the key, context, or encrypted record is incorrect")]
    DecryptionFailed,
    #[error("vault nonce has an invalid length")]
    InvalidNonce,
    #[error("vault encryption key version {0} is not supported")]
    UnsupportedKeyVersion(i32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ciphertext_is_bound_to_its_context() {
        let vault = CredentialVault::from_base64(&STANDARD.encode([7_u8; 32])).expect("vault");
        let encrypted = vault
            .encrypt("account:e2b:one", b"secret")
            .expect("encrypt");
        assert_eq!(
            vault
                .decrypt(
                    "account:e2b:one",
                    &encrypted.encrypted_payload,
                    &encrypted.nonce,
                    encrypted.encryption_key_version,
                )
                .expect("decrypt")
                .as_slice(),
            b"secret"
        );
        assert!(
            vault
                .decrypt(
                    "account:e2b:two",
                    &encrypted.encrypted_payload,
                    &encrypted.nonce,
                    encrypted.encryption_key_version,
                )
                .is_err()
        );
    }

    #[test]
    fn debug_output_never_contains_the_key() {
        let encoded = STANDARD.encode([9_u8; 32]);
        let vault = CredentialVault::from_base64(&encoded).expect("vault");
        let debug = format!("{vault:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(&encoded));
    }
}
