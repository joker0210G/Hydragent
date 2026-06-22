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

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    let mut key_block = [0u8; 64];
    if key.len() > 64 {
        let hash = Sha256::digest(key);
        key_block[..32].copy_from_slice(&hash);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    for i in 0..64 {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }
    let mut inner = Sha256::new();
    inner.update(&ipad);
    inner.update(data);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(&inner_hash);
    let outer_hash = outer.finalize();
    outer_hash.into()
}

pub fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32, dk: &mut [u8; 32]) {
    let mut block = [0u8; 36];
    block[..salt.len()].copy_from_slice(salt);
    block[salt.len()..salt.len() + 4].copy_from_slice(&1u32.to_be_bytes());

    let mut u = hmac_sha256(password, &block[..salt.len() + 4]);
    let mut f = u;

    for _ in 1..iterations {
        u = hmac_sha256(password, &u);
        for j in 0..32 {
            f[j] ^= u[j];
        }
    }
    dk.copy_from_slice(&f);
}
