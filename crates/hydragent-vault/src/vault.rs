use anyhow::{anyhow, Context};
use rand::RngCore;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::crypto::{decrypt, derive_key, encrypt};
use crate::developer_recovery::{derive_slot2_key_user_side, parse_dev_private_key_file};
use crate::taint::TaintedString;

// ---------------------------------------------------------------------------
// Slot — a single encrypted copy of the vault master key
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct VaultSlot {
    pub active: bool,
    pub salt: [u8; 32],
    pub nonce: [u8; 24],
    pub encrypted_master_key: Vec<u8>,
}

impl VaultSlot {
    fn inactive() -> Self {
        Self {
            active: false,
            salt: [0u8; 32],
            nonce: [0u8; 24],
            encrypted_master_key: vec![],
        }
    }
}

// ---------------------------------------------------------------------------
// Vault Metadata V2 (legacy — kept for backwards compatibility)
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize)]
pub struct VaultMetadataV2 {
    pub magic: [u8; 4],
    pub version: u8,
    pub master_key_nonce: [u8; 24],
    pub slot_0: VaultSlot,
    pub slot_1: VaultSlot,
    pub encrypted_payload: Vec<u8>,
}

// ---------------------------------------------------------------------------
// Vault Metadata V3 (current — three-slot design)
// ---------------------------------------------------------------------------

/// V3 vault metadata with three independent unlock slots:
///
/// - **Slot 0:** Passphrase PIN (Argon2id KDF — for remote/interactive use)
/// - **Slot 1:** Admin Key File (user's 128-byte random seed stored portably)
/// - **Slot 2:** Developer Recovery (SHA-256 of dev public seed + user seed — optional)
///
/// `user_key_seed` is stored in plaintext. It is not secret —
/// it acts as the user's "public key" for the developer recovery scheme.
/// Stored as `Vec<u8>` because serde only auto-derives fixed arrays up to [u8; 32].
#[derive(serde::Serialize, serde::Deserialize)]
pub struct VaultMetadataV3 {
    pub magic: [u8; 4],
    pub version: u8,           // = 3
    pub master_key_nonce: [u8; 24],
    pub slot_0: VaultSlot,     // Passphrase PIN
    pub slot_1: VaultSlot,     // Admin Key File
    pub slot_2: VaultSlot,     // Developer Recovery (may be inactive)
    /// Random 32-byte public key of the user.
    pub user_key_seed: Vec<u8>,
    /// User's registered recovery public key (Ed25519) for request signing/verification.
    pub recovery_identity_pub: Vec<u8>,
    /// Whether Slot 2 (developer emergency recovery) is active.
    pub developer_recovery_enabled: bool,
    pub encrypted_payload: Vec<u8>,
}

// ---------------------------------------------------------------------------
// InitOptions — options for initializing a new V3 vault
// ---------------------------------------------------------------------------

/// Options passed to [`Vault::init_v3`].
pub struct InitOptions {
    /// The passphrase PIN for Slot 0.
    pub passphrase: String,
    /// If `Some`, the user private key is written as hex to this path
    /// (this becomes the portable Admin Key File).
    pub admin_key_export_path: Option<PathBuf>,
    /// Whether to activate Slot 2 (developer emergency recovery).
    pub developer_recovery: bool,
}

pub struct InitV3Result {
    pub id_priv_hex: String,
    pub id_pub_hex: String,
}

// ---------------------------------------------------------------------------
// Vault
// ---------------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // Legacy V2 init (kept for existing callers / tests)
    // -----------------------------------------------------------------------

    pub fn init(&self, passphrase: &str) -> anyhow::Result<()> {
        if self.path.exists() {
            return Err(anyhow!("Vault already exists at {:?}", self.path));
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).context("Failed to create vault directory")?;
        }
        let secrets: HashMap<String, TaintedString> = HashMap::new();
        self.save(passphrase, &secrets)?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // V3 init — three-slot vault
    // -----------------------------------------------------------------------

    /// Initialize a new V3 vault with up to three unlock slots.
    ///
    /// Fails if the vault file already exists.
    pub fn init_v3(&self, opts: &InitOptions) -> anyhow::Result<InitV3Result> {
        if self.path.exists() {
            return Err(anyhow!("Vault already exists at {:?}", self.path));
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).context("Failed to create vault directory")?;
        }

        // 1. Generate master key and user private key seed.
        let mut master_key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut master_key);

        let mut user_private_key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut user_private_key);

        // Derive user public key from private key
        let user_pub_bytes = crate::developer_recovery::x25519_public(&user_private_key);
        let user_key_seed_vec = user_pub_bytes.to_vec();

        // Generate Recovery Identity Keypair (Ed25519)
        let mut id_seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut id_seed);
        let id_priv = ed25519_dalek::SigningKey::from_bytes(&id_seed);
        let id_pub = id_priv.verifying_key();
        let id_pub_vec = id_pub.to_bytes().to_vec();

        // 2. Slot 0: Passphrase PIN (Argon2id).
        let mut slot_0 = VaultSlot::inactive();
        slot_0.active = true;
        rand::thread_rng().fill_bytes(&mut slot_0.salt);
        rand::thread_rng().fill_bytes(&mut slot_0.nonce);
        let slot0_key = derive_key(&opts.passphrase, &slot_0.salt)?;
        slot_0.encrypted_master_key = encrypt(&master_key, &slot0_key, &slot_0.nonce)?;

        // 3. Slot 1: Admin Key File (hex of user_private_key via Argon2id).
        let mut slot_1 = VaultSlot::inactive();
        slot_1.active = true;
        rand::thread_rng().fill_bytes(&mut slot_1.salt);
        rand::thread_rng().fill_bytes(&mut slot_1.nonce);
        let seed_hex = hex::encode(&user_private_key);
        let slot1_key = derive_key(&seed_hex, &slot_1.salt)?;
        slot_1.encrypted_master_key = encrypt(&master_key, &slot1_key, &slot_1.nonce)?;

        // 4. Slot 2: Developer Recovery (optional).
        let mut slot_2 = VaultSlot::inactive();
        if opts.developer_recovery {
            slot_2.active = true;
            rand::thread_rng().fill_bytes(&mut slot_2.salt);
            rand::thread_rng().fill_bytes(&mut slot_2.nonce);
            // Slot 2 key = SHA-256(X25519(user_private_key, DEV_PUBLIC_KEY))
            let slot2_key = derive_slot2_key_user_side(&user_private_key);
            slot_2.encrypted_master_key = encrypt(&master_key, &slot2_key, &slot_2.nonce)?;
        }

        // 5. Encrypt the (empty) secrets payload.
        let raw: HashMap<String, String> = HashMap::new();
        let plaintext = bincode::serialize(&raw).context("Failed to serialize secrets")?;
        let mut payload_nonce = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut payload_nonce);
        let encrypted_payload = encrypt(&plaintext, &master_key, &payload_nonce)?;

        // 6. Write vault file.
        let meta = VaultMetadataV3 {
            magic: *b"HVLT",
            version: 3,
            master_key_nonce: payload_nonce,
            slot_0,
            slot_1,
            slot_2,
            user_key_seed: user_key_seed_vec,
            recovery_identity_pub: id_pub_vec,
            developer_recovery_enabled: opts.developer_recovery,
            encrypted_payload,
        };
        let file_bytes = bincode::serialize(&meta).context("Failed to serialize V3 metadata")?;
        fs::write(&self.path, file_bytes).context("Failed to write vault file")?;

        // 7. Optionally export the admin key file.
        if let Some(ref export_path) = opts.admin_key_export_path {
            if let Some(parent) = export_path.parent() {
                fs::create_dir_all(parent)
                    .context("Failed to create admin key export directory")?;
            }
            fs::write(export_path, seed_hex.as_bytes())
                .context("Failed to write admin key file")?;
        }

        // 8. Set up the passphrase admin auth hash (legacy compat).
        let _ = setup_admin_auth(&opts.passphrase);

        Ok(InitV3Result {
            id_priv_hex: hex::encode(id_priv.to_bytes()),
            id_pub_hex: hex::encode(id_pub.to_bytes()),
        })
    }

    // -----------------------------------------------------------------------
    // Load V3 metadata
    // -----------------------------------------------------------------------

    pub fn load_meta_v3(&self) -> anyhow::Result<VaultMetadataV3> {
        let file_bytes = fs::read(&self.path).context("Failed to read vault file")?;
        if file_bytes.len() < 5 {
            return Err(anyhow!("Vault file is truncated"));
        }
        if &file_bytes[0..4] != b"HVLT" {
            return Err(anyhow!("Invalid vault magic"));
        }
        if file_bytes[4] != 3 {
            return Err(anyhow!("Not a V3 vault (version={})", file_bytes[4]));
        }
        let meta: VaultMetadataV3 =
            bincode::deserialize(&file_bytes).context("Failed to deserialize V3 vault metadata")?;
        Ok(meta)
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

    // -----------------------------------------------------------------------
    // Load secrets (supports V1, V2, V3)
    // -----------------------------------------------------------------------

    pub fn load(&self, passphrase: &str) -> anyhow::Result<HashMap<String, TaintedString>> {
        if !self.path.exists() {
            return Err(anyhow!("Vault file does not exist at {:?}", self.path));
        }

        let file_bytes = fs::read(&self.path).context("Failed to read vault file")?;
        if file_bytes.len() < 5 {
            return Err(anyhow!("Vault file is truncated or corrupted"));
        }
        if &file_bytes[0..4] != b"HVLT" {
            return Err(anyhow!("Invalid vault magic header"));
        }

        let version = file_bytes[4];

        // --- V1 ---
        if version == 1 {
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
            let raw: HashMap<String, String> =
                bincode::deserialize(&plaintext).context("Failed to deserialize secrets map")?;
            return Ok(raw
                .into_iter()
                .map(|(k, v)| (k, TaintedString::credential(v)))
                .collect());
        }

        // --- V2 ---
        if version == 2 {
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
                let decrypted =
                    decrypt(&meta.slot_1.encrypted_master_key, &slot_key, &meta.slot_1.nonce)
                        .context("Failed to decrypt master key using Admin Key File")?;
                master_key.copy_from_slice(&decrypted[0..32]);
            } else {
                if !meta.slot_0.active {
                    return Err(anyhow!("Slot 0 (Passphrase PIN) is not active in this vault"));
                }
                let slot_key = derive_key(passphrase, &meta.slot_0.salt)?;
                let decrypted =
                    decrypt(&meta.slot_0.encrypted_master_key, &slot_key, &meta.slot_0.nonce)
                        .context("Failed to decrypt master key using Passphrase PIN")?;
                master_key.copy_from_slice(&decrypted[0..32]);
            }

            let plaintext = decrypt(&meta.encrypted_payload, &master_key, &meta.master_key_nonce)
                .context("Failed to decrypt vault payload")?;
            let raw: HashMap<String, String> =
                bincode::deserialize(&plaintext).context("Failed to deserialize secrets map")?;
            return Ok(raw
                .into_iter()
                .map(|(k, v)| (k, TaintedString::credential(v)))
                .collect());
        }

        // --- V3 ---
        if version == 3 {
            let meta: VaultMetadataV3 = bincode::deserialize(&file_bytes)
                .context("Failed to deserialize V3 vault metadata")?;

            let master_key = if passphrase.starts_with("dev_recovery_file:") {
                // Slot 2: Developer recovery via offline private key file.
                let dev_key_path = passphrase.trim_start_matches("dev_recovery_file:");
                let file_contents = fs::read_to_string(dev_key_path)
                    .context("Failed to read developer private key file")?;
                let dev_priv_seed = parse_dev_private_key_file(&file_contents)
                    .context("Failed to parse developer private key file")?;
                if !meta.slot_2.active {
                    return Err(anyhow!(
                        "Slot 2 (Developer Recovery) is not enabled in this vault"
                    ));
                }
                let mut user_pub_bytes = [0u8; 32];
                if meta.user_key_seed.len() != 32 {
                    return Err(anyhow!("user_key_seed in vault metadata has wrong length, expected 32 bytes"));
                }
                user_pub_bytes.copy_from_slice(&meta.user_key_seed);
                let slot2_key = crate::developer_recovery::derive_slot2_key_developer_side(
                    &dev_priv_seed,
                    &user_pub_bytes,
                );
                let decrypted =
                    decrypt(&meta.slot_2.encrypted_master_key, &slot2_key, &meta.slot_2.nonce)
                        .context("Failed to decrypt master key using Developer Recovery Key")?;
                let mut mk = [0u8; 32];
                mk.copy_from_slice(&decrypted[0..32]);
                mk
            } else if passphrase.starts_with("admin_key_file:") || PathBuf::from(passphrase).exists() {
                // Slot 1: Admin Key File.
                let key_file_path = if passphrase.starts_with("admin_key_file:") {
                    passphrase.trim_start_matches("admin_key_file:").to_string()
                } else {
                    passphrase.to_string()
                };
                if !meta.slot_1.active {
                    return Err(anyhow!("Slot 1 (Admin Key) is not active in this vault"));
                }
                let seed_hex = fs::read_to_string(&key_file_path)
                    .context("Failed to read admin key file")?;
                let slot1_key = derive_key(seed_hex.trim(), &meta.slot_1.salt)?;
                let decrypted =
                    decrypt(&meta.slot_1.encrypted_master_key, &slot1_key, &meta.slot_1.nonce)
                        .context("Failed to decrypt master key using Admin Key File")?;
                let mut mk = [0u8; 32];
                mk.copy_from_slice(&decrypted[0..32]);
                mk
            } else {
                // Slot 0: Passphrase PIN.
                if !meta.slot_0.active {
                    return Err(anyhow!("Slot 0 (Passphrase PIN) is not active in this vault"));
                }
                let slot0_key = derive_key(passphrase, &meta.slot_0.salt)?;
                let decrypted =
                    decrypt(&meta.slot_0.encrypted_master_key, &slot0_key, &meta.slot_0.nonce)
                        .context("Failed to decrypt master key using Passphrase PIN")?;
                let mut mk = [0u8; 32];
                mk.copy_from_slice(&decrypted[0..32]);
                mk
            };

            let plaintext = decrypt(&meta.encrypted_payload, &master_key, &meta.master_key_nonce)
                .context("Failed to decrypt vault payload")?;
            let raw: HashMap<String, String> =
                bincode::deserialize(&plaintext).context("Failed to deserialize secrets map")?;
            return Ok(raw
                .into_iter()
                .map(|(k, v)| (k, TaintedString::credential(v)))
                .collect());
        }

        Err(anyhow!("Unsupported vault version: {}", version))
    }

    // -----------------------------------------------------------------------
    // Save secrets (V2 format for existing vaults; V3 written by init_v3/save_v3)
    // -----------------------------------------------------------------------

    pub fn save(
        &self,
        passphrase: &str,
        secrets: &HashMap<String, TaintedString>,
    ) -> anyhow::Result<()> {
        // If this is a V3 vault, delegate to save_v3.
        if self.path.exists() {
            if let Ok(v) = fs::read(&self.path) {
                if v.len() >= 5 && &v[0..4] == b"HVLT" && v[4] == 3 {
                    return self.save_v3(passphrase, secrets);
                }
            }
        }

        // V2 save path (unchanged).
        let (is_admin_key, key_bytes) = if passphrase.starts_with("admin_key_file:") {
            let path = passphrase.trim_start_matches("admin_key_file:");
            (true, fs::read(path).context("Failed to read admin key file")?)
        } else if PathBuf::from(passphrase).exists() {
            (true, fs::read(passphrase).context("Failed to read admin key file")?)
        } else {
            (false, passphrase.as_bytes().to_vec())
        };

        let mut master_key = [0u8; 32];
        let mut slot_0 = VaultSlot::inactive();
        let mut slot_1 = VaultSlot::inactive();

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
                    let decrypted =
                        decrypt(&slot_1.encrypted_master_key, &slot_key, &slot_1.nonce)
                            .context("Failed to decrypt master key using Admin Key File")?;
                    master_key.copy_from_slice(&decrypted[0..32]);
                } else {
                    if !slot_0.active {
                        return Err(anyhow!("Slot 0 (Passphrase PIN) is not active in this vault"));
                    }
                    let slot_key = derive_key(passphrase, &slot_0.salt)?;
                    let decrypted =
                        decrypt(&slot_0.encrypted_master_key, &slot_key, &slot_0.nonce)
                            .context("Failed to decrypt master key using Passphrase PIN")?;
                    master_key.copy_from_slice(&decrypted[0..32]);
                }
            } else if let Ok(v1_bytes) = fs::read(&self.path) {
                if v1_bytes.len() >= 61 && &v1_bytes[0..4] == b"HVLT" && v1_bytes[4] == 1 {
                    let mut salt = [0u8; 32];
                    salt.copy_from_slice(&v1_bytes[5..37]);
                    if is_admin_key {
                        return Err(anyhow!(
                            "Cannot migrate V1 vault using Admin Key File"
                        ));
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

    /// Save secrets to an existing V3 vault. Authenticates using Slot 0 or Slot 1
    /// (Slot 2 / developer recovery is read-only and cannot be used to write).
    fn save_v3(
        &self,
        passphrase: &str,
        secrets: &HashMap<String, TaintedString>,
    ) -> anyhow::Result<()> {
        let mut meta = self.load_meta_v3()?;

        // Authenticate and extract master key.
        let master_key = if passphrase.starts_with("admin_key_file:") || PathBuf::from(passphrase).exists() {
            let key_file_path = if passphrase.starts_with("admin_key_file:") {
                passphrase.trim_start_matches("admin_key_file:").to_string()
            } else {
                passphrase.to_string()
            };
            if !meta.slot_1.active {
                return Err(anyhow!("Slot 1 (Admin Key) is not active in this vault"));
            }
            let seed_hex = fs::read_to_string(&key_file_path)
                .context("Failed to read admin key file")?;
            let slot1_key = derive_key(seed_hex.trim(), &meta.slot_1.salt)?;
            let decrypted =
                decrypt(&meta.slot_1.encrypted_master_key, &slot1_key, &meta.slot_1.nonce)
                    .context("Failed to decrypt master key using Admin Key File")?;
            let mut mk = [0u8; 32];
            mk.copy_from_slice(&decrypted[0..32]);
            mk
        } else {
            if !meta.slot_0.active {
                return Err(anyhow!("Slot 0 (Passphrase PIN) is not active in this vault"));
            }
            let slot0_key = derive_key(passphrase, &meta.slot_0.salt)?;
            let decrypted =
                decrypt(&meta.slot_0.encrypted_master_key, &slot0_key, &meta.slot_0.nonce)
                    .context("Failed to decrypt master key using Passphrase PIN")?;
            let mut mk = [0u8; 32];
            mk.copy_from_slice(&decrypted[0..32]);
            mk
        };

        // Re-encrypt the secrets payload with the same master key, new nonce.
        let raw: HashMap<String, String> = secrets
            .iter()
            .map(|(k, v)| (k.clone(), v.expose_secret().to_string()))
            .collect();
        let plaintext = bincode::serialize(&raw).context("Failed to serialize secrets")?;
        let mut payload_nonce = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut payload_nonce);
        meta.encrypted_payload = encrypt(&plaintext, &master_key, &payload_nonce)?;
        meta.master_key_nonce = payload_nonce;

        let file_bytes =
            bincode::serialize(&meta).context("Failed to serialize V3 vault metadata")?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).context("Failed to create vault directory")?;
        }
        fs::write(&self.path, file_bytes).context("Failed to write vault file")?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Passphrase recovery (V3 only)
    // -----------------------------------------------------------------------

    /// Reset the passphrase PIN (Slot 0) using the Admin Key File as authorization.
    ///
    /// The Admin Key File (`admin_key.bin`) proves the user's identity. Without it,
    /// Slot 0 cannot be changed — this prevents remote attackers from resetting the PIN.
    pub fn recover_passphrase(
        &self,
        admin_key_path: &Path,
        new_passphrase: &str,
    ) -> anyhow::Result<()> {
        let mut meta = self.load_meta_v3().context(
            "recover_passphrase requires a V3 vault. Use `vault init-v3` to create one.",
        )?;

        // Authenticate via Slot 1 (Admin Key File).
        if !meta.slot_1.active {
            return Err(anyhow!(
                "Slot 1 (Admin Key) is not active. Cannot recover passphrase without the admin key file."
            ));
        }
        let seed_hex = fs::read_to_string(admin_key_path)
            .context("Failed to read admin key file")?;
        let slot1_key = derive_key(seed_hex.trim(), &meta.slot_1.salt)?;
        let decrypted =
            decrypt(&meta.slot_1.encrypted_master_key, &slot1_key, &meta.slot_1.nonce)
                .context("Admin key file rejected — wrong file or tampered?")?;
        let mut master_key = [0u8; 32];
        master_key.copy_from_slice(&decrypted[0..32]);

        // Re-encrypt Slot 0 with the new passphrase.
        meta.slot_0.active = true;
        rand::thread_rng().fill_bytes(&mut meta.slot_0.salt);
        rand::thread_rng().fill_bytes(&mut meta.slot_0.nonce);
        let new_slot0_key = derive_key(new_passphrase, &meta.slot_0.salt)?;
        meta.slot_0.encrypted_master_key =
            encrypt(&master_key, &new_slot0_key, &meta.slot_0.nonce)?;

        // Update admin auth hash.
        let _ = setup_admin_auth(new_passphrase);

        // Write back.
        let file_bytes =
            bincode::serialize(&meta).context("Failed to serialize V3 vault metadata")?;
        fs::write(&self.path, file_bytes).context("Failed to write vault file")?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Admin key setup (V2 legacy)
    // -----------------------------------------------------------------------

    pub fn set_admin_key(&self, passphrase_pin: &str, admin_key_path: PathBuf) -> anyhow::Result<()> {
        let _secrets = self.load(passphrase_pin)?;
        let admin_key_bytes = fs::read(&admin_key_path).context("Failed to read admin key file")?;
        let key_hex = hex::encode(&admin_key_bytes);

        let mut meta = self.load_meta_v2().unwrap_or_else(|_| VaultMetadataV2 {
            magic: *b"HVLT",
            version: 2,
            master_key_nonce: [0u8; 24],
            slot_0: VaultSlot::inactive(),
            slot_1: VaultSlot::inactive(),
            encrypted_payload: vec![],
        });

        let mut master_key = [0u8; 32];
        if meta.slot_0.active {
            let slot_key = derive_key(passphrase_pin, &meta.slot_0.salt)?;
            let decrypted =
                decrypt(&meta.slot_0.encrypted_master_key, &slot_key, &meta.slot_0.nonce)?;
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

    /// Generate an emergency recovery request to send to the developer.
    ///
    /// Requires the user's Ed25519 recovery identity private key (`id_priv_hex`).
    /// Verifies that it corresponds to the registered `recovery_identity_pub` in the vault metadata,
    /// generates an ephemeral X25519 session key, and creates a signed JSON request payload.
    /// Also saves the session state to `recovery_session.json` to allow offline recovery to resume later.
    pub fn generate_recovery_request(&self, id_priv_hex: &str) -> anyhow::Result<RecoveryRequestResult> {
        let meta = self.load_meta_v3().context("Failed to load V3 vault metadata")?;
        if !meta.slot_2.active {
            return Err(anyhow!("Developer Recovery (Slot 2) is not enabled in this vault"));
        }

        // 1. Parse Recovery Identity Private Key (Ed25519)
        let id_priv_bytes = hex::decode(id_priv_hex.trim())
            .context("Invalid hex in Recovery Identity Private Key")?;
        if id_priv_bytes.len() != 32 {
            return Err(anyhow!("Recovery Mnemonic / Private Key must be 32 bytes (64 hex characters)"));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&id_priv_bytes);
        let id_priv = ed25519_dalek::SigningKey::from_bytes(&seed);

        // Verify that id_priv matches the registered id_pub in metadata
        let derived_pub = id_priv.verifying_key();
        if derived_pub.to_bytes() != meta.recovery_identity_pub.as_slice() {
            return Err(anyhow!("Provided Recovery Private Key does not match the registered key in vault metadata"));
        }

        // 2. Generate X25519 session keypair
        let mut priv_r = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut priv_r);
        let pub_r = crate::developer_recovery::x25519_public(&priv_r);

        // 3. Construct request fields
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut nonce_bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let request_nonce = hex::encode(nonce_bytes);

        let user_public_key_hex = hex::encode(&meta.user_key_seed);
        let slot_2_ciphertext_hex = hex::encode(&meta.slot_2.encrypted_master_key);
        let slot_2_nonce_hex = hex::encode(&meta.slot_2.nonce);
        let pub_r_hex = hex::encode(&pub_r);

        // Create canonical sorted JSON map for signature verification parity with python
        let mut map = serde_json::Map::new();
        map.insert("nonce".to_string(), serde_json::Value::String(request_nonce));
        map.insert("pub_r".to_string(), serde_json::Value::String(pub_r_hex));
        map.insert("slot_2_ciphertext".to_string(), serde_json::Value::String(slot_2_ciphertext_hex));
        map.insert("slot_2_nonce".to_string(), serde_json::Value::String(slot_2_nonce_hex));
        map.insert("timestamp".to_string(), serde_json::Value::Number(serde_json::Number::from(timestamp)));
        map.insert("user_public_key".to_string(), serde_json::Value::String(user_public_key_hex));

        let canonical_str = serde_json::to_string(&map)?;

        // 4. Sign the request
        use ed25519_dalek::Signer;
        let sig = id_priv.sign(canonical_str.as_bytes());
        let signature_hex = hex::encode(sig.to_bytes());

        // 5. Construct full output JSON
        let full_payload = serde_json::json!({
            "request": map,
            "signature": signature_hex
        });
        let payload_json = serde_json::to_string_pretty(&full_payload)?;

        // 6. Save recovery session details
        let session_data = serde_json::json!({
            "priv_r": hex::encode(priv_r),
            "user_public_key": hex::encode(&meta.user_key_seed),
            "timestamp": timestamp
        });
        
        let session_dir = self.path.parent().unwrap_or(Path::new(".")).join("recovery");
        fs::create_dir_all(&session_dir)?;
        let session_file_path = session_dir.join("recovery_session.json");
        fs::write(&session_file_path, serde_json::to_string_pretty(&session_data)?)?;

        Ok(RecoveryRequestResult {
            payload_json,
            session_file_path,
        })
    }

    /// Apply the WrappedResponse received from the developer to fully recover and rebuild the vault.
    ///
    /// This will:
    /// 1. Load the ephemeral `priv_r` from the saved recovery session.
    /// 2. Derive the session key and decrypt the recovered MasterKey.
    /// 3. Force setting a new passphrase PIN for Slot 0.
    /// 4. Regenerate a new Admin Key file for Slot 1.
    /// 5. Re-encrypt Slot 2 (if active) under the new keys.
    /// 6. Rotate the Recovery Identity Keypair (Ed25519) and print the new recovery phrase.
    /// 7. Securely zeroize all secret material from memory.
    pub fn apply_recovery_response(
        &self,
        response_json_str: &str,
        new_passphrase: &str,
    ) -> anyhow::Result<RecoveryApplyResult> {
        let mut meta = self.load_meta_v3().context("Failed to load V3 vault metadata")?;

        // 1. Read recovery session details
        let session_dir = self.path.parent().unwrap_or(Path::new(".")).join("recovery");
        let session_file_path = session_dir.join("recovery_session.json");
        if !session_file_path.exists() {
            return Err(anyhow!("Recovery session file not found at {:?}. Did you generate a request first?", session_file_path));
        }

        let session_content = fs::read_to_string(&session_file_path)?;
        let session_json: serde_json::Value = serde_json::from_str(&session_content)?;
        let priv_r_hex = session_json["priv_r"].as_str().ok_or_else(|| anyhow!("Missing priv_r in session file"))?;
        let priv_r_bytes = hex::decode(priv_r_hex)?;
        if priv_r_bytes.len() != 32 {
            return Err(anyhow!("Invalid priv_r length in session file"));
        }
        let mut priv_r = [0u8; 32];
        priv_r.copy_from_slice(&priv_r_bytes);

        // 2. Parse WrappedResponse
        let response_json: serde_json::Value = serde_json::from_str(response_json_str.trim())?;
        let ciphertext_hex = response_json["ciphertext"].as_str().ok_or_else(|| anyhow!("Missing ciphertext in response"))?;
        let nonce_hex = response_json["nonce"].as_str().ok_or_else(|| anyhow!("Missing nonce in response"))?;
        
        let ciphertext = hex::decode(ciphertext_hex)?;
        let response_nonce_vec = hex::decode(nonce_hex)?;
        if response_nonce_vec.len() != 24 {
            return Err(anyhow!("response nonce must be 24 bytes"));
        }
        let mut response_nonce = [0u8; 24];
        response_nonce.copy_from_slice(&response_nonce_vec);

        // 3. Derive SessionKey: SHA-256(X25519(priv_r, DEV_PUBLIC_KEY))
        let dev_pub_bytes = crate::developer_recovery::dev_public_key_bytes();
        let session_shared = crate::developer_recovery::x25519_dh(&priv_r, &dev_pub_bytes);
        
        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        hasher.update(&session_shared);
        let session_key = hasher.finalize();
        let mut session_key_arr = [0u8; 32];
        session_key_arr.copy_from_slice(&session_key);

        // 4. Decrypt MasterKey from response
        let decrypted_master_key = decrypt(&ciphertext, &session_key_arr, &response_nonce)
            .context("Failed to decrypt WrappedResponse. Key exchange mismatch or corrupted response.")?;
        if decrypted_master_key.len() != 32 {
            return Err(anyhow!("Decrypted MasterKey has invalid length, expected 32 bytes"));
        }
        let mut master_key = [0u8; 32];
        master_key.copy_from_slice(&decrypted_master_key);

        // 5. Encrypt MasterKey into Slot 0 with new passphrase PIN
        meta.slot_0.active = true;
        rand::thread_rng().fill_bytes(&mut meta.slot_0.salt);
        rand::thread_rng().fill_bytes(&mut meta.slot_0.nonce);
        let slot0_key = derive_key(new_passphrase, &meta.slot_0.salt)?;
        meta.slot_0.encrypted_master_key = encrypt(&master_key, &slot0_key, &meta.slot_0.nonce)?;

        // 6. Generate a new user private key and write new Slot 1 (Admin Key)
        let mut new_user_private = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut new_user_private);
        let new_seed_hex = hex::encode(new_user_private);

        meta.slot_1.active = true;
        rand::thread_rng().fill_bytes(&mut meta.slot_1.salt);
        rand::thread_rng().fill_bytes(&mut meta.slot_1.nonce);
        let slot1_key = derive_key(&new_seed_hex, &meta.slot_1.salt)?;
        meta.slot_1.encrypted_master_key = encrypt(&master_key, &slot1_key, &meta.slot_1.nonce)?;

        // Update the user_key_seed to the new public key
        let new_user_pub = crate::developer_recovery::x25519_public(&new_user_private);
        meta.user_key_seed = new_user_pub.to_vec();

        // 7. Generate a new Recovery Identity Keypair
        let mut new_id_seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut new_id_seed);
        let new_id_priv = ed25519_dalek::SigningKey::from_bytes(&new_id_seed);
        let new_id_pub = new_id_priv.verifying_key();
        meta.recovery_identity_pub = new_id_pub.to_bytes().to_vec();

        // 8. Re-encrypt Slot 2 (if active)
        if meta.slot_2.active {
            rand::thread_rng().fill_bytes(&mut meta.slot_2.salt);
            rand::thread_rng().fill_bytes(&mut meta.slot_2.nonce);
            let slot2_key = derive_slot2_key_user_side(&new_user_private);
            meta.slot_2.encrypted_master_key = encrypt(&master_key, &slot2_key, &meta.slot_2.nonce)?;
        }

        // 9. Save updated V3 vault
        let file_bytes = bincode::serialize(&meta).context("Failed to serialize V3 vault metadata")?;
        fs::write(&self.path, file_bytes).context("Failed to write updated vault file")?;

        // 10. Write the new admin key file to vault directory
        let admin_key_path = self.path.parent().unwrap_or(Path::new(".")).join("admin_key.bin");
        fs::write(&admin_key_path, new_seed_hex.as_bytes()).context("Failed to write new admin key file")?;

        // 11. Cleanup temporary session files and zeroize secrets in memory
        let _ = fs::remove_file(&session_file_path);
        
        use zeroize::Zeroize;
        priv_r.zeroize();
        master_key.zeroize();
        new_id_seed.zeroize();

        Ok(RecoveryApplyResult {
            new_id_priv_hex: hex::encode(new_id_priv.to_bytes()),
            new_id_pub_hex: hex::encode(new_id_pub.to_bytes()),
            admin_key_path,
        })
    }
}

pub struct RecoveryRequestResult {
    pub payload_json: String,
    pub session_file_path: PathBuf,
}

pub struct RecoveryApplyResult {
    pub new_id_priv_hex: String,
    pub new_id_pub_hex: String,
    pub admin_key_path: PathBuf,
}

// ---------------------------------------------------------------------------
// Admin auth hash helper
// ---------------------------------------------------------------------------

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
