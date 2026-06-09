use anyhow::{anyhow, Context};
use rand::RngCore;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::crypto::{decrypt, derive_key, encrypt};
use crate::taint::TaintedString;

pub struct Vault {
    path: PathBuf,
}

impl Vault {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    pub fn init(&self, passphrase: &str) -> anyhow::Result<()> {
        if self.path.exists() {
            return Err(anyhow!("Vault already exists at {:?}", self.path));
        }

        // Create directory structure if needed
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).context("Failed to create vault directory")?;
        }

        let secrets: HashMap<String, TaintedString> = HashMap::new();
        self.save(passphrase, &secrets)?;
        Ok(())
    }

    pub fn load(&self, passphrase: &str) -> anyhow::Result<HashMap<String, TaintedString>> {
        if !self.path.exists() {
            return Err(anyhow!("Vault file does not exist at {:?}", self.path));
        }

        let file_bytes = fs::read(&self.path).context("Failed to read vault file")?;
        if file_bytes.len() < 61 {
            return Err(anyhow!("Vault file is truncated or corrupted"));
        }

        let magic = &file_bytes[0..4];
        if magic != b"HVLT" {
            return Err(anyhow!("Invalid vault magic header"));
        }

        let version = file_bytes[4];
        if version != 1 {
            return Err(anyhow!("Unsupported vault version: {}", version));
        }

        let mut salt = [0u8; 32];
        salt.copy_from_slice(&file_bytes[5..37]);

        let mut nonce = [0u8; 24];
        nonce.copy_from_slice(&file_bytes[37..61]);

        let ciphertext = &file_bytes[61..];

        let key = derive_key(passphrase, &salt)?;
        let plaintext = decrypt(ciphertext, &key, &nonce)
            .context("Failed to decrypt vault. Incorrect passphrase?")?;

        let secrets: HashMap<String, TaintedString> = bincode::deserialize(&plaintext)
            .context("Failed to deserialize secrets map")?;

        Ok(secrets)
    }

    pub fn save(
        &self,
        passphrase: &str,
        secrets: &HashMap<String, TaintedString>,
    ) -> anyhow::Result<()> {
        let mut salt = [0u8; 32];
        let mut nonce = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut salt);
        rand::thread_rng().fill_bytes(&mut nonce);

        let plaintext = bincode::serialize(secrets).context("Failed to serialize secrets")?;
        let key = derive_key(passphrase, &salt)?;
        let ciphertext = encrypt(&plaintext, &key, &nonce)?;

        let mut file_bytes = Vec::with_capacity(4 + 1 + 32 + 24 + ciphertext.len());
        file_bytes.extend_from_slice(b"HVLT");
        file_bytes.push(1); // version
        file_bytes.extend_from_slice(&salt);
        file_bytes.extend_from_slice(&nonce);
        file_bytes.extend_from_slice(&ciphertext);

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).context("Failed to create vault directory")?;
        }
        fs::write(&self.path, file_bytes).context("Failed to write vault file")?;

        Ok(())
    }
}
