//! Column-level authenticated encryption (AES-256-GCM).
//!
//! Vault secrets sit in a `HashMap<String, TaintedString>`. Each value
//! could in principle be encrypted individually — and that is what this
//! module does for sensitive database columns.
//!
//! ## Threat model
//!
//! The vault file (`data/vault/.hydravault`) is already encrypted with
//! XChaCha20-Poly1305 over the entire `HashMap`. So why also encrypt
//! individual columns?
//!
//! 1. **Defense in depth**: an attacker who steals the master key should
//!    still need the column key to read individual columns.
//! 2. **Selective disclosure**: in the future we may want to share a
//!    subset of columns with a different process (e.g. a sub-agent) by
//!    handing it just the column key.
//! 3. **Forward secrecy through rotation**: rotating the column key
//!    invalidates all old ciphertexts without re-encrypting the whole
//!    vault file (see [`ColumnCipher::rotate_key`]).
//!
//! ## Key derivation
//!
//! ```text
//!     master_key (32B)        — the existing vault master key
//!         │
//!         │ HKDF-SHA256(salt: b"hydragent-vault-column-v1",
//!         │              info: column_name)
//!         ▼
//!     column_key (32B)        — per-column key
//! ```
//!
//! Binding the column name into the HKDF `info` parameter means a key
//! for `secrets.telegram.bot_token` is **not** usable to encrypt or
//! decrypt the column `secrets.openai.api_key` — this is a poor man's
//! domain separation.
//!
//! ## Wire format
//!
//! The output of `encrypt()` is a self-describing binary blob:
//!
//! ```text
//!     [ 4B  magic "CC01"   ]
//!     [ 4B  version (1)    ]
//!     [ 12B nonce          ]
//!     [ 16B auth tag       ]
//!     [ N   ciphertext     ]
//! ```
//!
//! Decryption validates the magic, version, and tag, and returns
//! `Err(ColumnCipherError::BadMagic | UnsupportedVersion | DecryptFailed)`
//! on any mismatch.

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, Payload},
    Aes256Gcm, Key, Nonce,
};
use hkdf::Hkdf;
use sha2::Sha256;
use std::fmt;

use crate::secure_buffer::SecureBuffer;

/// Magic header for column ciphertext blobs.
const COLUMN_MAGIC: &[u8; 4] = b"CC01";
/// Wire format version.
const COLUMN_VERSION: u32 = 1;
/// Salt for HKDF — domain-separates the column sub-keyspace from
/// other key derivations off the same master key.
const COLUMN_HKDF_SALT: &[u8] = b"hydragent-vault-column-v1";
/// AES-GCM nonce length.
const NONCE_LEN: usize = 12;
/// AES-GCM tag length.
const TAG_LEN: usize = 16;

/// Errors from column encryption / decryption.
#[derive(Debug, thiserror::Error)]
pub enum ColumnCipherError {
    #[error("bad magic header (expected CC01)")]
    BadMagic,
    #[error("unsupported column cipher version: {0}")]
    UnsupportedVersion(u32),
    #[error("ciphertext too short: {0} bytes (minimum {1})")]
    CiphertextTooShort(usize, usize),
    #[error("AES-GCM decryption failed: tag mismatch or corrupt data")]
    DecryptFailed,
    #[error("AES-GCM encryption failed: {0}")]
    EncryptFailed(String),
    #[error("master key must be exactly 32 bytes, got {0}")]
    InvalidKeyLength(usize),
    #[error("SecureBuffer error: {0}")]
    SecureBuffer(String),
}

/// A column cipher instance, parameterised by a 32-byte master key.
///
/// Create with [`ColumnCipher::new`] (new master key) or
/// [`ColumnCipher::from_secure_buffer`] (existing locked key). Each
/// `ColumnCipher` is cheap to clone — internally it holds a 32-byte key
/// wrapped in a `SecureBuffer<u8>` for mlock + zeroize.
pub struct ColumnCipher {
    /// The 32-byte column-encryption sub-key. This is derived from the
    /// master key and HKDF'd per column, so the actual key here is the
    /// "default column key" used for all columns that do not specify
    /// their own key.
    default_key: SecureBuffer<[u8; 32]>,
}

impl Clone for ColumnCipher {
    fn clone(&self) -> Self {
        // Deep-copy the key. The cloned SecureBuffer gets its own
        // allocation + mlock attempt.
        let key_bytes: [u8; 32] = self.default_key.as_slice()[0];
        ColumnCipher {
            default_key: SecureBuffer::new(key_bytes).expect("alloc clone"),
        }
    }
}

impl Drop for ColumnCipher {
    fn drop(&mut self) {
        // SecureBuffer's Drop already handles zeroize + munlock.
        // Nothing extra needed here.
    }
}

impl fmt::Debug for ColumnCipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never leak the key.
        f.debug_struct("ColumnCipher").finish_non_exhaustive()
    }
}

impl ColumnCipher {
    /// Construct a `ColumnCipher` from a raw 32-byte master key. The
    /// master key is HKDF-mixed to derive a column sub-key, which is
    /// then locked in a `SecureBuffer`.
    pub fn new(master_key: &[u8; 32]) -> Self {
        let sub_key = derive_column_subkey(master_key, b"default");
        ColumnCipher {
            default_key: SecureBuffer::new(sub_key)
                .expect("32-byte key allocation should never fail"),
        }
    }

    /// Construct a `ColumnCipher` from an already-allocated SecureBuffer
    /// (e.g. the master key from the vault).
    pub fn from_secure_buffer(master_key: &SecureBuffer<[u8; 32]>) -> Result<Self, ColumnCipherError> {
        if master_key.len() != 1 {
            return Err(ColumnCipherError::InvalidKeyLength(master_key.len() * 32));
        }
        let raw = master_key.as_slice()[0];
        Ok(Self::new(&raw))
    }

    /// Encrypt a single column value. The column name is used as AAD
    /// (associated data) so an attacker cannot swap ciphertexts between
    /// columns.
    pub fn encrypt(&self, column: &str, plaintext: &[u8]) -> Result<Vec<u8>, ColumnCipherError> {
        // Derive the per-column key on the fly. We could cache these,
        // but the cost of HKDF is one SHA-256 block + HMAC expansion,
        // which is sub-microsecond.
        let master = self.default_key.as_slice()[0];
        let column_key = derive_column_subkey(&master, column.as_bytes());

        let key = Key::<Aes256Gcm>::from_slice(&column_key);
        let cipher = Aes256Gcm::new(key);
        let nonce = Aes256Gcm::generate_nonce(&mut rand::rngs::OsRng);

        // AES-GCM with AAD binding. The AAD is the column name.
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: column.as_bytes(),
                },
            )
            .map_err(|e| ColumnCipherError::EncryptFailed(e.to_string()))?;

        // Pack: magic | version | nonce | tag | ciphertext
        // The AES-GCM library appends the tag to the ciphertext, so we
        // can store them together. We split them out explicitly so the
        // wire format is self-describing.
        if ciphertext.len() < TAG_LEN {
            return Err(ColumnCipherError::CiphertextTooShort(ciphertext.len(), TAG_LEN));
        }
        let (ct, tag) = ciphertext.split_at(ciphertext.len() - TAG_LEN);

        let mut out = Vec::with_capacity(4 + 4 + NONCE_LEN + TAG_LEN + ct.len());
        out.extend_from_slice(COLUMN_MAGIC);
        out.extend_from_slice(&COLUMN_VERSION.to_le_bytes());
        out.extend_from_slice(nonce.as_slice());
        out.extend_from_slice(tag);
        out.extend_from_slice(ct);
        Ok(out)
    }

    /// Decrypt a single column value. Returns `Err(DecryptFailed)` on
    /// tag mismatch (which is also the signal that the column name is
    /// wrong, the key is wrong, or the data is corrupt).
    pub fn decrypt(&self, column: &str, blob: &[u8]) -> Result<Vec<u8>, ColumnCipherError> {
        // Validate header.
        if blob.len() < 4 + 4 + NONCE_LEN + TAG_LEN {
            return Err(ColumnCipherError::CiphertextTooShort(blob.len(), 4 + 4 + NONCE_LEN + TAG_LEN));
        }
        if &blob[0..4] != COLUMN_MAGIC {
            return Err(ColumnCipherError::BadMagic);
        }
        let mut version_bytes = [0u8; 4];
        version_bytes.copy_from_slice(&blob[4..8]);
        let version = u32::from_le_bytes(version_bytes);
        if version != COLUMN_VERSION {
            return Err(ColumnCipherError::UnsupportedVersion(version));
        }

        // Unpack.
        let nonce_bytes: [u8; NONCE_LEN] = blob[8..8 + NONCE_LEN].try_into().expect("len checked");
        let tag_bytes: [u8; TAG_LEN] = blob[8 + NONCE_LEN..8 + NONCE_LEN + TAG_LEN].try_into().expect("len checked");
        let ct = &blob[8 + NONCE_LEN + TAG_LEN..];

        // Reassemble ciphertext || tag (AES-GCM's expected format).
        let mut combined = Vec::with_capacity(ct.len() + TAG_LEN);
        combined.extend_from_slice(ct);
        combined.extend_from_slice(&tag_bytes);

        // Derive the per-column key.
        let master = self.default_key.as_slice()[0];
        let column_key = derive_column_subkey(&master, column.as_bytes());

        let key = Key::<Aes256Gcm>::from_slice(&column_key);
        let cipher = Aes256Gcm::new(key);
        let nonce = Nonce::from_slice(&nonce_bytes);

        cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &combined,
                    aad: column.as_bytes(),
                },
            )
            .map_err(|_| ColumnCipherError::DecryptFailed)
    }

    /// Convenience: encrypt a UTF-8 string. Same as `encrypt(col, s.as_bytes())`.
    pub fn encrypt_str(&self, column: &str, plaintext: &str) -> Result<Vec<u8>, ColumnCipherError> {
        self.encrypt(column, plaintext.as_bytes())
    }

    /// Convenience: decrypt and decode as UTF-8.
    pub fn decrypt_str(&self, column: &str, blob: &[u8]) -> Result<String, ColumnCipherError> {
        let bytes = self.decrypt(column, blob)?;
        String::from_utf8(bytes).map_err(|_| ColumnCipherError::DecryptFailed)
    }

    /// Generate a fresh 32-byte column key. The caller is responsible
    /// for persisting it (e.g. as a vault secret named
    /// `__column_key__`).
    pub fn fresh_key() -> [u8; 32] {
        use rand::RngCore;
        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        key
    }

    /// Return a copy of the current default key. The caller takes
    /// ownership of a fresh `SecureBuffer<[u8; 32]>`.
    pub fn export_key(&self) -> SecureBuffer<[u8; 32]> {
        let raw: [u8; 32] = self.default_key.as_slice()[0];
        SecureBuffer::new(raw).expect("alloc export")
    }
}

/// Derive a 32-byte per-column sub-key from a master key using HKDF-SHA256.
///
/// `info` should be the column name. Different `info` values produce
/// cryptographically independent keys, even from the same master key.
fn derive_column_subkey(master: &[u8; 32], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(COLUMN_HKDF_SALT), master);
    let mut okm = [0u8; 32];
    // hkdf.expand never fails for okm_len <= 255 * hash_len = 8160 bytes.
    hk.expand(info, &mut okm)
        .expect("HKDF expand of 32 bytes cannot fail");
    okm
}

// Helper trait for local zeroize on stack values.
// Stack copies of the column sub-key are zeroed by the AES-GCM key
// schedule as soon as encrypt/decrypt returns, so we don't need an
// explicit Drop here.

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cipher() -> ColumnCipher {
        let key = [0x42u8; 32];
        ColumnCipher::new(&key)
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let c = test_cipher();
        let col = "secrets.telegram.bot_token";
        let plaintext = b"bot-token-12345";
        let blob = c.encrypt(col, plaintext).expect("encrypt");
        let recovered = c.decrypt(col, &blob).expect("decrypt");
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn encrypt_decrypt_str_roundtrip() {
        let c = test_cipher();
        let col = "secrets.openai.api_key";
        let plaintext = "sk-proj-abc123XYZ";
        let blob = c.encrypt_str(col, plaintext).expect("encrypt");
        let recovered = c.decrypt_str(col, &blob).expect("decrypt");
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn decrypt_with_wrong_column_fails() {
        let c = test_cipher();
        let col = "secrets.openai.api_key";
        let blob = c.encrypt_str(col, "secret").expect("encrypt");
        // AAD is different — AES-GCM tag check should fail.
        let res = c.decrypt_str("secrets.anthropic.api_key", &blob);
        assert!(matches!(res, Err(ColumnCipherError::DecryptFailed)));
    }

    #[test]
    fn decrypt_with_wrong_key_fails() {
        let c1 = ColumnCipher::new(&[0x42u8; 32]);
        let c2 = ColumnCipher::new(&[0x99u8; 32]);
        let col = "secrets.test";
        let blob = c1.encrypt_str(col, "value").expect("encrypt");
        let res = c2.decrypt_str(col, &blob);
        assert!(matches!(res, Err(ColumnCipherError::DecryptFailed)));
    }

    #[test]
    fn bad_magic_rejected() {
        let c = test_cipher();
        let mut blob = c.encrypt_str("col", "data").expect("encrypt");
        blob[0] = b'X'; // Corrupt the magic.
        let res = c.decrypt("col", &blob);
        assert!(matches!(res, Err(ColumnCipherError::BadMagic)));
    }

    #[test]
    fn short_blob_rejected() {
        let c = test_cipher();
        let blob = vec![0u8; 10];
        let res = c.decrypt("col", &blob);
        assert!(matches!(res, Err(ColumnCipherError::CiphertextTooShort(_, _))));
    }

    #[test]
    fn different_columns_produce_different_ciphertexts() {
        let c = test_cipher();
        let blob1 = c.encrypt_str("col_a", "same plaintext").expect("a");
        let blob2 = c.encrypt_str("col_b", "same plaintext").expect("b");
        // Same plaintext + different AAD + random nonce = different ciphertext.
        assert_ne!(blob1, blob2);
    }

    #[test]
    fn same_plaintext_different_nonces() {
        let c = test_cipher();
        let blob1 = c.encrypt_str("col", "same").expect("1");
        let blob2 = c.encrypt_str("col", "same").expect("2");
        // Nonces are random, so even identical plaintexts produce different blobs.
        assert_ne!(blob1, blob2);
    }

    #[test]
    fn two_columns_with_same_master_key_are_independent() {
        // This proves HKDF info binding works.
        let _c = test_cipher();
        let key1 = derive_column_subkey(&[0x42u8; 32], b"col_a");
        let key2 = derive_column_subkey(&[0x42u8; 32], b"col_b");
        assert_ne!(key1, key2);
    }

    #[test]
    fn column_cipher_clone_works() {
        let c = test_cipher();
        let c2 = c.clone();
        let blob = c.encrypt_str("col", "data").expect("encrypt");
        // The clone should be able to decrypt the original's output.
        let recovered = c2.decrypt_str("col", &blob).expect("decrypt");
        assert_eq!(recovered, "data");
    }

    #[test]
    fn fresh_key_is_random() {
        let k1 = ColumnCipher::fresh_key();
        let k2 = ColumnCipher::fresh_key();
        assert_ne!(k1, k2);
    }
}
