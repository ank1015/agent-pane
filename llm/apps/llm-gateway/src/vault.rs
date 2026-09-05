use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
};
use uuid::Uuid;
use zeroize::Zeroizing;

pub const CURRENT_ENCRYPTION_KEY_VERSION: i32 = 1;

#[derive(Clone)]
pub struct Vault {
    key: Arc<Zeroizing<[u8; 32]>>,
}

impl Vault {
    pub fn from_base64(encoded_key: &str) -> Result<Self, VaultError> {
        let decoded = Zeroizing::new(
            STANDARD
                .decode(encoded_key.trim())
                .map_err(|_| VaultError::InvalidMasterKeyEncoding)?,
        );
        let key = Zeroizing::new(
            decoded
                .as_slice()
                .try_into()
                .map_err(|_| VaultError::InvalidMasterKeyLength)?,
        );
        Ok(Self { key: Arc::new(key) })
    }

    pub fn encrypt(
        &self,
        account_id: Uuid,
        provider: &str,
        plaintext: &[u8],
    ) -> Result<EncryptedPayload, VaultError> {
        let cipher = XChaCha20Poly1305::new(self.key.as_ref().as_ref().into());
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let aad = associated_data(account_id, provider);
        let encrypted_payload = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| VaultError::EncryptionFailed)?;
        Ok(EncryptedPayload {
            encrypted_payload,
            nonce: nonce.to_vec(),
            encryption_key_version: CURRENT_ENCRYPTION_KEY_VERSION,
        })
    }

    pub fn decrypt(
        &self,
        account_id: Uuid,
        provider: &str,
        encrypted_payload: &[u8],
        nonce: &[u8],
        encryption_key_version: i32,
    ) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        if encryption_key_version != CURRENT_ENCRYPTION_KEY_VERSION {
            return Err(VaultError::UnsupportedKeyVersion(encryption_key_version));
        }
        let nonce: &[u8; 24] = nonce.try_into().map_err(|_| VaultError::InvalidNonce)?;
        let cipher = XChaCha20Poly1305::new(self.key.as_ref().as_ref().into());
        let aad = associated_data(account_id, provider);
        cipher
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: encrypted_payload,
                    aad: aad.as_bytes(),
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| VaultError::DecryptionFailed)
    }
}

fn associated_data(account_id: Uuid, provider: &str) -> String {
    format!("llm-gateway:v1:{account_id}:{provider}")
}

pub struct EncryptedPayload {
    pub encrypted_payload: Vec<u8>,
    pub nonce: Vec<u8>,
    pub encryption_key_version: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("LLM_GATEWAY_VAULT_KEY must be valid base64")]
    InvalidMasterKeyEncoding,
    #[error("LLM_GATEWAY_VAULT_KEY must decode to exactly 32 bytes")]
    InvalidMasterKeyLength,
    #[error("vault encryption failed")]
    EncryptionFailed,
    #[error("vault decryption failed; the master key or encrypted record is incorrect")]
    DecryptionFailed,
    #[error("vault nonce has an invalid length")]
    InvalidNonce,
    #[error("vault encryption key version {0} is not supported")]
    UnsupportedKeyVersion(i32),
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use uuid::Uuid;

    use super::Vault;

    #[test]
    fn ciphertext_is_bound_to_account_and_provider() {
        let vault = Vault::from_base64(&STANDARD.encode([7_u8; 32])).unwrap();
        let account_id = Uuid::now_v7();
        let encrypted = vault.encrypt(account_id, "openai", b"secret").unwrap();
        let plaintext = vault
            .decrypt(
                account_id,
                "openai",
                &encrypted.encrypted_payload,
                &encrypted.nonce,
                encrypted.encryption_key_version,
            )
            .unwrap();
        assert_eq!(plaintext.as_slice(), b"secret");
        assert!(
            vault
                .decrypt(
                    account_id,
                    "fireworks",
                    &encrypted.encrypted_payload,
                    &encrypted.nonce,
                    encrypted.encryption_key_version,
                )
                .is_err()
        );
    }
}
