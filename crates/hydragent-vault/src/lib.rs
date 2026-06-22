pub mod column_cipher;
pub mod crypto;
pub mod injector;
pub mod mlock;
pub mod rotator;
pub mod secure_buffer;
pub mod taint;
pub mod vault;

pub use column_cipher::{ColumnCipher, ColumnCipherError};
pub use crypto::{decrypt, derive_key, encrypt, hmac_sha256, pbkdf2_hmac_sha256};
pub use injector::{inject_str, inject_value, KeyInjector};
pub use mlock::{is_mlock_available, mlock, munlock, MlockError};
pub use rotator::{Rotator, RotationError, RotationReport};
pub use secure_buffer::{SecureBuffer, SecureBufferError};
pub use taint::{TaintCategory, TaintedString};
pub use vault::Vault;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_crypto_roundtrip() {
        let key = [0u8; 32];
        let nonce = [0u8; 24];
        let plaintext = b"Hello, World!";
        let ciphertext = crypto::encrypt(plaintext, &key, &nonce).unwrap();
        let decrypted = crypto::decrypt(&ciphertext, &key, &nonce).unwrap();
        assert_eq!(plaintext.to_vec(), decrypted);
    }

    #[test]
    fn test_kdf_deterministic() {
        let salt = [1u8; 32];
        let key1 = crypto::derive_key("password", &salt).unwrap();
        let key2 = crypto::derive_key("password", &salt).unwrap();
        assert_eq!(key1, key2);

        let key3 = crypto::derive_key("different", &salt).unwrap();
        assert_ne!(key1, key3);
    }

    #[test]
    fn test_vault_operations() {
        // Let's create a temporary vault file path
        let temp_dir = std::env::temp_dir();
        let vault_path = temp_dir.join("test_vault.hvlt");
        if vault_path.exists() {
            let _ = std::fs::remove_file(&vault_path);
        }

        let vault = Vault::new(vault_path.clone());
        assert!(!vault.exists());

        let passphrase = "vault_password";
        vault.init(passphrase).unwrap();
        assert!(vault.exists());

        let mut secrets = vault.load(passphrase).unwrap();
        assert!(secrets.is_empty());

        secrets.insert("api.key".to_string(), TaintedString::new("abc123xyz".to_string()));
        vault.save(passphrase, &secrets).unwrap();

        // Load and check
        let loaded = vault.load(passphrase).unwrap();
        assert_eq!(loaded.get("api.key").unwrap().expose_secret(), "abc123xyz");

        // Decrypt with wrong password should fail
        assert!(vault.load("wrong_password").is_err());

        let _ = std::fs::remove_file(&vault_path);
    }

    #[test]
    fn test_vault_init_idempotency() {
        // Re-init on an existing vault should fail
        let vault_path = std::env::temp_dir().join("test_vault_idempotency.hvlt");
        if vault_path.exists() {
            let _ = std::fs::remove_file(&vault_path);
        }
        let vault = Vault::new(vault_path.clone());
        vault.init("passphrase-1").unwrap();
        // Second init must fail
        let result = vault.init("passphrase-2");
        assert!(result.is_err(), "init on existing vault must fail");
        let _ = std::fs::remove_file(&vault_path);
    }

    #[test]
    fn test_injector_template_replacement() {
        let mut secrets = HashMap::new();
        secrets.insert("API_KEY".to_string(), TaintedString::new("secret_val".to_string()));

        let template = "Bearer {{API_KEY}} for user {{USER}}";
        let injected = inject_str(template, &secrets);
        assert_eq!(injected, "Bearer secret_val for user {{USER}}");
    }

    #[test]
    fn test_injector_role_based_injection() {
        // KeyInjector must replace {{SCOPE}} placeholders in `system`
        // and `tool` role messages, and must NOT touch `user` or
        // `assistant` role messages.
        let mut secrets = HashMap::new();
        secrets.insert(
            "GITHUB_TOKEN".to_string(),
            TaintedString::new("ghp_abcdef1234567890".to_string()),
        );
        let injector = KeyInjector::new(secrets);

        // (1) System role: replaced
        let (out, scopes) = injector.inject_message(
            "system",
            "You have access to {{GITHUB_TOKEN}} in your env.",
        );
        assert!(out.expose_secret().contains("ghp_abcdef1234567890"));
        assert!(!out.expose_secret().contains("{{GITHUB_TOKEN}}"));
        assert!(scopes.contains(&"GITHUB_TOKEN".to_string()));

        // (2) Tool role: replaced
        let (out, scopes) = injector.inject_message(
            "tool",
            "Result: status=200, auth={{GITHUB_TOKEN}}",
        );
        assert!(out.expose_secret().contains("ghp_abcdef1234567890"));
        assert!(scopes.contains(&"GITHUB_TOKEN".to_string()));

        // (3) User role: NOT replaced
        let (out, scopes) = injector.inject_message(
            "user",
            "Please use {{GITHUB_TOKEN}} for me.",
        );
        assert!(
            out.expose_secret().contains("{{GITHUB_TOKEN}}"),
            "user-role messages must NOT be injected, got: {}",
            out.expose_secret()
        );
        assert!(scopes.is_empty(), "no scopes should be reported for user role");

        // (4) Assistant role: NOT replaced
        let (out, _) = injector.inject_message(
            "assistant",
            "I'll use {{GITHUB_TOKEN}} for that.",
        );
        assert!(out.expose_secret().contains("{{GITHUB_TOKEN}}"));
    }

    #[test]
    fn test_injector_missing_scope_preserved() {
        // If a placeholder references a scope that's not in the vault,
        // the placeholder is preserved (no error, no replacement).
        let mut secrets = HashMap::new();
        secrets.insert("KNOWN".to_string(), TaintedString::new("value1".to_string()));
        let injector = KeyInjector::new(secrets);

        let (out, scopes) = injector.inject_message(
            "system",
            "Known: {{KNOWN}}, Unknown: {{NOT_THERE}}",
        );
        assert!(out.expose_secret().contains("value1"));
        assert!(out.expose_secret().contains("{{NOT_THERE}}"));
        assert_eq!(scopes, vec!["KNOWN".to_string()]);
    }

    #[test]
    fn test_taint_merge_propagation() {
        // Once tainted, always tainted: combining a credential with
        // an untrusted string gives a value that carries BOTH taint
        // tags. The serialized form must refuse to leak the secret.
        let cred = TaintedString::credential("ghp_secret123".to_string());
        let untrusted = TaintedString::untrusted("user's repo name".to_string());
        let merged = cred.merge(&untrusted);
        assert!(merged.has_taint(TaintCategory::Credential));
        assert!(merged.has_taint(TaintCategory::Untrusted));
        // Display redacts
        assert_eq!(format!("{}", merged), "[REDACTED]");
        // Serialize is refused
        let json = serde_json::to_string(&merged);
        assert!(json.is_err(), "tainted merge must not serialize");
    }

    #[test]
    fn test_vault_get_never_panics_on_missing_scope() {
        let vault_path = std::env::temp_dir().join("test_vault_missing_scope.hvlt");
        if vault_path.exists() {
            let _ = std::fs::remove_file(&vault_path);
        }
        let vault = Vault::new(vault_path.clone());
        vault.init("passphrase").unwrap();
        // Loading an empty vault must succeed
        let loaded = vault.load("passphrase").unwrap();
        assert!(loaded.get("never_set").is_none());
        let _ = std::fs::remove_file(&vault_path);
    }

    #[test]
    fn test_tainted_string_display_includes_credential_label() {
        // Debug form should NOT include the raw secret and SHOULD
        // include the taint category for audit log readability.
        let s = TaintedString::credential("abc".to_string());
        let dbg = format!("{:?}", s);
        assert!(!dbg.contains("abc"), "Debug leaked raw secret: {}", dbg);
        assert!(dbg.contains("credential"), "Debug missing taint label: {}", dbg);
    }

    #[test]
    fn test_vault_slot_1_admin_key() {
        let temp_dir = std::env::temp_dir();
        let vault_path = temp_dir.join("test_vault_slot1.hvlt");
        if vault_path.exists() {
            let _ = std::fs::remove_file(&vault_path);
        }

        let vault = Vault::new(vault_path.clone());
        let passphrase = "my_passphrase_pin";
        vault.init(passphrase).unwrap();

        // Create a fake admin key file
        let admin_key_path = temp_dir.join("admin_key.pem");
        std::fs::write(&admin_key_path, b"super_secret_physical_key_file_contents").unwrap();

        // Set Slot 1 using the admin key file
        vault.set_admin_key(passphrase, admin_key_path.clone()).unwrap();

        // Load the secrets map using Slot 0 (passphrase pin)
        let loaded_slot0 = vault.load(passphrase).unwrap();
        assert!(loaded_slot0.is_empty());

        // Save a new secret using Slot 0
        let mut secrets = loaded_slot0;
        secrets.insert("test_key".to_string(), TaintedString::credential("test_val".to_string()));
        vault.save(passphrase, &secrets).unwrap();

        // Load secrets map using Slot 1 (Admin Key File) via explicit path
        let loaded_slot1 = vault.load(&format!("admin_key_file:{}", admin_key_path.to_str().unwrap())).unwrap();
        assert_eq!(loaded_slot1.get("test_key").unwrap().expose_secret(), "test_val");

        // Clean up
        let _ = std::fs::remove_file(&vault_path);
        let _ = std::fs::remove_file(&admin_key_path);
    }
}

