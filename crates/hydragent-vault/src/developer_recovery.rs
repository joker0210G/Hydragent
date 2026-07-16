//! Developer Emergency Recovery — Slot 2 cryptographic helpers using Curve25519 Montgomery arithmetic.
//!
//! The developer holds a 128-byte private key seed (256 hex chars), stored **offline**
//! in `config/security/dev_private_key.txt`. Only the first 32 bytes are used as the X25519 private key.
//! The matching 32-byte public key is compiled into this file as `DEV_PUBLIC_KEY_HEX`.
//!
//! ## Mathematical Flow
//!
//! ### Slot 2 key derivation (user side, at vault init):
//! ```text
//! shared_secret = X25519(user_private_key, DEV_PUBLIC_KEY)
//! slot2_key     = SHA-256(shared_secret)
//! ```
//!
//! ### Slot 2 key derivation (developer side, at recovery):
//! ```text
//! shared_secret = X25519(DEV_PRIVATE_KEY, user_public_key)
//! slot2_key     = SHA-256(shared_secret)
//! ```

use sha2::{Digest, Sha256};
use curve25519_dalek::montgomery::MontgomeryPoint;
use curve25519_dalek::scalar::Scalar;

/// Developer public key — 32 bytes (64 hex characters).
///
/// This is the X25519 public key of the developer. It is safe to embed.
pub const DEV_PUBLIC_KEY_HEX: &str = "9e93ff950bff753916be592aadf86f436f4d83478dcc26e89daaf1a978cf381f";

/// Standard X25519 scalar clamping.
pub fn clamp(mut key: [u8; 32]) -> [u8; 32] {
    key[0] &= 248;
    key[31] &= 127;
    key[31] |= 64;
    key
}

/// Derive the X25519 public key from a private key.
pub fn x25519_public(private_key: &[u8; 32]) -> [u8; 32] {
    let clamped = clamp(*private_key);
    let scalar = Scalar::from_bytes_mod_order(clamped);
    let base_point = MontgomeryPoint([
        9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ]);
    let public_point = base_point * scalar;
    public_point.to_bytes()
}

/// Perform X25519 DH key exchange.
pub fn x25519_dh(private_key: &[u8; 32], other_public: &[u8; 32]) -> [u8; 32] {
    let clamped = clamp(*private_key);
    let scalar = Scalar::from_bytes_mod_order(clamped);
    let public_point = MontgomeryPoint(*other_public);
    let shared_point = public_point * scalar;
    shared_point.to_bytes()
}

/// Decode the developer public key bytes.
pub fn dev_public_key_bytes() -> [u8; 32] {
    let bytes = hex::decode(DEV_PUBLIC_KEY_HEX)
        .expect("DEV_PUBLIC_KEY_HEX must be valid 32-byte hex");
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    arr
}

/// Derive Slot 2 key using the user's private key (32 bytes) and developer's public key.
///
/// `slot2_key = SHA-256(X25519(user_private_key, DEV_PUBLIC_KEY))`
pub fn derive_slot2_key_user_side(user_private_bytes: &[u8; 32]) -> [u8; 32] {
    let dev_pub = dev_public_key_bytes();
    let shared = x25519_dh(user_private_bytes, &dev_pub);
    
    let mut hasher = Sha256::new();
    hasher.update(&shared);
    let result = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&result);
    key
}

/// Derive Slot 2 key using the developer's private key (32 bytes) and the user's public key (32 bytes).
///
/// `slot2_key = SHA-256(X25519(DEV_PRIVATE_KEY, user_public_key))`
pub fn derive_slot2_key_developer_side(
    dev_private_bytes: &[u8; 32],
    user_public_bytes: &[u8; 32],
) -> [u8; 32] {
    let shared = x25519_dh(dev_private_bytes, user_public_bytes);
    
    let mut hasher = Sha256::new();
    hasher.update(&shared);
    let result = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&result);
    key
}

/// Parse the developer private key bytes from the contents of `dev_private_key.txt`.
///
/// Extracts the 128-byte key seed and takes the first 32 bytes as the X25519 private key.
pub fn parse_dev_private_key_file(contents: &str) -> anyhow::Result<[u8; 32]> {
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(hex_val) = line.strip_prefix("DEVELOPER_PRIVATE_KEY=") {
            let bytes = hex::decode(hex_val.trim())
                .map_err(|e| anyhow::anyhow!("Invalid hex in DEVELOPER_PRIVATE_KEY: {}", e))?;
            if bytes.len() < 32 {
                return Err(anyhow::anyhow!(
                    "DEVELOPER_PRIVATE_KEY must be at least 32 bytes (64 hex chars), got {} bytes",
                    bytes.len()
                ));
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes[0..32]);
            return Ok(arr);
        }
    }
    Err(anyhow::anyhow!(
        "DEVELOPER_PRIVATE_KEY= line not found in developer key file"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot2_key_user_and_dev_side_match() {
        // Generate actual private keys
        let mut dev_priv = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut dev_priv);
        let dev_public = x25519_public(&dev_priv);

        let mut user_priv = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut user_priv);
        let user_public = x25519_public(&user_priv);

        // Derive user-side shared secret using dev_public
        let user_shared = x25519_dh(&user_priv, &dev_public);
        let mut h1 = Sha256::new();
        h1.update(&user_shared);
        let user_key: [u8; 32] = h1.finalize().into();

        // Derive dev-side shared secret using user_public
        let dev_shared = x25519_dh(&dev_priv, &user_public);
        let mut h2 = Sha256::new();
        h2.update(&dev_shared);
        let dev_key: [u8; 32] = h2.finalize().into();

        assert_eq!(user_key, dev_key, "Both sides must produce the same Slot 2 key");
    }
}
