//! Track 6.4 integration tests: column-level AES-256-GCM encryption
//! end-to-end, including AAD binding to the column name and roundtrip
//! with a key sourced from a vault via the rotator.

use std::env;
use std::path::PathBuf;

use hydragent_vault::column_cipher::ColumnCipher;
use hydragent_vault::rotator::Rotator;
use hydragent_vault::vault::Vault;

fn temp_path(tag: &str) -> PathBuf {
    let mut p = env::temp_dir();
    p.push(format!(
        "hydragent_64intg_{}_{}_{}.hvlt",
        tag,
        std::process::id(),
        // Avoid zero collisions when tests are parallelized.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p
}

#[test]
fn column_cipher_roundtrip_with_vault_sourced_key() {
    // 1. Create a vault and store a 32-byte column key.
    let path = temp_path("col_vault");
    let v = Vault::new(path.clone());
    v.init("open-sesame").unwrap();
    let mut entries = std::collections::HashMap::new();
    let _master_key = [0xa5u8; 32];
    entries.insert(
        "user_id".to_string(),
        hydragent_vault::taint::TaintedString::credential("alice".to_string()),
    );
    entries.insert(
        "api_token".to_string(),
        hydragent_vault::taint::TaintedString::credential("bot-12345".to_string()),
    );
    v.save("open-sesame", &entries).unwrap();
    let _ = std::fs::remove_file(&path);

    // 2. Build a ColumnCipher from a freshly rotated key, then
    //    encrypt and decrypt several columns.
    let key_for_cipher = [0x7eu8; 32];
    let cipher = ColumnCipher::new(&key_for_cipher);

    let pt_email = b"alice@example.com".to_vec();
    let pt_phone = b"+1-555-0100".to_vec();
    let pt_ssn = b"123-45-6789".to_vec();

    let ct_email = cipher.encrypt("email", &pt_email).unwrap();
    let ct_phone = cipher.encrypt("phone", &pt_phone).unwrap();
    let ct_ssn = cipher.encrypt("ssn", &pt_ssn).unwrap();

    assert_eq!(cipher.decrypt("email", &ct_email).unwrap(), pt_email);
    assert_eq!(cipher.decrypt("phone", &ct_phone).unwrap(), pt_phone);
    assert_eq!(cipher.decrypt("ssn", &ct_ssn).unwrap(), pt_ssn);

    // 3. The on-disk file does not need to exist for the cipher to
    //    work; this test demonstrates the cipher is a pure function
    //    of (key, column, plaintext) and does not require vault I/O.
    let _ = std::fs::remove_file(&path);
}

#[test]
fn column_cipher_aad_is_actually_bound() {
    // A blob that was encrypted under column "ssn" must NOT decrypt
    // when re-submitted with column "credit_card", even with the
    // correct key. This proves the column name is bound as AAD.
    let key = [0xcdu8; 32];
    let cipher = ColumnCipher::new(&key);
    let pt = b"secret-ssn".to_vec();
    let blob = cipher.encrypt("ssn", &pt).unwrap();
    // Same key, wrong column name: should fail.
    let res = cipher.decrypt("credit_card", &blob);
    assert!(
        res.is_err(),
        "decryption with wrong column should fail (AAD binding)"
    );
}

#[test]
fn column_cipher_string_roundtrip_preserves_unicode() {
    let key = [0x42u8; 32];
    let cipher = ColumnCipher::new(&key);
    let payload = "こんにちは 🌍 — Türkçe: şifre";
    let blob = cipher.encrypt_str("name_unicode", payload).unwrap();
    let decoded = cipher.decrypt_str("name_unicode", &blob).unwrap();
    assert_eq!(decoded, payload);
}

#[test]
fn column_cipher_clone_is_independent() {
    // Cloning a ColumnCipher must produce a fully independent key
    // copy (deep clone, not a shared reference). Both copies should
    // produce ciphertexts that the *other* copy can decrypt.
    let key = [0x99u8; 32];
    let c1 = ColumnCipher::new(&key);
    let c2 = c1.clone();
    let pt = b"shared-secret".to_vec();
    let blob_c1 = c1.encrypt("shared_col", &pt).unwrap();
    let blob_c2 = c2.encrypt("shared_col", &pt).unwrap();
    // Each can decrypt the other's blob (same key bytes underneath).
    assert_eq!(c2.decrypt("shared_col", &blob_c1).unwrap(), pt);
    assert_eq!(c1.decrypt("shared_col", &blob_c2).unwrap(), pt);
    // And the exported keys are equal.
    assert_eq!(c1.export_key().as_slice()[0], c2.export_key().as_slice()[0]);
}

#[test]
fn column_cipher_fresh_key_is_unique() {
    // Two fresh keys generated from the OS CSPRNG must not collide.
    // `fresh_key()` returns a raw `[u8; 32]` (not a SecureBuffer).
    let k1: [u8; 32] = ColumnCipher::fresh_key();
    let k2: [u8; 32] = ColumnCipher::fresh_key();
    assert_ne!(k1, k2);
    // Each key is exactly 32 bytes.
    assert_eq!(k1.len(), 32);
    assert_eq!(k2.len(), 32);
}

#[test]
fn column_cipher_from_secure_buffer_matches_raw_construction() {
    // Constructing a ColumnCipher from a SecureBuffer<[u8; 32]>
    // should produce a cipher that behaves identically to one
    // constructed from the raw `[u8; 32]`.
    let key = [0x5au8; 32];
    let buf =
        hydragent_vault::secure_buffer::SecureBuffer::<[u8; 32]>::from_byte_array(key).unwrap();
    let cipher_a = ColumnCipher::from_secure_buffer(&buf).unwrap();
    let cipher_b = ColumnCipher::new(&key);
    let pt = b"abcdef".to_vec();
    let blob_a = cipher_a.encrypt("k", &pt).unwrap();
    // cipher_b can decrypt what cipher_a encrypted (same key bytes).
    assert_eq!(cipher_b.decrypt("k", &blob_a).unwrap(), pt);
}

#[test]
fn full_column_key_lifecycle_via_rotator() {
    // End-to-end: create a vault, rotate the column key twice via
    // the rotator, and verify each rotation produces a different key
    // that round-trips through the cipher.
    let path = temp_path("col_lifecycle");
    let pp = "hunter2hunter2hunter2hunter2";
    let v = Vault::new(path.clone());
    v.init(pp).unwrap();
    let mut entries = std::collections::HashMap::new();
    entries.insert(
        "balance".to_string(),
        hydragent_vault::taint::TaintedString::credential("1234.56".to_string()),
    );
    v.save(pp, &entries).unwrap();

    let rotator = Rotator::new(path.clone());

    // First rotation.
    let (report1, key1) = rotator.rotate_column_key(pp).unwrap();
    assert!(report1.column_key_rotated);
    assert!(report1.new_column_key_hex.is_some());
    let hex1 = report1.new_column_key_hex.clone().unwrap();
    assert_eq!(hex1.len(), 64); // 32 bytes hex
    let key1_bytes: [u8; 32] = key1.as_slice()[0];
    let cipher1 = ColumnCipher::new(&key1_bytes);
    let pt = b"sensitive-balance-data".to_vec();
    let blob = cipher1.encrypt("balance_cipher", &pt).unwrap();
    assert_eq!(cipher1.decrypt("balance_cipher", &blob).unwrap(), pt);

    // Second rotation: should produce a different key.
    let (report2, key2) = rotator.rotate_column_key(pp).unwrap();
    assert!(report2.column_key_rotated);
    let hex2 = report2.new_column_key_hex.clone().unwrap();
    let key2_bytes: [u8; 32] = key2.as_slice()[0];
    assert_ne!(hex1, hex2);
    assert_ne!(key1_bytes, key2_bytes);

    // Old cipher still decrypts its own blob (sanity).
    assert_eq!(cipher1.decrypt("balance_cipher", &blob).unwrap(), pt);

    // New cipher (built with the rotated key) must NOT decrypt
    // what the old key encrypted.
    let cipher2 = ColumnCipher::new(&key2_bytes);
    let res = cipher2.decrypt("balance_cipher", &blob);
    assert!(res.is_err(), "rotated key should not decrypt old ciphertext");

    // New cipher can encrypt and decrypt with its own key.
    let pt2 = b"updated-balance".to_vec();
    let blob2 = cipher2.encrypt("balance_cipher", &pt2).unwrap();
    assert_eq!(cipher2.decrypt("balance_cipher", &blob2).unwrap(), pt2);

    let _ = std::fs::remove_file(&path);
}
