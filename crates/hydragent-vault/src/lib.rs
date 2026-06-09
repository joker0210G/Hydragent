pub mod crypto;
pub mod taint;
pub mod vault;
pub mod injector;

pub use taint::TaintedString;
pub use vault::Vault;
pub use injector::{inject_str, inject_value, KeyInjector};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_tainted_string_redacted() {
        let secret = TaintedString::new("supersecretpassword".to_string());
        assert_eq!(format!("{}", secret), "[REDACTED]");
        assert_eq!(format!("{:?}", secret), "[REDACTED]");
        assert_eq!(secret.expose_secret(), "supersecretpassword");
    }

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
    fn test_injector() {
        let mut secrets = HashMap::new();
        secrets.insert("API_KEY".to_string(), TaintedString::new("secret_val".to_string()));

        let template = "Bearer {{API_KEY}} for user {{USER}}";
        let injected = inject_str(template, &secrets);
        assert_eq!(injected, "Bearer secret_val for user {{USER}}");
    }
}
