use anyhow::anyhow;
use argon2::{Argon2, Algorithm, Version, Params};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};

/// Derive a 256-bit key from a passphrase and salt using Argon2id.
pub fn derive_key(passphrase: &str, salt: &[u8; 32]) -> anyhow::Result<[u8; 32]> {
    let params = Params::new(65536, 3, 1, Some(32))
        .map_err(|e| anyhow!("Failed to initialize Argon2 params: {}", e))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow!("Argon2 key derivation failed: {}", e))?;
    Ok(key)
}

/// Encrypt the plaintext using XChaCha20-Poly1305.
pub fn encrypt(plaintext: &[u8], key: &[u8; 32], nonce: &[u8; 24]) -> anyhow::Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let x_nonce = XNonce::from_slice(nonce);
    let ciphertext = cipher
        .encrypt(x_nonce, plaintext)
        .map_err(|e| anyhow!("Encryption failed: {:?}", e))?;
    Ok(ciphertext)
}

/// Decrypt the ciphertext using XChaCha20-Poly1305.
pub fn decrypt(ciphertext: &[u8], key: &[u8; 32], nonce: &[u8; 24]) -> anyhow::Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let x_nonce = XNonce::from_slice(nonce);
    let plaintext = cipher
        .decrypt(x_nonce, ciphertext)
        .map_err(|e| anyhow!("Decryption failed: {:?}", e))?;
    Ok(plaintext)
}
