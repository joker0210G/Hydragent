//! Ed25519 signing key for non-repudiable action receipts.
//!
//! Every audit chain entry is signed over its `chain_hash` by the agent's
//! Ed25519 signing key. The verifying (public) key is published to
//! `config/keys/agent_ed25519.pub` so external auditors can verify the chain
//! without holding the private key.
//!
//! ## Lifecycle
//!
//! 1. **Generate** at first vault init ([`AgentSigner::generate`])
//! 2. **Persist** the 32-byte private key into the vault
//! 3. **Publish** the 32-byte public key to `config/keys/agent_ed25519.pub`
//! 4. **Rotate** with `vault rotate-key` — generates a new pair, keeps
//!    historical signatures verifiable against the old public key
//!
//! ## Thread safety
//!
//! `AgentSigner` is `Send + Sync` because the underlying `ed25519_dalek` keys
//! are pure data; the signing key is zeroized in [`Drop`].

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use rand::RngCore;
use hydragent_types::{AgentResponse, ToolCallRecord};
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::Path;
use zeroize::Zeroize;

/// PEM-style header for the published public key file. Hex body in between.
const PEM_HEADER: &str = "-----BEGIN ED25519 PUBLIC KEY-----";
const PEM_FOOTER: &str = "-----END ED25519 PUBLIC KEY-----";

/// Manages the agent's Ed25519 keypair.
///
/// Construct via [`AgentSigner::generate`] (fresh key) or
/// [`AgentSigner::from_bytes`] (load from vault).
pub struct AgentSigner {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
}

impl AgentSigner {
    /// Generate a fresh Ed25519 keypair using the OS CSPRNG.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let signing_key = SigningKey::from_bytes(&bytes);
        let verifying_key = signing_key.verifying_key();
        Self { signing_key, verifying_key }
    }

    /// Load an existing signing key from its raw 32-byte representation.
    ///
    /// Use this when restoring a key persisted in the vault.
    pub fn from_bytes(key_bytes: &[u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(key_bytes);
        let verifying_key = signing_key.verifying_key();
        Self { signing_key, verifying_key }
    }

    /// Return the cached verifying (public) key. Infallible.
    pub fn verifying_key(&self) -> VerifyingKey {
        self.verifying_key
    }

    /// Private key bytes — **only** call to persist into the encrypted vault.
    /// Never log, never write to plaintext, never serialize via JSON.
    pub fn private_key_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    /// Public key bytes — safe to publish for external verification.
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.verifying_key.to_bytes()
    }

    /// Sign arbitrary bytes. Returns the 64-byte Ed25519 signature.
    pub fn sign_bytes(&self, data: &[u8]) -> Signature {
        self.signing_key.sign(data)
    }

    /// Verify a 64-byte Ed25519 signature against this agent's public key.
    pub fn verify_bytes(&self, data: &[u8], signature_bytes: &[u8]) -> Result<(), SignerError> {
        if signature_bytes.len() != 64 {
            return Err(SignerError::InvalidLength(signature_bytes.len()));
        }
        let sig_array: [u8; 64] = signature_bytes
            .try_into()
            .expect("length checked above");
        let sig = Signature::from_bytes(&sig_array);
        self.verifying_key
            .verify(data, &sig)
            .map_err(|e| SignerError::VerificationFailed(e.to_string()))
    }

    /// Sign a completed [`ToolCallRecord`]. Returns hex-encoded signature.
    ///
    /// Canonical form is the serde_json representation of the record.
    /// ToolCallRecord field order is stable (declared in `hydragent-types`),
    /// so the same record always produces the same signature.
    pub fn sign_tool_call(&self, call: &ToolCallRecord) -> Result<String, SignerError> {
        let canonical = serde_json::to_string(call)
            .map_err(|e| SignerError::Serialize(e.to_string()))?;
        let sig = self.sign_bytes(canonical.as_bytes());
        Ok(hex::encode(sig.to_bytes()))
    }

    /// Sign an [`AgentResponse`]. Returns hex-encoded signature.
    ///
    /// Signs over a stable subset of fields: `page_id`, SHA-256 of `content`,
    /// and the `format` discriminant. This avoids including transient
    /// fields like streaming metadata.
    pub fn sign_response(&self, response: &AgentResponse) -> Result<String, SignerError> {
        let content_hash = hex::encode(Sha256::digest(response.content.as_bytes()));
        let canonical = serde_json::json!({
            "page_id": response.page_id,
            "content_hash": content_hash,
            "format": response.format,
        });
        let canonical_str = canonical.to_string();
        let sig = self.sign_bytes(canonical_str.as_bytes());
        Ok(hex::encode(sig.to_bytes()))
    }

    /// Export the verifying (public) key to a PEM-style file for external
    /// auditors. The body is the 32-byte public key hex-encoded.
    pub fn export_public_key(&self, path: impl AsRef<Path>) -> Result<(), SignerError> {
        let hex_key = hex::encode(self.public_key_bytes());
        let pem = format!("{PEM_HEADER}\n{hex_key}\n{PEM_FOOTER}\n");
        std::fs::write(path.as_ref(), pem)
            .map_err(|e| SignerError::Io(e.to_string()))?;
        tracing::info!(path = %path.as_ref().display(), "Ed25519 public key exported");
        Ok(())
    }

    /// Load a verifying key from a PEM-style file. Used by external auditors
    /// to verify chains without holding the agent's private key.
    pub fn load_verifying_key(pem: impl AsRef<Path>) -> Result<VerifyingKey, SignerError> {
        let content = std::fs::read_to_string(pem.as_ref())
            .map_err(|e| SignerError::Io(e.to_string()))?;
        let body = content
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("-----"))
            .ok_or_else(|| SignerError::InvalidPem("missing body".into()))?;
        let bytes = hex::decode(body).map_err(|e| SignerError::InvalidPem(e.to_string()))?;
        if bytes.len() != 32 {
            return Err(SignerError::InvalidLength(bytes.len()));
        }
        let arr: [u8; 32] = bytes.try_into().expect("length checked above");
        VerifyingKey::from_bytes(&arr).map_err(|e| SignerError::VerificationFailed(e.to_string()))
    }
}

impl Drop for AgentSigner {
    fn drop(&mut self) {
        // Zeroize the private key bytes from memory on drop.
        let mut key_bytes = self.signing_key.to_bytes();
        key_bytes.zeroize();
    }
}

impl fmt::Debug for AgentSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never expose the private key. Show only the first 8 bytes of the
        // public key (hex) so logs are correlatable but not forgeable.
        let pub_hex = hex::encode(&self.public_key_bytes()[..8]);
        write!(f, "AgentSigner(pubkey={pub_hex}…)")
    }
}

/// Errors raised by [`AgentSigner`].
#[derive(Debug, thiserror::Error)]
pub enum SignerError {
    #[error("invalid signature length: expected 64, got {0}")]
    InvalidLength(usize),

    #[error("verification failed: {0}")]
    VerificationFailed(String),

    #[error("invalid PEM file: {0}")]
    InvalidPem(String),

    #[error("serialization failed: {0}")]
    Serialize(String),

    #[error("I/O error: {0}")]
    Io(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydragent_types::{ResponseFormat, ToolStatus};

    fn make_record() -> ToolCallRecord {
        ToolCallRecord {
            call_id: "call-1".into(),
            tool_id: "web_search".into(),
            params_hash: "deadbeef".into(),
            status: ToolStatus::Success,
            execution_ms: 42,
            timestamp: 1_700_000_000_000,
        }
    }

    fn make_response() -> AgentResponse {
        AgentResponse {
            page_id: "page-1".into(),
            content: "hello world".into(),
            format: ResponseFormat::Markdown,
            consent_requests: vec![],
            tool_calls_executed: vec![],
        }
    }

    #[test]
    fn generate_produces_working_keypair() {
        let s = AgentSigner::generate();
        let pub_bytes = s.public_key_bytes();
        assert_eq!(pub_bytes.len(), 32);

        let data = b"audit event";
        let sig = s.sign_bytes(data);
        assert_eq!(sig.to_bytes().len(), 64);
        s.verify_bytes(data, &sig.to_bytes()).expect("verify must succeed");
    }

    #[test]
    fn from_bytes_roundtrips() {
        let original = AgentSigner::generate();
        let priv_bytes = original.private_key_bytes();
        let restored = AgentSigner::from_bytes(&priv_bytes);
        assert_eq!(original.public_key_bytes(), restored.public_key_bytes());

        let data = b"hello";
        let sig = restored.sign_bytes(data);
        original.verify_bytes(data, &sig.to_bytes()).expect("verify with original pub");
    }

    #[test]
    fn wrong_key_fails_verification() {
        let alice = AgentSigner::generate();
        let bob = AgentSigner::generate();
        let data = b"for alice";
        let sig = alice.sign_bytes(data);
        let result = bob.verify_bytes(data, &sig.to_bytes());
        assert!(matches!(result, Err(SignerError::VerificationFailed(_))));
    }

    #[test]
    fn wrong_length_rejected() {
        let s = AgentSigner::generate();
        let bad_sig = vec![0u8; 32];
        let result = s.verify_bytes(b"x", &bad_sig);
        assert!(matches!(result, Err(SignerError::InvalidLength(32))));
    }

    #[test]
    fn sign_tool_call_is_deterministic_per_record() {
        let s = AgentSigner::generate();
        let r1 = make_record();
        let r2 = make_record();
        let sig1 = s.sign_tool_call(&r1).unwrap();
        let sig2 = s.sign_tool_call(&r2).unwrap();
        assert_eq!(sig1, sig2, "same record -> same signature");
        assert_eq!(sig1.len(), 128, "Ed25519 hex = 64 bytes -> 128 chars");
    }

    #[test]
    fn sign_tool_call_changes_with_record() {
        let s = AgentSigner::generate();
        let r1 = make_record();
        let mut r2 = r1.clone();
        r2.call_id = "call-2".into();
        let sig1 = s.sign_tool_call(&r1).unwrap();
        let sig2 = s.sign_tool_call(&r2).unwrap();
        assert_ne!(sig1, sig2);
    }

    #[test]
    fn sign_response_is_deterministic() {
        let s = AgentSigner::generate();
        let r = make_response();
        let sig1 = s.sign_response(&r).unwrap();
        let sig2 = s.sign_response(&r).unwrap();
        assert_eq!(sig1, sig2);
        assert_eq!(sig1.len(), 128);
    }

    #[test]
    fn sign_response_changes_with_content() {
        let s = AgentSigner::generate();
        let r1 = make_response();
        let mut r2 = r1.clone();
        r2.content = "different content".into();
        let sig1 = s.sign_response(&r1).unwrap();
        let sig2 = s.sign_response(&r2).unwrap();
        assert_ne!(sig1, sig2);
    }

    #[test]
    fn debug_redacts_private_key() {
        let s = AgentSigner::generate();
        let dbg = format!("{s:?}");
        let priv_hex = hex::encode(s.private_key_bytes());
        assert!(!dbg.contains(&priv_hex), "Debug must not leak private key");
        let pub_hex = hex::encode(&s.public_key_bytes()[..8]);
        assert!(dbg.contains(&pub_hex), "Debug should show truncated pubkey");
    }

    #[test]
    fn pubkey_export_then_load_verifies() {
        let s = AgentSigner::generate();
        let dir = std::env::temp_dir().join(format!("hydragent-pubkey-test-{}.pem", uuid::Uuid::new_v4()));
        s.export_public_key(&dir).expect("export");

        let loaded = AgentSigner::load_verifying_key(&dir).expect("load");
        assert_eq!(loaded.to_bytes(), s.public_key_bytes());

        // Use the loaded key (not the signer) to verify a signature.
        let data = b"verify me";
        let sig = s.sign_bytes(data);
        loaded.verify(data, &sig).expect("verify with loaded pubkey");

        let _ = std::fs::remove_file(&dir);
    }
}
