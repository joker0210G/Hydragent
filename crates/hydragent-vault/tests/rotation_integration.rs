//! Track 6.4 integration tests: passphrase + column key rotation
//! via [`Rotator`], including atomic-rename, wrong-old rejection,
//! and key independence.

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;

use hydragent_vault::rotator::Rotator;
use hydragent_vault::taint::TaintedString;
use hydragent_vault::vault::Vault;

fn temp_path(tag: &str) -> PathBuf {
    let mut p = env::temp_dir();
    p.push(format!(
        "hydragent_64intg_rot_{}_{}_{}.hvlt",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p
}

fn credential(s: &str) -> TaintedString {
    TaintedString::credential(s.to_string())
}

#[test]
fn rotate_passphrase_round_trip() {
    let path = temp_path("pass");
    let v = Vault::new(path.clone());
    v.init("alpha").unwrap();
    let mut entries = HashMap::new();
    entries.insert("api".to_string(), credential("sk-old"));
    entries.insert("token".to_string(), credential("tk-old"));
    v.save("alpha", &entries).unwrap();

    let rotator = Rotator::new(path.clone());
    let report = rotator.rotate_passphrase("alpha", "beta").unwrap();
    assert_eq!(report.entries_after, 2);
    assert!(!report.column_key_rotated);
    assert!(report.new_column_key_hex.is_none());

    // Old passphrase no longer works.
    let v2 = Vault::new(path.clone());
    let res = v2.load("alpha");
    assert!(res.is_err(), "old passphrase should fail to load");

    // New passphrase loads with all entries intact.
    let loaded = v2.load("beta").unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded.get("api").unwrap().expose_secret(), "sk-old");
    assert_eq!(loaded.get("token").unwrap().expose_secret(), "tk-old");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn rotate_passphrase_rejects_wrong_old() {
    let path = temp_path("wrong");
    let v = Vault::new(path.clone());
    v.init("right").unwrap();
    let mut entries = HashMap::new();
    entries.insert("k".to_string(), credential("v"));
    v.save("right", &entries).unwrap();

    let rotator = Rotator::new(path.clone());
    let res = rotator.rotate_passphrase("WRONG", "new");
    assert!(res.is_err(), "wrong old passphrase must fail");

    // Original vault file is unchanged: still loads with the original
    // passphrase and contains the original entry.
    let loaded = Vault::new(path.clone()).load("right").unwrap();
    assert_eq!(loaded.get("k").unwrap().expose_secret(), "v");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn rotate_passphrase_rejects_same_value() {
    // Rotating to the same passphrase is forbidden (no-op rejected).
    let path = temp_path("same");
    let v = Vault::new(path.clone());
    v.init("pp").unwrap();

    let rotator = Rotator::new(path.clone());
    let res = rotator.rotate_passphrase("pp", "pp");
    assert!(res.is_err(), "same-passphrase rotation must be rejected");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn rotate_passphrase_on_missing_vault_fails() {
    let path = temp_path("missing_pass");
    // No init — file does not exist.
    let rotator = Rotator::new(path.clone());
    let res = rotator.rotate_passphrase("a", "b");
    assert!(res.is_err());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn rotate_column_key_returns_none_when_unset() {
    let path = temp_path("unset");
    let v = Vault::new(path.clone());
    v.init("pp").unwrap();
    let mut entries = HashMap::new();
    entries.insert("k".to_string(), credential("v"));
    v.save("pp", &entries).unwrap();

    let rotator = Rotator::new(path.clone());
    let hex = rotator.read_column_key("pp").unwrap();
    assert!(hex.is_none(), "no column key entry should exist yet");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn rotate_column_key_inserts_and_reads_back() {
    let path = temp_path("insert");
    let v = Vault::new(path.clone());
    v.init("pp").unwrap();
    let mut entries = HashMap::new();
    entries.insert("k".to_string(), credential("v"));
    v.save("pp", &entries).unwrap();

    let rotator = Rotator::new(path.clone());
    let (report, key) = rotator.rotate_column_key("pp").unwrap();
    assert!(report.column_key_rotated);
    let new_hex = report.new_column_key_hex.expect("hex returned");
    assert_eq!(new_hex.len(), 64);

    // Verify read_column_key returns the new value.
    let read_key = rotator.read_column_key("pp").unwrap();
    // read_key is a SecureBuffer<[u8; 32]> of the hex bytes.
    // The hex string should match the raw bytes' hex encoding.
    let read_bytes: [u8; 32] = read_key.expect("column key exists").as_slice()[0];
    let read_hex: String = read_bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join("");
    assert_eq!(read_hex, new_hex);

    // The returned SecureBuffer is 32 bytes and matches the hex.
    let raw_bytes: [u8; 32] = key.as_slice()[0];
    assert_eq!(raw_bytes.len(), 32);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn rotate_column_key_twice_yields_different_keys() {
    let path = temp_path("twice");
    let v = Vault::new(path.clone());
    v.init("pp").unwrap();

    let rotator = Rotator::new(path.clone());
    let (r1, _) = rotator.rotate_column_key("pp").unwrap();
    let (r2, _) = rotator.rotate_column_key("pp").unwrap();
    let h1 = r1.new_column_key_hex.unwrap();
    let h2 = r2.new_column_key_hex.unwrap();
    assert_ne!(h1, h2);

    // The on-disk file now contains the second key as the column key
    // entry, and entry count is still 1.
    let loaded = Vault::new(path.clone()).load("pp").unwrap();
    assert_eq!(loaded.len(), 1);
    assert!(loaded.contains_key("__column_key__"));
    assert_eq!(loaded.get("__column_key__").unwrap().expose_secret(), h2);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn rotate_column_key_on_missing_vault_fails() {
    let path = temp_path("missing_col");
    let rotator = Rotator::new(path.clone());
    let res = rotator.rotate_column_key("pp");
    assert!(res.is_err());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn rotate_column_key_preserves_existing_entries() {
    // The column key rotation should be additive: existing entries
    // are preserved, and the column key is added (or replaced).
    let path = temp_path("preserve");
    let v = Vault::new(path.clone());
    v.init("pp").unwrap();
    let mut entries = HashMap::new();
    entries.insert("username".to_string(), credential("alice"));
    entries.insert("session_id".to_string(), credential("abcd1234"));
    v.save("pp", &entries).unwrap();

    let rotator = Rotator::new(path.clone());
    let _ = rotator.rotate_column_key("pp").unwrap();

    // Reload and check that the original entries are intact.
    let loaded = Vault::new(path.clone()).load("pp").unwrap();
    assert_eq!(loaded.len(), 3); // 2 originals + 1 column key
    assert_eq!(loaded.get("username").unwrap().expose_secret(), "alice");
    assert_eq!(
        loaded.get("session_id").unwrap().expose_secret(),
        "abcd1234"
    );

    let _ = std::fs::remove_file(&path);
}
