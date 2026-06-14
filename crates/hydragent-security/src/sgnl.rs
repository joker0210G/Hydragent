//! Phase 6 / Track 6.3 — SGNL-inspired Continuous Authorization Engine (Layer 04)
//!
//! Maps the agent's per-session **risk score** to a per-tool
//! [`AuthDecision`] by combining:
//!
//!   * a [`PolicyConfig`] loaded from YAML
//!     (`config/security/policy.yaml`);
//!   * the session's running tallies of prompt-injection attempts
//!     and taint-sink violations (Layer 03 + Layer 05);
//!   * per-tool overrides declared in the policy file (e.g. always
//!     `deny` for `shell_exec`, or `prompt_above_risk: 0.1` for any
//!     network egress);
//!   * the tool's *base* permission tier from the registry.
//!
//! The risk formula is intentionally simple and transparent:
//!
//! ```text
//!   risk = min(1.0, injection_attempts * 0.2 + taint_violations * 0.3)
//! ```
//!
//! Risk ≥ `policy.deny_threshold`  →  `Deny`
//! Risk ≥ `policy.prompt_threshold` →  escalate one tier (auto→prompt)
//!
//! The engine is `Send + Sync` via [`dashmap::DashMap`] and exposes
//! the public methods used by the orchestrator to *evaluate* a tool
//! call before it runs and to *record* events as they happen.
//!
//! See `doc/phases/PHASE_6.md` §5.5 for the spec.

use dashmap::DashMap;
use hydragent_types::PermissionTier;
use serde::Deserialize;
use std::collections::{HashMap, VecDeque};
use thiserror::Error;

/// Decision produced by [`ContinuousAuthEngine::evaluate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthDecision {
    /// Tool may run without human confirmation.
    AutoApprove,
    /// Tool may run only after human approval.
    Prompt,
    /// Tool must not run.
    Deny,
}

impl AuthDecision {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AutoApprove => "auto_approve",
            Self::Prompt      => "prompt",
            Self::Deny        => "deny",
        }
    }
}

impl std::fmt::Display for AuthDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<PermissionTier> for AuthDecision {
    fn from(t: PermissionTier) -> Self {
        match t {
            PermissionTier::AutoApprove => Self::AutoApprove,
            PermissionTier::Prompt      => Self::Prompt,
            PermissionTier::Deny        => Self::Deny,
        }
    }
}

impl From<AuthDecision> for PermissionTier {
    fn from(d: AuthDecision) -> Self {
        match d {
            AuthDecision::AutoApprove => Self::AutoApprove,
            AuthDecision::Prompt      => Self::Prompt,
            AuthDecision::Deny        => Self::Deny,
        }
    }
}

/// SGNL policy loaded from YAML.
#[derive(Debug, Clone, Deserialize)]
pub struct PolicyConfig {
    /// Schema version. The loader rejects versions higher than
    /// [`PolicyConfig::SUPPORTED_VERSION`].
    pub version: u32,
    /// Risk score above which an `AutoApprove` tier becomes `Prompt`.
    #[serde(default = "default_prompt_threshold")]
    pub prompt_threshold: f64,
    /// Risk score above which any tier becomes `Deny`.
    #[serde(default = "default_deny_threshold")]
    pub deny_threshold: f64,
    /// Per-tool overrides (key = tool id).
    #[serde(default)]
    pub tool_overrides: HashMap<String, ToolPolicy>,
}

fn default_prompt_threshold() -> f64 { 0.3 }
fn default_deny_threshold() -> f64 { 0.7 }

/// Per-tool policy overrides.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ToolPolicy {
    /// Always return this decision (highest priority).
    pub always: Option<AuthDecision>,
    /// Force `Deny` when risk score exceeds this value.
    pub deny_above_risk: Option<f64>,
    /// Force `Prompt` when risk score exceeds this value (only
    /// meaningful when `deny_above_risk` is not exceeded).
    pub prompt_above_risk: Option<f64>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            version: 1,
            prompt_threshold: default_prompt_threshold(),
            deny_threshold: default_deny_threshold(),
            tool_overrides: HashMap::new(),
        }
    }
}

impl PolicyConfig {
    /// Highest version this build of the engine understands.
    pub const SUPPORTED_VERSION: u32 = 1;

    /// Parse a YAML document into a `PolicyConfig`, validating
    /// the `version` field.
    pub fn from_yaml(yaml: &str) -> Result<Self, PolicyError> {
        let p: Self = serde_yaml::from_str(yaml)?;
        if p.version > Self::SUPPORTED_VERSION {
            return Err(PolicyError::UnsupportedVersion {
                found: p.version,
                max: Self::SUPPORTED_VERSION,
            });
        }
        if !(0.0..=1.0).contains(&p.prompt_threshold) {
            return Err(PolicyError::OutOfRange { field: "prompt_threshold", value: p.prompt_threshold });
        }
        if !(0.0..=1.0).contains(&p.deny_threshold) {
            return Err(PolicyError::OutOfRange { field: "deny_threshold", value: p.deny_threshold });
        }
        if p.prompt_threshold > p.deny_threshold {
            return Err(PolicyError::ThresholdsInverted {
                prompt: p.prompt_threshold,
                deny: p.deny_threshold,
            });
        }
        Ok(p)
    }
}

/// Errors that can occur while loading a policy.
#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("failed to parse policy YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("unsupported policy version {found} (max supported: {max})")]
    UnsupportedVersion { found: u32, max: u32 },
    #[error("threshold {field}={value} is outside [0.0, 1.0]")]
    OutOfRange { field: &'static str, value: f64 },
    #[error("prompt_threshold ({prompt}) must be ≤ deny_threshold ({deny})")]
    ThresholdsInverted { prompt: f64, deny: f64 },
}

#[derive(Debug, Default)]
struct SessionWindow {
    /// Most-recent actions, kept for audit/debug.
    actions: VecDeque<(i64, String)>,
    injection_attempts: u32,
    taint_violations: u32,
    /// Cached, monotonically increasing risk score in [0.0, 1.0].
    risk_score: f64,
}

/// SGNL-inspired continuous authorization engine.
#[derive(Debug)]
pub struct ContinuousAuthEngine {
    session_windows: DashMap<String, SessionWindow>,
    policy: PolicyConfig,
}

impl Default for ContinuousAuthEngine {
    fn default() -> Self { Self::new() }
}

impl ContinuousAuthEngine {
    /// Build an engine with the default policy (prompt 0.3, deny 0.7).
    pub fn new() -> Self {
        Self::with_policy(PolicyConfig::default())
    }

    /// Build an engine with a custom policy.
    pub fn with_policy(policy: PolicyConfig) -> Self {
        Self { session_windows: DashMap::new(), policy }
    }

    /// Borrow the active policy.
    pub fn policy(&self) -> &PolicyConfig {
        &self.policy
    }

    /// Evaluate whether `tool_id` may run for `session_id`. The base
    /// tier comes from the tool registry; the engine may **escalate**
    /// it (auto → prompt, prompt → deny) based on the session's risk
    /// score and the tool's override policy.
    pub fn evaluate(
        &self,
        tool_id: &str,
        session_id: &str,
        base_tier: PermissionTier,
    ) -> AuthDecision {
        let risk = self.risk_score(session_id);

        // 1. Tool override takes precedence
        if let Some(ovr) = self.policy.tool_overrides.get(tool_id) {
            if let Some(deny_at) = ovr.deny_above_risk {
                if risk > deny_at {
                    return AuthDecision::Deny;
                }
            }
            if let Some(prompt_at) = ovr.prompt_above_risk {
                if risk > prompt_at {
                    // After override-prompt, if base was already
                    // more restrictive, keep it.
                    if base_tier == PermissionTier::AutoApprove {
                        return AuthDecision::Prompt;
                    }
                }
            }
            if let Some(always) = ovr.always {
                return always;
            }
        }

        // 2. Tier escalation from risk score
        let escalated = self.escalate_tier(base_tier, risk);
        match escalated {
            PermissionTier::AutoApprove => AuthDecision::AutoApprove,
            PermissionTier::Prompt      => AuthDecision::Prompt,
            PermissionTier::Deny        => AuthDecision::Deny,
        }
    }

    /// Inbound-evaluation entry point (used when a third party
    /// provides an external risk score — e.g. an upstream SGNL
    /// gateway).
    pub fn evaluate_inbound(&self, _session_id: &str, risk_score: f64) -> AuthDecision {
        if risk_score > self.policy.deny_threshold {
            AuthDecision::Deny
        } else if risk_score > self.policy.prompt_threshold {
            AuthDecision::Prompt
        } else {
            AuthDecision::AutoApprove
        }
    }

    /// Record a generic session event (e.g. "tool_call:echo",
    /// "user_input", "llm_response"). Tracked for audit only;
    /// doesn't move the risk score.
    pub fn record_event(&self, session_id: &str, event_type: &str) {
        let now = now_ms();
        let mut w = self.session_windows.entry(session_id.to_string()).or_default();
        w.actions.push_back((now, event_type.to_string()));
        while w.actions.len() > 100 {
            w.actions.pop_front();
        }
    }

    /// Record a prompt-injection attempt. Increases the session's
    /// risk score.
    pub fn record_injection_attempt(&self, session_id: &str) {
        let mut w = self.session_windows.entry(session_id.to_string()).or_default();
        w.injection_attempts += 1;
        w.risk_score = recompute_risk(w.injection_attempts, w.taint_violations);
    }

    /// Record a taint-sink violation. Increases the session's
    /// risk score.
    pub fn record_taint_violation(&self, session_id: &str) {
        let mut w = self.session_windows.entry(session_id.to_string()).or_default();
        w.taint_violations += 1;
        w.risk_score = recompute_risk(w.injection_attempts, w.taint_violations);
    }

    /// Current risk score for `session_id` (0.0 if unknown).
    pub fn risk_score(&self, session_id: &str) -> f64 {
        self.session_windows.get(session_id).map(|w| w.risk_score).unwrap_or(0.0)
    }

    /// Number of injection attempts recorded in `session_id`.
    pub fn injection_attempts(&self, session_id: &str) -> u32 {
        self.session_windows.get(session_id).map(|w| w.injection_attempts).unwrap_or(0)
    }

    /// Number of taint violations recorded in `session_id`.
    pub fn taint_violations(&self, session_id: &str) -> u32 {
        self.session_windows.get(session_id).map(|w| w.taint_violations).unwrap_or(0)
    }

    /// Drop all session state.
    pub fn clear(&self) {
        self.session_windows.clear();
    }

    /// True if a decision is more restrictive than the base tier.
    pub fn is_escalated(base: PermissionTier, decision: AuthDecision) -> bool {
        matches!(
            (base, decision),
            (PermissionTier::AutoApprove, AuthDecision::Prompt)
                | (PermissionTier::AutoApprove, AuthDecision::Deny)
                | (PermissionTier::Prompt, AuthDecision::Deny)
        )
    }

    fn escalate_tier(&self, base: PermissionTier, risk: f64) -> PermissionTier {
        if risk > self.policy.deny_threshold {
            PermissionTier::Deny
        } else if risk > self.policy.prompt_threshold {
            match base {
                PermissionTier::AutoApprove => PermissionTier::Prompt,
                other => other,
            }
        } else {
            base
        }
    }
}

/// Pure-function form of the risk formula — exported for tests.
fn recompute_risk(injection_attempts: u32, taint_violations: u32) -> f64 {
    let inj = (injection_attempts as f64) * 0.2;
    let tnt = (taint_violations as f64) * 0.3;
    (inj + tnt).min(1.0)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> ContinuousAuthEngine {
        ContinuousAuthEngine::new()
    }

    #[test]
    fn clean_session_auto_approves() {
        let e = engine();
        assert_eq!(
            e.evaluate("echo", "s1", PermissionTier::AutoApprove),
            AuthDecision::AutoApprove
        );
    }

    #[test]
    fn base_tier_prompt_is_preserved() {
        let e = engine();
        assert_eq!(
            e.evaluate("send_email", "s1", PermissionTier::Prompt),
            AuthDecision::Prompt
        );
    }

    #[test]
    fn base_tier_deny_is_preserved() {
        let e = engine();
        assert_eq!(
            e.evaluate("rm_rf", "s1", PermissionTier::Deny),
            AuthDecision::Deny
        );
    }

    #[test]
    fn one_injection_attempt_keeps_risk_below_prompt() {
        // 1 * 0.2 = 0.2 < 0.3 (prompt threshold)
        let e = engine();
        e.record_injection_attempt("s1");
        assert!(e.risk_score("s1") < e.policy().prompt_threshold);
        assert_eq!(
            e.evaluate("echo", "s1", PermissionTier::AutoApprove),
            AuthDecision::AutoApprove
        );
    }

    #[test]
    fn two_injection_attempts_escalate_auto_to_prompt() {
        // 2 * 0.2 = 0.4 > 0.3
        let e = engine();
        e.record_injection_attempt("s1");
        e.record_injection_attempt("s1");
        assert!(e.risk_score("s1") > e.policy().prompt_threshold);
        assert_eq!(
            e.evaluate("echo", "s1", PermissionTier::AutoApprove),
            AuthDecision::Prompt
        );
    }

    #[test]
    fn four_injection_attempts_deny_auto() {
        // 4 * 0.2 = 0.8 > 0.7
        let e = engine();
        for _ in 0..4 {
            e.record_injection_attempt("s1");
        }
        assert!(e.risk_score("s1") > e.policy().deny_threshold);
        assert_eq!(
            e.evaluate("echo", "s1", PermissionTier::AutoApprove),
            AuthDecision::Deny
        );
    }

    #[test]
    fn three_taint_violations_deny_auto() {
        // 3 * 0.3 = 0.9 > 0.7
        let e = engine();
        for _ in 0..3 {
            e.record_taint_violation("s1");
        }
        assert_eq!(
            e.evaluate("echo", "s1", PermissionTier::AutoApprove),
            AuthDecision::Deny
        );
    }

    #[test]
    fn mixed_injection_and_taint_accumulate() {
        let e = engine();
        e.record_injection_attempt("s1");
        e.record_taint_violation("s1");
        // 0.2 + 0.3 = 0.5 → prompt
        assert!((e.risk_score("s1") - 0.5).abs() < 1e-9);
        assert_eq!(
            e.evaluate("echo", "s1", PermissionTier::AutoApprove),
            AuthDecision::Prompt
        );
    }

    #[test]
    fn risk_score_caps_at_one() {
        let e = engine();
        for _ in 0..100 {
            e.record_injection_attempt("s1");
        }
        assert!(e.risk_score("s1") <= 1.0);
    }

    #[test]
    fn tool_override_always_deny() {
        let policy = PolicyConfig {
            version: 1,
            prompt_threshold: 0.3,
            deny_threshold: 0.7,
            tool_overrides: HashMap::from([(
                "shell_exec".to_string(),
                ToolPolicy { always: Some(AuthDecision::Deny), ..Default::default() },
            )]),
        };
        let e = ContinuousAuthEngine::with_policy(policy);
        assert_eq!(
            e.evaluate("shell_exec", "s1", PermissionTier::AutoApprove),
            AuthDecision::Deny
        );
    }

    #[test]
    fn tool_override_deny_above_risk() {
        let policy = PolicyConfig {
            version: 1,
            prompt_threshold: 0.3,
            deny_threshold: 0.7,
            tool_overrides: HashMap::from([(
                "send_email".to_string(),
                ToolPolicy { deny_above_risk: Some(0.5), ..Default::default() },
            )]),
        };
        let e = ContinuousAuthEngine::with_policy(policy);
        // 1 injection attempt → 0.2 (under 0.3 prompt and 0.5 override)
        e.record_injection_attempt("s1");
        assert_eq!(
            e.evaluate("send_email", "s1", PermissionTier::AutoApprove),
            AuthDecision::AutoApprove,
        );
        // 2nd injection: 0.4 > 0.3 global prompt → escalated to Prompt.
        // Override does NOT fire (0.4 not > 0.5), so the global
        // prompt threshold takes over.
        e.record_injection_attempt("s1");
        assert_eq!(
            e.evaluate("send_email", "s1", PermissionTier::AutoApprove),
            AuthDecision::Prompt,
        );
        // 1 taint violation pushes risk to 0.7 > 0.5 → override-deny fires.
        e.record_taint_violation("s1");
        assert_eq!(
            e.evaluate("send_email", "s1", PermissionTier::AutoApprove),
            AuthDecision::Deny,
        );
    }

    #[test]
    fn tool_override_prompt_above_risk() {
        let policy = PolicyConfig {
            version: 1,
            prompt_threshold: 0.3,
            deny_threshold: 0.7,
            tool_overrides: HashMap::from([(
                "send_email".to_string(),
                ToolPolicy { prompt_above_risk: Some(0.1), ..Default::default() },
            )]),
        };
        let e = ContinuousAuthEngine::with_policy(policy);
        e.record_injection_attempt("s1"); // 0.2 > 0.1
        assert_eq!(
            e.evaluate("send_email", "s1", PermissionTier::AutoApprove),
            AuthDecision::Prompt,
        );
    }

    #[test]
    fn evaluate_inbound_respects_thresholds() {
        let e = engine();
        assert_eq!(e.evaluate_inbound("s1", 0.0),  AuthDecision::AutoApprove);
        assert_eq!(e.evaluate_inbound("s1", 0.29), AuthDecision::AutoApprove);
        assert_eq!(e.evaluate_inbound("s1", 0.31), AuthDecision::Prompt);
        assert_eq!(e.evaluate_inbound("s1", 0.69), AuthDecision::Prompt);
        assert_eq!(e.evaluate_inbound("s1", 0.71), AuthDecision::Deny);
        assert_eq!(e.evaluate_inbound("s1", 1.0),  AuthDecision::Deny);
    }

    #[test]
    fn sessions_are_isolated() {
        let e = engine();
        for _ in 0..10 {
            e.record_injection_attempt("s1");
        }
        assert_eq!(e.risk_score("s2"), 0.0);
        assert_eq!(e.risk_score("s1") > 0.0, true);
    }

    #[test]
    fn from_yaml_basic() {
        let y = r#"
version: 1
prompt_threshold: 0.4
deny_threshold: 0.8
tool_overrides:
  shell_exec:
    always: deny
"#;
        let p = PolicyConfig::from_yaml(y).unwrap();
        assert_eq!(p.prompt_threshold, 0.4);
        assert_eq!(p.deny_threshold, 0.8);
        assert!(p.tool_overrides.contains_key("shell_exec"));
    }

    #[test]
    fn from_yaml_rejects_unsupported_version() {
        let y = "version: 99\n";
        let r = PolicyConfig::from_yaml(y);
        assert!(matches!(r, Err(PolicyError::UnsupportedVersion { found: 99, max: 1 })));
    }

    #[test]
    fn from_yaml_rejects_inverted_thresholds() {
        let y = "version: 1\nprompt_threshold: 0.9\ndeny_threshold: 0.5\n";
        let r = PolicyConfig::from_yaml(y);
        assert!(matches!(r, Err(PolicyError::ThresholdsInverted { .. })));
    }

    #[test]
    fn from_yaml_rejects_out_of_range() {
        let y = "version: 1\nprompt_threshold: 1.5\ndeny_threshold: 0.7\n";
        let r = PolicyConfig::from_yaml(y);
        assert!(matches!(r, Err(PolicyError::OutOfRange { .. })));
    }

    #[test]
    fn is_escalated_table() {
        assert!(!ContinuousAuthEngine::is_escalated(PermissionTier::AutoApprove, AuthDecision::AutoApprove));
        assert!( ContinuousAuthEngine::is_escalated(PermissionTier::AutoApprove, AuthDecision::Prompt));
        assert!( ContinuousAuthEngine::is_escalated(PermissionTier::AutoApprove, AuthDecision::Deny));
        assert!(!ContinuousAuthEngine::is_escalated(PermissionTier::Prompt, AuthDecision::Prompt));
        assert!( ContinuousAuthEngine::is_escalated(PermissionTier::Prompt, AuthDecision::Deny));
    }

    #[test]
    fn auth_decision_as_str() {
        assert_eq!(AuthDecision::AutoApprove.as_str(), "auto_approve");
        assert_eq!(AuthDecision::Prompt.as_str(), "prompt");
        assert_eq!(AuthDecision::Deny.as_str(), "deny");
    }

    #[test]
    fn auth_decision_to_permission_tier_round_trip() {
        for d in [AuthDecision::AutoApprove, AuthDecision::Prompt, AuthDecision::Deny] {
            let t: PermissionTier = d.into();
            let back: AuthDecision = t.into();
            assert_eq!(d, back);
        }
    }

    #[test]
    fn clear_resets_all_sessions() {
        let e = engine();
        e.record_injection_attempt("s1");
        e.record_taint_violation("s1");
        e.clear();
        assert_eq!(e.risk_score("s1"), 0.0);
        assert_eq!(e.injection_attempts("s1"), 0);
        assert_eq!(e.taint_violations("s1"), 0);
    }
}
