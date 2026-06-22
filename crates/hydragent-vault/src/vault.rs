use anyhow::{anyhow, Context};
use rand::RngCore;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::crypto::{decrypt, derive_key, encrypt};
use crate::taint::TaintedString;

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct VaultSlot {
    pub active: bool,
    pub salt: [u8; 32],
    pub nonce: [u8; 24],
    pub encrypted_master_key: Vec<u8>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct VaultMetadataV2 {
    pub magic: [u8; 4],
    pub version: u8,
    pub master_key_nonce: [u8; 24],
    pub slot_0: VaultSlot,
    pub slot_1: VaultSlot,
    pub encrypted_payload: Vec<u8>,
}

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

    pub fn load_meta_v2(&self) -> anyhow::Result<VaultMetadataV2> {
        let file_bytes = fs::read(&self.path).context("Failed to read vault file")?;
        if file_bytes.len() < 5 {
            return Err(anyhow!("Vault file is truncated"));
        }
        if &file_bytes[0..4] != b"HVLT" {
            return Err(anyhow!("Invalid vault magic"));
        }
        if file_bytes[4] != 2 {
            return Err(anyhow!("Not a V2 vault"));
        }
        let meta: VaultMetadataV2 = bincode::deserialize(&file_bytes)
            .context("Failed to deserialize V2 vault metadata")?;
        Ok(meta)
    }

    pub fn load(&self, passphrase: &str) -> anyhow::Result<HashMap<String, TaintedString>> {
        if !self.path.exists() {
            return Err(anyhow!("Vault file does not exist at {:?}", self.path));
        }

        let file_bytes = fs::read(&self.path).context("Failed to read vault file")?;
        if file_bytes.len() < 5 {
            return Err(anyhow!("Vault file is truncated or corrupted"));
        }

        let magic = &file_bytes[0..4];
        if magic != b"HVLT" {
            return Err(anyhow!("Invalid vault magic header"));
        }

        let version = file_bytes[4];
        if version == 1 {
            // Decrypt V1 vault
            if file_bytes.len() < 61 {
                return Err(anyhow!("V1 vault is truncated"));
            }
            let mut salt = [0u8; 32];
            salt.copy_from_slice(&file_bytes[5..37]);

            let mut nonce = [0u8; 24];
            nonce.copy_from_slice(&file_bytes[37..61]);

            let ciphertext = &file_bytes[61..];

            let key = derive_key(passphrase, &salt)?;
            let plaintext = decrypt(ciphertext, &key, &nonce)
                .context("Failed to decrypt vault. Incorrect passphrase?")?;

            let raw: HashMap<String, String> = bincode::deserialize(&plaintext)
                .context("Failed to deserialize secrets map")?;
            let secrets: HashMap<String, TaintedString> = raw
                .into_iter()
                .map(|(k, v)| (k, TaintedString::credential(v)))
                .collect();
            return Ok(secrets);
        } else if version == 2 {
            let meta: VaultMetadataV2 = bincode::deserialize(&file_bytes)
                .context("Failed to deserialize V2 vault metadata")?;

            let (is_admin_key, key_bytes) = if passphrase.starts_with("admin_key_file:") {
                let path = passphrase.trim_start_matches("admin_key_file:");
                (true, fs::read(path).context("Failed to read admin key file")?)
            } else if PathBuf::from(passphrase).exists() {
                (true, fs::read(passphrase).context("Failed to read admin key file")?)
            } else {
                (false, passphrase.as_bytes().to_vec())
            };

            let mut master_key = [0u8; 32];
            if is_admin_key {
                if !meta.slot_1.active {
                    return Err(anyhow!("Slot 1 (Admin Key) is not active in this vault"));
                }
                let key_hex = hex::encode(&key_bytes);
                let slot_key = derive_key(&key_hex, &meta.slot_1.salt)?;
                let decrypted = decrypt(&meta.slot_1.encrypted_master_key, &slot_key, &meta.slot_1.nonce)
                    .context("Failed to decrypt master key using Admin Key File")?;
                master_key.copy_from_slice(&decrypted[0..32]);
            } else {
                if !meta.slot_0.active {
                    return Err(anyhow!("Slot 0 (Passphrase PIN) is not active in this vault"));
                }
                let slot_key = derive_key(passphrase, &meta.slot_0.salt)?;
                let decrypted = decrypt(&meta.slot_0.encrypted_master_key, &slot_key, &meta.slot_0.nonce)
                    .context("Failed to decrypt master key using Passphrase PIN")?;
                master_key.copy_from_slice(&decrypted[0..32]);
            }

            let plaintext = decrypt(&meta.encrypted_payload, &master_key, &meta.master_key_nonce)
                .context("Failed to decrypt vault payload")?;

            let raw: HashMap<String, String> = bincode::deserialize(&plaintext)
                .context("Failed to deserialize secrets map")?;
            let secrets: HashMap<String, TaintedString> = raw
                .into_iter()
                .map(|(k, v)| (k, TaintedString::credential(v)))
                .collect();
            return Ok(secrets);
        }

        Err(anyhow!("Unsupported vault version: {}", version))
    }

    pub fn save(
        &self,
        passphrase: &str,
        secrets: &HashMap<String, TaintedString>,
    ) -> anyhow::Result<()> {
        let (is_admin_key, key_bytes) = if passphrase.starts_with("admin_key_file:") {
            let path = passphrase.trim_start_matches("admin_key_file:");
            (true, fs::read(path).context("Failed to read admin key file")?)
        } else if PathBuf::from(passphrase).exists() {
            (true, fs::read(passphrase).context("Failed to read admin key file")?)
        } else {
            (false, passphrase.as_bytes().to_vec())
        };

        let mut master_key = [0u8; 32];
        let mut slot_0 = VaultSlot {
            active: false,
            salt: [0u8; 32],
            nonce: [0u8; 24],
            encrypted_master_key: vec![],
        };
        let mut slot_1 = VaultSlot {
            active: false,
            salt: [0u8; 32],
            nonce: [0u8; 24],
            encrypted_master_key: vec![],
        };

        if self.path.exists() {
            if let Ok(existing_meta) = self.load_meta_v2() {
                slot_0 = existing_meta.slot_0.clone();
                slot_1 = existing_meta.slot_1.clone();

                if is_admin_key {
                    if !slot_1.active {
                        return Err(anyhow!("Slot 1 (Admin Key) is not active in this vault"));
                    }
                    let key_hex = hex::encode(&key_bytes);
                    let slot_key = derive_key(&key_hex, &slot_1.salt)?;
                    let decrypted = decrypt(&slot_1.encrypted_master_key, &slot_key, &slot_1.nonce)
                        .context("Failed to decrypt master key using Admin Key File")?;
                    master_key.copy_from_slice(&decrypted[0..32]);
                } else {
                    if !slot_0.active {
                        return Err(anyhow!("Slot 0 (Passphrase PIN) is not active in this vault"));
                    }
                    let slot_key = derive_key(passphrase, &slot_0.salt)?;
                    let decrypted = decrypt(&slot_0.encrypted_master_key, &slot_key, &slot_0.nonce)
                        .context("Failed to decrypt master key using Passphrase PIN")?;
                    master_key.copy_from_slice(&decrypted[0..32]);
                }
            } else if let Ok(v1_bytes) = fs::read(&self.path) {
                if v1_bytes.len() >= 61 && &v1_bytes[0..4] == b"HVLT" && v1_bytes[4] == 1 {
                    let mut salt = [0u8; 32];
                    salt.copy_from_slice(&v1_bytes[5..37]);
                    if is_admin_key {
                        return Err(anyhow!("Cannot migrate V1 vault using Admin Key File (only Passphrase PIN works for V1 migration)"));
                    }
                    let key = derive_key(passphrase, &salt)?;
                    master_key.copy_from_slice(&key);
                } else {
                    rand::thread_rng().fill_bytes(&mut master_key);
                }
            } else {
                rand::thread_rng().fill_bytes(&mut master_key);
            }
        } else {
            rand::thread_rng().fill_bytes(&mut master_key);
        }

        if is_admin_key {
            slot_1.active = true;
            rand::thread_rng().fill_bytes(&mut slot_1.salt);
            rand::thread_rng().fill_bytes(&mut slot_1.nonce);
            let key_hex = hex::encode(&key_bytes);
            let slot_key = derive_key(&key_hex, &slot_1.salt)?;
            slot_1.encrypted_master_key = encrypt(&master_key, &slot_key, &slot_1.nonce)?;
        } else {
            slot_0.active = true;
            rand::thread_rng().fill_bytes(&mut slot_0.salt);
            rand::thread_rng().fill_bytes(&mut slot_0.nonce);
            let slot_key = derive_key(passphrase, &slot_0.salt)?;
            slot_0.encrypted_master_key = encrypt(&master_key, &slot_key, &slot_0.nonce)?;
            
            // Sync/setup admin auth PIN
            let _ = setup_admin_auth(passphrase);
        }

        let raw: HashMap<String, String> = secrets
            .iter()
            .map(|(k, v)| (k.clone(), v.expose_secret().to_string()))
            .collect();
        let plaintext = bincode::serialize(&raw).context("Failed to serialize secrets")?;

        let mut payload_nonce = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut payload_nonce);
        let encrypted_payload = encrypt(&plaintext, &master_key, &payload_nonce)?;

        let meta = VaultMetadataV2 {
            magic: *b"HVLT",
            version: 2,
            master_key_nonce: payload_nonce,
            slot_0,
            slot_1,
            encrypted_payload,
        };

        let file_bytes = bincode::serialize(&meta).context("Failed to serialize vault metadata")?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).context("Failed to create vault directory")?;
        }
        fs::write(&self.path, file_bytes).context("Failed to write vault file")?;

        Ok(())
    }

    pub fn set_admin_key(&self, passphrase_pin: &str, admin_key_path: PathBuf) -> anyhow::Result<()> {
        let _secrets = self.load(passphrase_pin)?;
        let admin_key_bytes = fs::read(&admin_key_path).context("Failed to read admin key file")?;
        let key_hex = hex::encode(&admin_key_bytes);

        let mut meta = self.load_meta_v2().unwrap_or_else(|_| {
            VaultMetadataV2 {
                magic: *b"HVLT",
                version: 2,
                master_key_nonce: [0u8; 24],
                slot_0: VaultSlot { active: false, salt: [0u8; 32], nonce: [0u8; 24], encrypted_master_key: vec![] },
                slot_1: VaultSlot { active: false, salt: [0u8; 32], nonce: [0u8; 24], encrypted_master_key: vec![] },
                encrypted_payload: vec![],
            }
        });

        let mut master_key = [0u8; 32];
        if meta.slot_0.active {
            let slot_key = derive_key(passphrase_pin, &meta.slot_0.salt)?;
            let decrypted = decrypt(&meta.slot_0.encrypted_master_key, &slot_key, &meta.slot_0.nonce)?;
            master_key.copy_from_slice(&decrypted[0..32]);
        } else {
            let file_bytes = fs::read(&self.path)?;
            let mut salt = [0u8; 32];
            salt.copy_from_slice(&file_bytes[5..37]);
            let key = derive_key(passphrase_pin, &salt)?;
            master_key.copy_from_slice(&key);
        }

        meta.slot_1.active = true;
        rand::thread_rng().fill_bytes(&mut meta.slot_1.salt);
        rand::thread_rng().fill_bytes(&mut meta.slot_1.nonce);
        let slot_key = derive_key(&key_hex, &meta.slot_1.salt)?;
        meta.slot_1.encrypted_master_key = encrypt(&master_key, &slot_key, &meta.slot_1.nonce)?;

        let file_bytes = bincode::serialize(&meta)?;
        fs::write(&self.path, file_bytes)?;
        Ok(())
    }
}

pub fn setup_admin_auth(passphrase_pin: &str) -> anyhow::Result<()> {
    let mut salt = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut salt);
    let mut derived_key = [0u8; 32];
    crate::crypto::pbkdf2_hmac_sha256(passphrase_pin.as_bytes(), &salt, 100000, &mut derived_key);

    let path = std::path::Path::new("config/security/admin_auth.hash");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::json!({
        "salt": hex::encode(salt),
        "hash": hex::encode(derived_key),
        "iterations": 100000
    });
    std::fs::write(path, serde_json::to_string_pretty(&data)?)?;
    Ok(())
}
