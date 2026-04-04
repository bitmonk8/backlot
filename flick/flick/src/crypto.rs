use crate::error::CredentialError;
use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};

const NONCE_LEN: usize = 12;
const AUTH_TAG_LEN: usize = 16; // Poly1305 authentication tag
const PREFIX: &str = "enc3:";

pub fn encrypt(key: &[u8; 32], plaintext: &str, provider: &str) -> Result<String, CredentialError> {
    let cipher = ChaCha20Poly1305::new(key.into());
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext.as_bytes(),
                aad: provider.as_bytes(),
            },
        )
        .map_err(|_| CredentialError::EncryptionFailed)?;

    let mut combined = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);
    Ok(format!("{PREFIX}{}", hex::encode(combined)))
}

pub fn decrypt(key: &[u8; 32], value: &str, provider: &str) -> Result<String, CredentialError> {
    let hex_str = value
        .strip_prefix(PREFIX)
        .ok_or_else(|| CredentialError::InvalidFormat(format!("missing {PREFIX} prefix")))?;
    let combined =
        hex::decode(hex_str).map_err(|_| CredentialError::InvalidFormat("bad hex".into()))?;
    if combined.len() < NONCE_LEN + AUTH_TAG_LEN {
        return Err(CredentialError::InvalidFormat("too short".into()));
    }
    let (nonce_bytes, ciphertext) = combined.split_at(NONCE_LEN);
    let nonce = Nonce::from_slice(nonce_bytes);
    let cipher = ChaCha20Poly1305::new(key.into());
    let plaintext = cipher
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext,
                aad: provider.as_bytes(),
            },
        )
        .map_err(|_| CredentialError::DecryptionFailed(provider.to_string()))?;
    String::from_utf8(plaintext)
        .map_err(|_| CredentialError::DecryptionFailed(provider.to_string()))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_round_trip() {
        let key = [42u8; 32];
        let plaintext = "sk-test-api-key-12345";
        let encrypted = encrypt(&key, plaintext, "test").expect("encryption should succeed");
        assert!(encrypted.starts_with(PREFIX));
        let decrypted = decrypt(&key, &encrypted, "test").expect("decryption should succeed");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn decrypt_wrong_key_fails() {
        let key1 = [1u8; 32];
        let key2 = [2u8; 32];
        let encrypted = encrypt(&key1, "secret", "test").expect("encryption should succeed");
        let result = decrypt(&key2, &encrypted, "test");
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_bad_prefix_fails() {
        let key = [42u8; 32];
        let result = decrypt(&key, "notencrypted", "test");
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_too_short_fails() {
        let key = [42u8; 32];
        let result = decrypt(&key, "enc3:aabb", "test");
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_aad_mismatch_fails() {
        let key = [42u8; 32];
        let encrypted = encrypt(&key, "secret", "anthropic").expect("encrypt");
        let result = decrypt(&key, &encrypted, "openai");
        assert!(matches!(result, Err(CredentialError::DecryptionFailed(_))));
    }
}
