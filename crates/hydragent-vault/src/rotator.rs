//! Credential rotation: passphrase rotation and column-key rotation.
//!
//! ## Why rotate?
//!
//! - **Passphrase**: protects the master key via Argon2id KDF. A weak
//!   passphrase, a shared team passphrase, or a passphrase seen in a
//!   shoulder-surf can be rotated without re-issuing every secret.
//! - **Column key**: the AES-256-GCM sub-key used by [`ColumnCipher`].
//!   Rotating it invalidates all old ciphertexts (forward secrecy)
//!   and re-derives fresh HKDF sub-keys for every column.
//!
//! ## Algorithm
//!
//! ### `rotate_passphrase(old, new)`
//!
//! 1. Read vault with `old` passphrase → `HashMap<String, TaintedString>`.
//! 2. Write vault with `new` passphrase (re-encrypts with a fresh salt
//!    and nonce).
//! 3. Verify: read vault with `new` passphrase → confirm it parses and
//!    the entry count matches.
//!
//! Atomicity is enforced by writing the new file to `vault.hvlt.tmp`
//! and then renaming it to `vault.hvlt`. If the temp file write fails,
//! the original is untouched.
//!
//! ### `rotate_column_key(passphrase)`
//!
//! 1. Read vault with `passphrase`.
//! 2. Generate a fresh 32-byte column key (OS CSPRNG).
//! 3. Replace the existing `__column_key__` entry (or insert a new one).
//! 4. Write the vault back.
//!
//! Note: rotating the column key invalidates all previously
//! column-encrypted data unless the consumer re-encrypts. Callers
//! should treat old `ColumnCipher` ciphertexts as garbage after a
//! rotation; this is intentional.
//!
//! ## Thread safety
//!
//! The rotator is a single-shot struct — construct it with a vault
//! path, call the rotation method, drop it. There is no shared state
//! to protect.

use std::path::{Path, PathBuf};

use crate::secure_buffer::SecureBuffer;
use crate::taint::TaintedString;
use crate::vault::Vault;

/// Special vault entry name that stores the column key.
///
/// Stored as a hex-encoded 32-byte key inside a `TaintedString`. The
/// rotator reads/writes this entry; the [`ColumnCipher`](crate::column_cipher::ColumnCipher)
/// reads it on startup.
pub const COLUMN_KEY_ENTRY: &str = "__column_key__";

/// Report of a rotation operation. Returned to the caller for logging
/// and verification.
#[derive(Debug, Clone)]
pub struct RotationReport {
    /// Number of entries in the vault after rotation.
    pub entries_after: usize,
    /// Whether the column key was also rotated.
    pub column_key_rotated: bool,
    /// Hex-encoded column key after rotation (only when rotated).
    pub new_column_key_hex: Option<String>,
}

/// Errors from rotation operations.
#[derive(Debug, thiserror::Error)]
pub enum RotationError {
    #[error("failed to read vault with old passphrase: {0}")]
    ReadOldFailed(String),
    #[error("failed to write vault with new passphrase: {0}")]
    WriteNewFailed(String),
    #[error("failed to verify vault with new passphrase: {0}")]
    VerifyFailed(String),
    #[error("vault file is missing — cannot rotate")]
    VaultMissing,
    #[error("old passphrase rejected (decrypt failed or corrupt)")]
    WrongOldPassphrase,
    #[error("entry count mismatch after rotation: pre={pre}, post={post}")]
    EntryCountMismatch { pre: usize, post: usize },
    #[error("column key parse failed: {0}")]
    ColumnKeyParse(String),
    #[error("io error: {0}")]
    Io(String),
}

/// The rotator. Construct with a vault path; call a rotation method.
pub struct Rotator {
    path: PathBuf,
}

impl Rotator {
    /// Create a new rotator that operates on the given vault file path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Path to the vault file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Rotate the vault's passphrase. Reads with `old`, re-encrypts with
    /// `new`, verifies the result.
    ///
    /// Returns a [`RotationReport`] with the entry count after rotation.
    pub fn rotate_passphrase(
        &self,
        old_passphrase: &str,
        new_passphrase: &str,
    ) -> Result<RotationReport, RotationError> {
        if !self.path.exists() {
            return Err(RotationError::VaultMissing);
        }
        if old_passphrase == new_passphrase {
            // Technically a no-op, but we reject it because users
            // probably didn't mean to rotate to the same value.
            return Err(RotationError::WriteNewFailed(
                "new passphrase is identical to old".into(),
            ));
        }

        // 1. Read with old passphrase.
        let entries = Vault::new(self.path.clone())
            .load(old_passphrase)
            .map_err(|e| RotationError::ReadOldFailed(e.to_string()))?;
        let pre_count = entries.len();

        // 2. Write atomically: temp file, then rename.
        let temp_path = self.path.with_extension("hvlt.tmp");
        if let Err(e) = Vault::new(temp_path.clone()).save(new_passphrase, &entries) {
            return Err(RotationError::WriteNewFailed(e.to_string()));
        }

        // 2b. Verify the temp file by re-reading it.
        if !self.path_after_rotate_check(temp_path.as_path(), new_passphrase, pre_count) {
            // Cleanup temp file
            let _ = std::fs::remove_file(&temp_path);
            return Err(RotationError::WriteNewFailed("temp file write failed verification".into()));
        }

        // 3. Atomic rename.
        std::fs::rename(&temp_path, &self.path)
            .map_err(|e| RotationError::Io(e.to_string()))?;

        Ok(RotationReport {
            entries_after: pre_count,
            column_key_rotated: false,
            new_column_key_hex: None,
        })
    }

    /// Verify a freshly-written temp vault file. Helper for atomic rename.
    fn path_after_rotate_check(
        &self,
        temp_path: &Path,
        passphrase: &str,
        expected_entries: usize,
    ) -> bool {
        let res = Vault::new(temp_path.to_path_buf()).load(passphrase);
        match res {
            Ok(entries) => entries.len() == expected_entries,
            Err(_) => false,
        }
    }

    /// Rotate the column key. Generates a new 32-byte AES key (OS CSPRNG),
    /// stores it in the vault as `__column_key__`, and returns the new
    /// key as both a hex string (for logs) and a [`SecureBuffer`] (for
    /// live use).
    ///
    /// The vault is re-encrypted with the same passphrase, just with
    /// a fresh salt and nonce (so the file is byte-different even
    /// though the contents are functionally identical except for the
    /// new column key entry).
    pub fn rotate_column_key(
        &self,
        passphrase: &str,
    ) -> Result<(RotationReport, SecureBuffer<[u8; 32]>), RotationError> {
        if !self.path.exists() {
            return Err(RotationError::VaultMissing);
        }

        // 1. Read current entries.
        let mut entries = Vault::new(self.path.clone())
            .load(passphrase)
            .map_err(|e| RotationError::ReadOldFailed(e.to_string()))?;
        let pre_count = entries.len();
        let column_key_exists = entries.contains_key(COLUMN_KEY_ENTRY);

        // 2. Generate a fresh 32-byte column key.
        let mut key_bytes = [0u8; 32];
        use rand::RngCore;
        rand::rngs::OsRng.fill_bytes(&mut key_bytes);
        let key_hex = hex_encode(&key_bytes);

        // 3. Insert the new column key entry.
        entries.insert(COLUMN_KEY_ENTRY.to_string(), TaintedString::credential(key_hex.clone()));

        // 4. Re-encrypt (atomic via temp file). Expected entry count
        //    is `pre_count` if the column key was already present
        //    (we just overwrote it), or `pre_count + 1` if it was new.
        let expected_count = if column_key_exists {
            pre_count
        } else {
            pre_count + 1
        };

        let temp_path = self.path.with_extension("hvlt.tmp");
        if let Err(e) = Vault::new(temp_path.clone()).save(passphrase, &entries) {
            return Err(RotationError::WriteNewFailed(e.to_string()));
        }

        if !self.path_after_rotate_check(temp_path.as_path(), passphrase, expected_count) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(RotationError::WriteNewFailed("post-write verification failed".into()));
        }
        std::fs::rename(&temp_path, &self.path)
            .map_err(|e| RotationError::Io(e.to_string()))?;

        // 5. Build the SecureBuffer for the caller.
        let secure_key = SecureBuffer::new(key_bytes)
            .map_err(|e| RotationError::ColumnKeyParse(e.to_string()))?;

        Ok((
            RotationReport {
                entries_after: pre_count,
                column_key_rotated: true,
                new_column_key_hex: Some(key_hex),
            },
            secure_key,
        ))
    }

    /// Read the column key from the vault. Returns `None` if no column
    /// key has been set yet.
    pub fn read_column_key(
        &self,
        passphrase: &str,
    ) -> Result<Option<SecureBuffer<[u8; 32]>>, RotationError> {
        if !self.path.exists() {
            return Err(RotationError::VaultMissing);
        }
        let entries = Vault::new(self.path.clone())
            .load(passphrase)
            .map_err(|e| RotationError::ReadOldFailed(e.to_string()))?;
        match entries.get(COLUMN_KEY_ENTRY) {
            Some(tainted) => {
                let hex_str = tainted.expose_secret();
                let bytes = hex_decode(hex_str)
                    .map_err(|e| RotationError::ColumnKeyParse(e))?;
                if bytes.len() != 32 {
                    return Err(RotationError::ColumnKeyParse(format!(
                        "column key is {} bytes, expected 32",
                        bytes.len()
                    )));
                }
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                let secure = SecureBuffer::new(arr)
                    .map_err(|e| RotationError::ColumnKeyParse(e.to_string()))?;
                Ok(Some(secure))
            }
            None => Ok(None),
        }
    }
}

/// Hex-encode a byte slice (no external dep).
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Decode a hex string into bytes. Errors on odd length or non-hex chars.
fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("odd length".into());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let hi = hex_nibble(bytes[i])?;
        let lo = hex_nibble(bytes[i + 1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(format!("non-hex char: {}", b as char)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::env;

    fn temp_vault(name: &str) -> PathBuf {
        let mut p = env::temp_dir();
        p.push(format!("hydragent_rotator_test_{}_{}.hvlt", name, std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn rotate_passphrase_preserves_entries() {
        let path = temp_vault("pass_pres");
        let v = Vault::new(path.clone());
        v.init("oldpass").unwrap();
        let mut entries: HashMap<String, TaintedString> = HashMap::new();
        entries.insert("api_key".into(), TaintedString::credential("secret-1".into()));
        entries.insert("bot_token".into(), TaintedString::credential("bot-1".into()));
        v.save("oldpass", &entries).unwrap();

        let rotator = Rotator::new(path.clone());
        let report = rotator.rotate_passphrase("oldpass", "newpass").unwrap();
        assert_eq!(report.entries_after, 2);
        assert!(!report.column_key_rotated);

        // Old passphrase should be rejected.
        let old_attempt = Vault::new(path.clone()).load("oldpass");
        assert!(old_attempt.is_err());

        // New passphrase should work and have both entries.
        let new_entries = Vault::new(path.clone()).load("newpass").unwrap();
        assert_eq!(new_entries.len(), 2);
        assert_eq!(new_entries.get("api_key").unwrap().expose_secret(), "secret-1");
        assert_eq!(new_entries.get("bot_token").unwrap().expose_secret(), "bot-1");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rotate_passphrase_to_same_value_fails() {
        let path = temp_vault("samepass");
        let v = Vault::new(path.clone());
        v.init("samepass").unwrap();

        let rotator = Rotator::new(path.clone());
        let res = rotator.rotate_passphrase("samepass", "samepass");
        assert!(res.is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rotate_passphrase_wrong_old_fails() {
        let path = temp_vault("wrong_old");
        let v = Vault::new(path.clone());
        v.init("realpass").unwrap();

        let rotator = Rotator::new(path.clone());
        let res = rotator.rotate_passphrase("wrongpass", "newpass");
        assert!(res.is_err());
        // Original file should still be intact.
        let still = Vault::new(path.clone()).load("realpass");
        assert!(still.is_ok());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rotate_column_key_inserts_entry() {
        let path = temp_vault("col_rot");
        let v = Vault::new(path.clone());
        v.init("vaultpass").unwrap();

        let rotator = Rotator::new(path.clone());
        let (report, _key) = rotator.rotate_column_key("vaultpass").unwrap();
        assert!(report.column_key_rotated);
        assert!(report.new_column_key_hex.is_some());
        assert_eq!(report.new_column_key_hex.as_ref().unwrap().len(), 64);

        // The vault should now contain the column key entry.
        let entries = Vault::new(path.clone()).load("vaultpass").unwrap();
        assert!(entries.contains_key(COLUMN_KEY_ENTRY));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_column_key_returns_none_when_unset() {
        let path = temp_vault("col_none");
        let v = Vault::new(path.clone());
        v.init("vaultpass").unwrap();

        let rotator = Rotator::new(path.clone());
        let res = rotator.read_column_key("vaultpass").unwrap();
        assert!(res.is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_column_key_returns_value_after_rotation() {
        let path = temp_vault("col_read");
        let v = Vault::new(path.clone());
        v.init("vaultpass").unwrap();

        let rotator = Rotator::new(path.clone());
        let (report, _) = rotator.rotate_column_key("vaultpass").unwrap();
        let key_hex = report.new_column_key_hex.unwrap();

        let read = rotator.read_column_key("vaultpass").unwrap();
        let buf = read.expect("should have key");
        let bytes = buf.as_slice()[0];
        assert_eq!(hex_encode(&bytes), key_hex);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rotate_column_key_twice_yields_different_keys() {
        let path = temp_vault("col_twice");
        let v = Vault::new(path.clone());
        v.init("vaultpass").unwrap();

        let rotator = Rotator::new(path.clone());
        let (r1, _) = rotator.rotate_column_key("vaultpass").unwrap();
        let (r2, _) = rotator.rotate_column_key("vaultpass").unwrap();

        assert_ne!(r1.new_column_key_hex, r2.new_column_key_hex);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_vault_returns_error() {
        let path = temp_vault("missing");
        let rotator = Rotator::new(path.clone());
        let res = rotator.rotate_passphrase("a", "b");
        assert!(matches!(res, Err(RotationError::VaultMissing)));
        let res2 = rotator.rotate_column_key("a");
        assert!(matches!(res2, Err(RotationError::VaultMissing)));
    }

    #[test]
    fn hex_roundtrip() {
        let original = [0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, 0x02, 0x03];
        let s = hex_encode(&original);
        let decoded = hex_decode(&s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn hex_decode_rejects_odd_length() {
        assert!(hex_decode("abc").is_err());
    }

    #[test]
    fn hex_decode_rejects_invalid_chars() {
        assert!(hex_decode("zz").is_err());
    }
}
