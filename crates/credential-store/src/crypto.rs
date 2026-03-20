use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::RngCore;
use ring::hkdf;

use crate::error::{CredentialStoreError, Result};

/// Derive an AES-256 key from a master key, per-credential salt, and provider name.
/// Uses HKDF-SHA256 with info = "edgeclaw-token-v1:{provider}".
pub fn derive_key(master_key: &[u8; 32], salt: &[u8; 32], provider: &str) -> Result<[u8; 32]> {
    let hkdf_salt = hkdf::Salt::new(hkdf::HKDF_SHA256, salt);
    let prk = hkdf_salt.extract(master_key);
    let info = format!("edgeclaw-token-v1:{provider}");
    let info_bytes = [info.as_bytes()];
    let okm = prk
        .expand(&info_bytes, HkdfLen(32))
        .map_err(|_| CredentialStoreError::KeyDerivation("HKDF expand failed".into()))?;

    let mut key = [0u8; 32];
    okm.fill(&mut key)
        .map_err(|_| CredentialStoreError::KeyDerivation("HKDF fill failed".into()))?;
    Ok(key)
}

/// Encrypt plaintext with AES-256-GCM. Returns `[nonce(12) | ciphertext | tag(16)]`.
pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| CredentialStoreError::Encryption(e.to_string()))?;

    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| CredentialStoreError::Encryption(e.to_string()))?;

    let mut blob = Vec::with_capacity(12 + ciphertext.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);
    Ok(blob)
}

/// Decrypt a blob produced by `encrypt`. Expects `[nonce(12) | ciphertext | tag(16)]`.
pub fn decrypt(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < 12 + 16 {
        return Err(CredentialStoreError::Decryption(
            "ciphertext too short".into(),
        ));
    }

    let (nonce_bytes, ciphertext) = blob.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| CredentialStoreError::Decryption(e.to_string()))?;

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| CredentialStoreError::Decryption(e.to_string()))
}

/// Generate a random 256-bit salt.
pub fn generate_salt() -> [u8; 32] {
    let mut salt = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut salt);
    salt
}

/// Helper for HKDF output length.
struct HkdfLen(usize);

impl hkdf::KeyType for HkdfLen {
    fn len(&self) -> usize {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_master_key() -> [u8; 32] {
        [0xAA; 32]
    }

    fn test_salt() -> [u8; 32] {
        [0xBB; 32]
    }

    #[test]
    fn deterministic_key_derivation() {
        let mk = test_master_key();
        let salt = test_salt();
        let k1 = derive_key(&mk, &salt, "github").unwrap();
        let k2 = derive_key(&mk, &salt, "github").unwrap();
        assert_eq!(k1, k2);
    }

    #[test]
    fn different_providers_different_keys() {
        let mk = test_master_key();
        let salt = test_salt();
        let k1 = derive_key(&mk, &salt, "github").unwrap();
        let k2 = derive_key(&mk, &salt, "slack").unwrap();
        assert_ne!(k1, k2);
    }

    #[test]
    fn different_salts_different_keys() {
        let mk = test_master_key();
        let s1 = [0xBB; 32];
        let s2 = [0xCC; 32];
        let k1 = derive_key(&mk, &s1, "github").unwrap();
        let k2 = derive_key(&mk, &s2, "github").unwrap();
        assert_ne!(k1, k2);
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let key = derive_key(&test_master_key(), &test_salt(), "github").unwrap();
        let plaintext = b"my-secret-token-value";
        let blob = encrypt(&key, plaintext).unwrap();
        let decrypted = decrypt(&key, &blob).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let key = derive_key(&test_master_key(), &test_salt(), "github").unwrap();
        let mut blob = encrypt(&key, b"secret").unwrap();
        // Flip a byte in the ciphertext portion (after the 12-byte nonce)
        blob[14] ^= 0xFF;
        assert!(decrypt(&key, &blob).is_err());
    }

    #[test]
    fn tampered_nonce_fails() {
        let key = derive_key(&test_master_key(), &test_salt(), "github").unwrap();
        let mut blob = encrypt(&key, b"secret").unwrap();
        // Flip a byte in the nonce
        blob[0] ^= 0xFF;
        assert!(decrypt(&key, &blob).is_err());
    }

    #[test]
    fn fresh_nonce_per_encrypt() {
        let key = derive_key(&test_master_key(), &test_salt(), "github").unwrap();
        let b1 = encrypt(&key, b"same-plaintext").unwrap();
        let b2 = encrypt(&key, b"same-plaintext").unwrap();
        // The nonce (first 12 bytes) should differ
        assert_ne!(&b1[..12], &b2[..12]);
        // And thus the full blobs differ
        assert_ne!(b1, b2);
    }
}
