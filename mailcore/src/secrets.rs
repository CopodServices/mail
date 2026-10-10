use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, Key, Nonce};
use anyhow::{Context, Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

/// AES-256-GCM encryption for stored DKIM private keys. The key is supplied as
/// base64 (URL-safe or standard) and must decode to 32 bytes.
#[derive(Clone)]
pub struct SecretBox {
    cipher: Aes256Gcm,
}

impl SecretBox {
    #[allow(deprecated)]
    pub fn new(key_base64: &str) -> Result<Self> {
        let key_bytes = URL_SAFE_NO_PAD
            .decode(key_base64)
            .or_else(|_| STANDARD.decode(key_base64))
            .context("ENCRYPTION_KEY must be base64")?;
        if key_bytes.len() != 32 {
            return Err(anyhow!("ENCRYPTION_KEY must decode to 32 bytes"));
        }
        let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
        Ok(Self {
            cipher: Aes256Gcm::new(key),
        })
    }

    pub fn encrypt_bytes(&self, plaintext: &[u8]) -> Result<String> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = self
            .cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| anyhow!("encryption failed"))?;
        let mut combined = nonce.to_vec();
        combined.extend_from_slice(&ciphertext);
        Ok(STANDARD.encode(combined))
    }

    pub fn decrypt_bytes(&self, encoded: &str) -> Result<Vec<u8>> {
        let combined = STANDARD
            .decode(encoded)
            .context("stored secret is not valid base64")?;
        if combined.len() < 12 {
            return Err(anyhow!("stored secret is truncated"));
        }
        let (nonce_bytes, ciphertext) = combined.split_at(12);
        #[allow(deprecated)]
        let nonce = Nonce::from_slice(nonce_bytes);
        self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| anyhow!("decryption failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> String {
        URL_SAFE_NO_PAD.encode([7u8; 32])
    }

    #[test]
    fn round_trips_bytes() {
        let store = SecretBox::new(&key()).unwrap();
        let ciphertext = store.encrypt_bytes(b"der-bytes").unwrap();
        assert_eq!(store.decrypt_bytes(&ciphertext).unwrap(), b"der-bytes");
    }

    #[test]
    fn rejects_short_keys() {
        let short = URL_SAFE_NO_PAD.encode([1u8; 16]);
        assert!(SecretBox::new(&short).is_err());
    }
}
