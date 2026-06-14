// crates/hydragent-security/src/taint.rs
//
// Phase 6 / Track 6.2: Taint sink policy.
//
// A [`SinkPolicy`] maps each known *sink* (e.g. LlmRequestMessages, Log,
// OutboundNetwork, …) to a [`SinkRule`] that declares which taint
// categories are *forbidden* (must never reach the sink) and which must
// be *redacted* before delivery. Policies are loaded from YAML (see
// `config/security/taint_sinks.yaml.example`) or constructed in code
// via [`SinkPolicy::default_policy`].
//
// `enforce()` is the runtime check: given a (sink, taint) pair it
// returns a [`TaintViolation`] if any forbidden category is present.
// `requires_redact()` tells callers whether the value should be run
// through a redactor first. Both functions treat an empty taint set
// as always-allowed.

use std::collections::BTreeMap;

use hydragent_types::{TaintCategory, TaintSet};
use serde::{Deserialize, Serialize};

/// Where taint-tracked data is being sent. Phase 6 / Track 6.2 defines
/// 5 sinks. Add a new variant (and a rule) when adding a new outbound
/// destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaintSink {
    /// Outbound messages sent to the LLM (chat completions, tool calls).
    LlmRequestMessages,
    /// Any network egress (HTTP, websocket, webhook, …).
    OutboundNetwork,
    /// Log lines emitted to the console, file, or observability backend.
    Log,
    /// Writes into the encrypted vault.
    VaultWrite,
    /// Writes to the local filesystem.
    DiskWrite,
}

impl TaintSink {
    /// All 5 sinks in canonical order.
    pub const ALL: [TaintSink; 5] = [
        TaintSink::LlmRequestMessages,
        TaintSink::OutboundNetwork,
        TaintSink::Log,
        TaintSink::VaultWrite,
        TaintSink::DiskWrite,
    ];

    /// Snake-case form (matches `#[serde(rename_all = "snake_case")]`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LlmRequestMessages => "llm_request_messages",
            Self::OutboundNetwork     => "outbound_network",
            Self::Log                => "log",
            Self::VaultWrite         => "vault_write",
            Self::DiskWrite          => "disk_write",
        }
    }
}

impl std::fmt::Display for TaintSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-sink policy: which taint categories are forbidden and which
/// must be redacted before delivery. A category should not appear in
/// both lists — if it does, `enforce` will block the value (forbid
/// wins).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SinkRule {
    /// Taint categories that are blocked from reaching this sink.
    #[serde(default)]
    pub forbid: Vec<TaintCategory>,
    /// Taint categories that are allowed but must be redacted first.
    #[serde(default)]
    pub redact: Vec<TaintCategory>,
}

impl SinkRule {
    pub fn new(forbid: Vec<TaintCategory>, redact: Vec<TaintCategory>) -> Self {
        Self { forbid, redact }
    }
}

/// Versioned map of `TaintSink → SinkRule`. Load from YAML or build
/// in code via [`SinkPolicy::default_policy`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SinkPolicy {
    pub version: u32,
    pub sinks: BTreeMap<TaintSink, SinkRule>,
}

impl SinkPolicy {
    /// Highest policy version this build supports. Bump when changing
    /// the schema in a backward-incompatible way.
    pub const SUPPORTED_VERSION: u32 = 1;

    /// Hardcoded fallback policy. Used when no `taint_sinks.yaml` is
    /// present (or fails to load). Mirrors the YAML shipped in
    /// `config/security/taint_sinks.yaml.example`.
    pub fn default_policy() -> Self {
        let mut sinks = BTreeMap::new();
        sinks.insert(
            TaintSink::LlmRequestMessages,
            SinkRule::new(
                vec![TaintCategory::Secret, TaintCategory::PII],
                vec![TaintCategory::PII],
            ),
        );
        sinks.insert(
            TaintSink::OutboundNetwork,
            SinkRule::new(
                vec![
                    TaintCategory::Secret,
                    TaintCategory::PII,
                    TaintCategory::SystemInternal,
                    TaintCategory::LlmOutput,
                ],
                vec![],
            ),
        );
        sinks.insert(
            TaintSink::Log,
            SinkRule::new(
                vec![TaintCategory::Secret],
                vec![TaintCategory::PII],
            ),
        );
        sinks.insert(
            TaintSink::DiskWrite,
            SinkRule::new(
                vec![TaintCategory::Secret, TaintCategory::SystemInternal],
                vec![],
            ),
        );
        sinks.insert(
            TaintSink::VaultWrite,
            SinkRule::new(
                vec![TaintCategory::Secret],
                vec![],
            ),
        );
        Self { version: 1, sinks }
    }

    /// Parse a YAML policy document. Returns an error on bad YAML or
    /// on a `version` higher than [`Self::SUPPORTED_VERSION`].
    pub fn load_yaml(yaml: &str) -> Result<Self, TaintError> {
        let policy: SinkPolicy = serde_yaml::from_str(yaml)?;
        if policy.version > Self::SUPPORTED_VERSION {
            return Err(TaintError::UnsupportedVersion {
                found: policy.version,
                max: Self::SUPPORTED_VERSION,
            });
        }
        Ok(policy)
    }

    /// Look up the rule for a sink (`None` if not configured).
    pub fn rule(&self, sink: TaintSink) -> Option<&SinkRule> {
        self.sinks.get(&sink)
    }

    /// Enforce policy: returns `Err(TaintViolation)` if the taint set
    /// contains any category in the sink's `forbid` list. Empty taint
    /// is always allowed. Sinks with no configured rule are allowed.
    pub fn enforce(&self, sink: TaintSink, taint: &TaintSet) -> Result<(), TaintViolation> {
        if taint.is_empty() {
            return Ok(());
        }
        let rule = match self.sinks.get(&sink) {
            Some(r) => r,
            None => return Ok(()),
        };
        let forbidden: Vec<TaintCategory> = rule
            .forbid
            .iter()
            .copied()
            .filter(|c| taint.contains(*c))
            .collect();
        if forbidden.is_empty() {
            Ok(())
        } else {
            Err(TaintViolation { sink, forbidden })
        }
    }

    /// Returns true if the taint set contains any category in the
    /// sink's `redact` list. Callers should run the value through a
    /// redactor before delivery.
    pub fn requires_redact(&self, sink: TaintSink, taint: &TaintSet) -> bool {
        if taint.is_empty() {
            return false;
        }
        match self.sinks.get(&sink) {
            Some(r) => r.redact.iter().any(|c| taint.contains(*c)),
            None => false,
        }
    }
}

impl Default for SinkPolicy {
    fn default() -> Self { Self::default_policy() }
}

/// Returned by [`SinkPolicy::enforce`] when a forbidden taint
/// reaches a sink. Carries the sink and the list of forbidden
/// categories that were actually present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaintViolation {
    pub sink: TaintSink,
    pub forbidden: Vec<TaintCategory>,
}

impl std::fmt::Display for TaintViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cats: Vec<String> = self.forbidden.iter().map(|c| c.to_string()).collect();
        write!(
            f,
            "taint violation at sink `{}`: forbidden categories [{}]",
            self.sink,
            cats.join(",")
        )
    }
}

impl std::error::Error for TaintViolation {}

/// Errors that can occur while loading a policy from YAML.
#[derive(Debug, thiserror::Error)]
pub enum TaintError {
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("policy version {found} exceeds supported maximum {max}")]
    UnsupportedVersion { found: u32, max: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(c: TaintCategory) -> TaintSet { TaintSet::singleton(c) }

    #[test]
    fn taint_sink_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&TaintSink::LlmRequestMessages).unwrap(),
            "\"llm_request_messages\""
        );
        assert_eq!(
            serde_json::to_string(&TaintSink::OutboundNetwork).unwrap(),
            "\"outbound_network\""
        );
        assert_eq!(serde_json::to_string(&TaintSink::Log).unwrap(), "\"log\"");
        assert_eq!(
            serde_json::to_string(&TaintSink::VaultWrite).unwrap(),
            "\"vault_write\""
        );
        assert_eq!(
            serde_json::to_string(&TaintSink::DiskWrite).unwrap(),
            "\"disk_write\""
        );
    }

    #[test]
    fn taint_sink_serde_roundtrip() {
        for s in TaintSink::ALL {
            let json = serde_json::to_string(&s).unwrap();
            let back: TaintSink = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn default_policy_has_all_five_sinks() {
        let p = SinkPolicy::default_policy();
        assert_eq!(p.sinks.len(), 5);
        for s in TaintSink::ALL {
            assert!(p.sinks.contains_key(&s), "default policy missing rule for {s}");
        }
    }

    #[test]
    fn default_policy_secret_blocked_everywhere() {
        let p = SinkPolicy::default_policy();
        let secret = t(TaintCategory::Secret);
        for s in TaintSink::ALL {
            assert!(p.enforce(s, &secret).is_err(), "Secret should be blocked at {s}");
        }
    }

    #[test]
    fn default_policy_pii_blocked_at_outbound_and_llm() {
        let p = SinkPolicy::default_policy();
        let pii = t(TaintCategory::PII);
        assert!(p.enforce(TaintSink::OutboundNetwork, &pii).is_err());
        assert!(p.enforce(TaintSink::LlmRequestMessages, &pii).is_err());
        // PII is not in the forbid list for Log/DiskWrite/VaultWrite
        assert!(p.enforce(TaintSink::Log, &pii).is_ok());
        assert!(p.enforce(TaintSink::DiskWrite, &pii).is_ok());
        assert!(p.enforce(TaintSink::VaultWrite, &pii).is_ok());
    }

    #[test]
    fn default_policy_pii_requires_redact_at_log() {
        let p = SinkPolicy::default_policy();
        let pii = t(TaintCategory::PII);
        assert!(p.requires_redact(TaintSink::Log, &pii));
        assert!(p.requires_redact(TaintSink::LlmRequestMessages, &pii));
        assert!(!p.requires_redact(TaintSink::OutboundNetwork, &pii));
        assert!(!p.requires_redact(TaintSink::DiskWrite, &pii));
        assert!(!p.requires_redact(TaintSink::VaultWrite, &pii));
    }

    #[test]
    fn default_policy_system_internal_blocked_at_outbound_and_disk() {
        let p = SinkPolicy::default_policy();
        let si = t(TaintCategory::SystemInternal);
        assert!(p.enforce(TaintSink::OutboundNetwork, &si).is_err());
        assert!(p.enforce(TaintSink::DiskWrite, &si).is_err());
        assert!(p.enforce(TaintSink::Log, &si).is_ok());
        assert!(p.enforce(TaintSink::LlmRequestMessages, &si).is_ok());
        assert!(p.enforce(TaintSink::VaultWrite, &si).is_ok());
    }

    #[test]
    fn default_policy_llm_output_blocked_at_outbound() {
        let p = SinkPolicy::default_policy();
        let lo = t(TaintCategory::LlmOutput);
        assert!(p.enforce(TaintSink::OutboundNetwork, &lo).is_err());
        assert!(p.enforce(TaintSink::Log, &lo).is_ok());
        assert!(p.enforce(TaintSink::LlmRequestMessages, &lo).is_ok());
        assert!(p.enforce(TaintSink::DiskWrite, &lo).is_ok());
        assert!(p.enforce(TaintSink::VaultWrite, &lo).is_ok());
    }

    #[test]
    fn default_policy_user_input_allowed_everywhere() {
        let p = SinkPolicy::default_policy();
        let ui = t(TaintCategory::UserInput);
        for s in TaintSink::ALL {
            assert!(p.enforce(s, &ui).is_ok(), "UserInput should be allowed at {s}");
        }
    }

    #[test]
    fn default_policy_tool_output_allowed_everywhere() {
        let p = SinkPolicy::default_policy();
        let to = t(TaintCategory::ToolOutput);
        for s in TaintSink::ALL {
            assert!(p.enforce(s, &to).is_ok(), "ToolOutput should be allowed at {s}");
        }
    }

    #[test]
    fn empty_taint_always_allowed() {
        let p = SinkPolicy::default_policy();
        let empty = TaintSet::new();
        for s in TaintSink::ALL {
            assert!(p.enforce(s, &empty).is_ok());
            assert!(!p.requires_redact(s, &empty));
        }
    }

    #[test]
    fn union_violation_lists_all_forbidden_present() {
        let p = SinkPolicy::default_policy();
        let mut both = TaintSet::new();
        both.insert(TaintCategory::Secret);
        both.insert(TaintCategory::SystemInternal);
        let v = p.enforce(TaintSink::OutboundNetwork, &both).unwrap_err();
        assert_eq!(v.sink, TaintSink::OutboundNetwork);
        assert!(v.forbidden.contains(&TaintCategory::Secret));
        assert!(v.forbidden.contains(&TaintCategory::SystemInternal));
        assert_eq!(v.forbidden.len(), 2);
    }

    #[test]
    fn non_present_forbidden_not_in_violation() {
        // If the taint set has only UserInput, but the sink forbids
        // Secret+PII, the violation list should be empty.
        let p = SinkPolicy::default_policy();
        let ui = t(TaintCategory::UserInput);
        let v = p.enforce(TaintSink::OutboundNetwork, &ui).unwrap();
        // If we got Ok(()) we don't have a violation, but if we did,
        // the forbidden list would be empty. Both are valid.
        let _ = v;
    }

    #[test]
    fn sink_with_no_rule_allows_everything() {
        let p = SinkPolicy {
            version: 1,
            sinks: BTreeMap::new(),
        };
        let secret = t(TaintCategory::Secret);
        for s in TaintSink::ALL {
            assert!(p.enforce(s, &secret).is_ok());
            assert!(!p.requires_redact(s, &secret));
        }
    }

    #[test]
    fn load_yaml_minimal() {
        let yaml = "version: 1\nsinks: {}\n";
        let p = SinkPolicy::load_yaml(yaml).unwrap();
        assert_eq!(p.version, 1);
        assert_eq!(p.sinks.len(), 0);
    }

    #[test]
    fn load_yaml_full() {
        let yaml = r#"
version: 1
sinks:
  llm_request_messages:
    forbid: [secret, pii]
    redact: [pii]
  outbound_network:
    forbid: [secret, pii, system_internal, llm_output]
    redact: []
  log:
    forbid: [secret]
    redact: [pii]
  disk_write:
    forbid: [secret, system_internal]
    redact: []
  vault_write:
    forbid: [secret]
    redact: []
"#;
        let p = SinkPolicy::load_yaml(yaml).unwrap();
        assert_eq!(p.sinks.len(), 5);
        let llm = p.rule(TaintSink::LlmRequestMessages).unwrap();
        assert!(llm.forbid.contains(&TaintCategory::Secret));
        assert!(llm.forbid.contains(&TaintCategory::PII));
        assert!(llm.redact.contains(&TaintCategory::PII));
    }

    #[test]
    fn load_yaml_unsupported_version_errors() {
        let yaml = "version: 99\nsinks: {}\n";
        let err = SinkPolicy::load_yaml(yaml).unwrap_err();
        match err {
            TaintError::UnsupportedVersion { found, max } => {
                assert_eq!(found, 99);
                assert_eq!(max, SinkPolicy::SUPPORTED_VERSION);
            }
            _ => panic!("expected UnsupportedVersion error, got {err:?}"),
        }
    }

    #[test]
    fn load_yaml_malformed_returns_yaml_error() {
        let yaml = "::: not valid yaml ::: [[[";
        let err = SinkPolicy::load_yaml(yaml);
        assert!(matches!(err, Err(TaintError::Yaml(_))));
    }

    #[test]
    fn load_yaml_partial_with_defaults() {
        // Only one sink configured, others are absent
        let yaml = r#"
version: 1
sinks:
  log:
    forbid: [secret]
    redact: []
"#;
        let p = SinkPolicy::load_yaml(yaml).unwrap();
        assert_eq!(p.sinks.len(), 1);
        let log = p.rule(TaintSink::Log).unwrap();
        assert_eq!(log.forbid, vec![TaintCategory::Secret]);
        assert!(log.redact.is_empty());
    }

    #[test]
    fn default_policy_matches_yaml_example() {
        // The YAML shipped in config/security/taint_sinks.yaml.example
        // must round-trip to the same policy as `default_policy()`.
        let yaml = r#"
version: 1
sinks:
  llm_request_messages:
    forbid: [secret, pii]
    redact: [pii]
  outbound_network:
    forbid: [secret, pii, system_internal, llm_output]
    redact: []
  log:
    forbid: [secret]
    redact: [pii]
  disk_write:
    forbid: [secret, system_internal]
    redact: []
  vault_write:
    forbid: [secret]
    redact: []
"#;
        let from_yaml = SinkPolicy::load_yaml(yaml).unwrap();
        let from_code = SinkPolicy::default_policy();
        assert_eq!(from_yaml, from_code);
    }

    #[test]
    fn taint_violation_displays_readably() {
        let v = TaintViolation {
            sink: TaintSink::OutboundNetwork,
            forbidden: vec![TaintCategory::Secret, TaintCategory::PII],
        };
        let s = v.to_string();
        assert!(s.contains("outbound_network"));
        assert!(s.contains("secret"));
        assert!(s.contains("pii"));
    }

    #[test]
    fn rule_new_constructs() {
        let r = SinkRule::new(vec![TaintCategory::Secret], vec![TaintCategory::PII]);
        assert_eq!(r.forbid, vec![TaintCategory::Secret]);
        assert_eq!(r.redact, vec![TaintCategory::PII]);
    }

    #[test]
    fn rule_default_is_empty() {
        let r = SinkRule::default();
        assert!(r.forbid.is_empty());
        assert!(r.redact.is_empty());
    }
}
