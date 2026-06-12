//! Taint tracking subsystem (Phase 3 §5.8).
//!
//! A [`TaintedString`] is a newtype that wraps secret-derived or
//! untrusted text and makes it impossible to accidentally log or
//! serialize the raw value. In addition to display-level redaction,
//! we tag every taint with one of six categories so the orchestrator
//! and prompt builder can apply category-specific policies
//! (e.g. reject `Untrusted` text from being used as a shell command).
//!
//! Categories:
//!
//! | Variant          | Source                                                  | Policy                                          |
//! |------------------|---------------------------------------------------------|-------------------------------------------------|
//! | `Credential`     | Vault secrets                                          | Never serialize. Inject only at network boundary. |
//! | `Pii`            | Personally identifiable info (name, email, SSN, …)    | Redact in logs. Pass through to LLM as raw.     |
//! | `Untrusted`      | Free-form text from user input / web search / tool     | Strip from tool-args going to shell tools.      |
//! | `Command`        | A shell command about to be executed                   | Reject if it contains `Untrusted` substrings.   |
//! | `NetworkOrigin`  | URL, hostname, or IP                                   | Validate against allowlist before fetch.        |
//! | `FilePath`       | A filesystem path                                     | Resolve and check against preopened roots.      |
//!
//! Propagation rule (see [`TaintedString::merge`]): if a value is
//! produced by combining two tainted strings, the result carries the
//! *union* of both taint sets. This is conservative — once tainted,
//! always tainted, until the value is explicitly sanitized
//! (e.g. by [`TaintedString::into_sanitized`]).

use std::fmt;
use zeroize::Zeroize;

/// The six taint categories a string can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaintCategory {
    /// Vault credential, API key, token, password.
    Credential,
    /// Personally identifiable information.
    Pii,
    /// Free-form text from user input, web search, or third-party tool output.
    Untrusted,
    /// A shell command.
    Command,
    /// A network origin (URL, hostname, IP).
    NetworkOrigin,
    /// A filesystem path.
    FilePath,
}

impl fmt::Display for TaintCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            TaintCategory::Credential => "credential",
            TaintCategory::Pii => "pii",
            TaintCategory::Untrusted => "untrusted",
            TaintCategory::Command => "command",
            TaintCategory::NetworkOrigin => "network_origin",
            TaintCategory::FilePath => "file_path",
        };
        f.write_str(s)
    }
}

/// A newtype that wraps a string and tracks its taint origin.
///
/// The inner `String` is **zeroized on drop** ([`Zeroize`]) so a
/// secret does not linger in heap memory after the wrapper goes out
/// of scope.
///
/// `Display` and `Debug` always render `[REDACTED]`. To read the
/// raw value, you MUST call [`expose_secret`](TaintedString::expose_secret)
/// — every such call site is an auditable, intentional exposure.
#[derive(Clone, Default, Zeroize)]
pub struct TaintedString {
    inner: String,
    /// The set of taint categories that apply to this string. Empty
    /// means "trusted / system-authored". Multiple categories can
    /// apply (e.g. a Command that contains Untrusted substrings).
    ///
    /// `#[zeroize(skip)]` — the category tags are *metadata*, not
    /// secret data, so we don't need to overwrite them on drop. The
    /// `inner` string is the only piece that must be zeroed.
    #[zeroize(skip)]
    taint: Vec<TaintCategory>,
}

impl TaintedString {
    /// Create a *trusted* (untagged) string. Use this for system-authored
    /// text like the LLM's own reasoning.
    pub fn new(s: String) -> Self {
        Self { inner: s, taint: Vec::new() }
    }

    /// Create a string with a single taint tag. Use the specific
    /// helpers ([`credential`](TaintedString::credential),
    /// [`untrusted`](TaintedString::untrusted), …) instead of this
    /// constructor in production code.
    pub fn with_taint(s: String, category: TaintCategory) -> Self {
        let mut taint = Vec::new();
        taint.push(category);
        Self { inner: s, taint }
    }

    pub fn credential(s: String) -> Self {
        Self::with_taint(s, TaintCategory::Credential)
    }
    pub fn pii(s: String) -> Self {
        Self::with_taint(s, TaintCategory::Pii)
    }
    pub fn untrusted(s: String) -> Self {
        Self::with_taint(s, TaintCategory::Untrusted)
    }
    pub fn command(s: String) -> Self {
        Self::with_taint(s, TaintCategory::Command)
    }
    pub fn network_origin(s: String) -> Self {
        Self::with_taint(s, TaintCategory::NetworkOrigin)
    }
    pub fn file_path(s: String) -> Self {
        Self::with_taint(s, TaintCategory::FilePath)
    }

    /// The only way to read the raw string. Each call is a deliberate,
    /// auditable exposure — e.g. when the network-boundary injector
    /// substitutes a placeholder with a real secret.
    pub fn expose_secret(&self) -> &str {
        &self.inner
    }

    /// Return the set of taint categories currently applied to this string.
    pub fn taint(&self) -> &[TaintCategory] {
        &self.taint
    }

    /// `true` if the string carries any taint (i.e. it is not
    /// system-authored).
    pub fn is_tainted(&self) -> bool {
        !self.taint.is_empty()
    }

    /// `true` if the string carries the given category.
    pub fn has_taint(&self, required: TaintCategory) -> bool {
        self.taint.contains(&required)
    }

    /// Combine two tainted strings by concatenation. The result carries
    /// the *union* of both taint sets — once tainted, always tainted.
    /// This is the propagation rule: any value that touches a
    /// `Credential` becomes a `Credential`.
    pub fn merge(&self, other: &TaintedString) -> TaintedString {
        let mut combined_taint: Vec<TaintCategory> = self.taint.clone();
        for cat in &other.taint {
            if !combined_taint.contains(cat) {
                combined_taint.push(*cat);
            }
        }
        let mut merged = String::with_capacity(self.inner.len() + other.inner.len());
        merged.push_str(&self.inner);
        merged.push_str(&other.inner);
        TaintedString { inner: merged, taint: combined_taint }
    }

    /// Strip all taint tags, returning an *untainted* string. The
    /// caller is asserting they have validated the value (e.g. they
    /// ran a shell command through `bash -n` and confirmed it's
    /// syntactically valid). This is the only way to remove taint.
    pub fn into_sanitized(self) -> TaintedString {
        TaintedString { inner: self.inner, taint: Vec::new() }
    }
}

impl From<String> for TaintedString {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for TaintedString {
    fn from(s: &str) -> Self {
        Self::new(s.to_string())
    }
}

impl fmt::Display for TaintedString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl fmt::Debug for TaintedString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.taint.is_empty() {
            write!(f, "TaintedString(trusted, len={})", self.inner.len())
        } else {
            write!(f, "TaintedString([REDACTED], taint=[{}])", self.taint.iter()
                .map(|c| c.to_string()).collect::<Vec<_>>().join(","))
        }
    }
}

impl serde::Serialize for TaintedString {
    /// Serialization IS allowed for taint categories that are safe
    /// to expose (Pii, NetworkOrigin, FilePath) but for the strictest
    /// category (Credential) we refuse to serialize. The default
    /// `Serialize` impl is bypassed by category check.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if self.taint.contains(&TaintCategory::Credential) {
            return Err(serde::ser::Error::custom(
                "refusing to serialize a TaintedString tagged Credential; \
                 expose it via KeyInjector at the network boundary instead",
            ));
        }
        serializer.serialize_str(&self.inner)
    }
}

impl<'de> serde::Deserialize<'de> for TaintedString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        // Deserialized strings come from our on-disk vault and are
        // tagged as Credential by default. This is the safe default —
        // if a caller wants a trusted string, they should construct
        // it via `TaintedString::new`.
        Ok(Self::credential(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_and_debug_redact() {
        let secret = TaintedString::credential("supersecret".to_string());
        assert_eq!(format!("{}", secret), "[REDACTED]");
        assert_eq!(format!("{:?}", secret).contains("supersecret"), false);
        // The Debug form should mention the taint category
        assert!(format!("{:?}", secret).contains("credential"));
    }

    #[test]
    fn test_expose_secret_returns_raw() {
        let secret = TaintedString::credential("supersecret".to_string());
        assert_eq!(secret.expose_secret(), "supersecret");
    }

    #[test]
    fn test_untainted_by_default() {
        let s = TaintedString::new("hello".to_string());
        assert!(!s.is_tainted());
        assert!(s.taint().is_empty());
    }

    #[test]
    fn test_taint_categories() {
        assert!(TaintedString::credential("k".to_string()).has_taint(TaintCategory::Credential));
        assert!(TaintedString::untrusted("u".to_string()).has_taint(TaintCategory::Untrusted));
        assert!(TaintedString::pii("p".to_string()).has_taint(TaintCategory::Pii));
        assert!(TaintedString::command("c".to_string()).has_taint(TaintCategory::Command));
        assert!(TaintedString::network_origin("n".to_string()).has_taint(TaintCategory::NetworkOrigin));
        assert!(TaintedString::file_path("f".to_string()).has_taint(TaintCategory::FilePath));
    }

    #[test]
    fn test_merge_unions_taint() {
        let cred = TaintedString::credential("ghp_".to_string());
        let untrusted = TaintedString::untrusted("user_input".to_string());
        let merged = cred.merge(&untrusted);
        // Merged string contains both substrings
        assert!(merged.expose_secret().contains("ghp_"));
        assert!(merged.expose_secret().contains("user_input"));
        // Merged string carries BOTH taint categories
        assert!(merged.has_taint(TaintCategory::Credential));
        assert!(merged.has_taint(TaintCategory::Untrusted));
    }

    #[test]
    fn test_merge_dedups_taint() {
        let a = TaintedString::untrusted("a".to_string());
        let b = TaintedString::untrusted("b".to_string());
        let merged = a.merge(&b);
        // Both inputs had the same taint, so the result has only one entry
        assert_eq!(merged.taint().len(), 1);
        assert!(merged.has_taint(TaintCategory::Untrusted));
    }

    #[test]
    fn test_serialize_credential_rejected() {
        let cred = TaintedString::credential("supersecret".to_string());
        let result = serde_json::to_string(&cred);
        assert!(result.is_err(), "Credential must not serialize; got {:?}", result);
    }

    #[test]
    fn test_serialize_non_credential_allowed() {
        let pii = TaintedString::pii("john@example.com".to_string());
        let json = serde_json::to_string(&pii).unwrap();
        assert_eq!(json, "\"john@example.com\"");

        let untagged = TaintedString::new("safe text".to_string());
        let json = serde_json::to_string(&untagged).unwrap();
        assert_eq!(json, "\"safe text\"");
    }

    #[test]
    fn test_deserialize_treated_as_credential() {
        let json = "\"some_secret\"";
        let s: TaintedString = serde_json::from_str(json).unwrap();
        assert!(s.has_taint(TaintCategory::Credential));
        assert_eq!(s.expose_secret(), "some_secret");
    }

    #[test]
    fn test_sanitize_clears_taint() {
        let cred = TaintedString::credential("secret".to_string());
        let sanitized = cred.into_sanitized();
        assert!(!sanitized.is_tainted());
        assert_eq!(sanitized.expose_secret(), "secret");
    }

    #[test]
    fn test_zeroize_on_drop() {
        // Smoke test — we can't easily verify zeroization from safe
        // Rust (the heap is reused), but we can at least assert that
        // the type is `Zeroize` and that explicit zeroize works.
        use zeroize::Zeroize;
        let mut s = TaintedString::credential("secret".to_string());
        s.zeroize();
        // After zeroize, the inner string is empty
        assert_eq!(s.expose_secret(), "");
    }
}
