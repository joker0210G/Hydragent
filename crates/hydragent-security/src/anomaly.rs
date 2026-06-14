//! Phase 6 / Track 6.3 — Anomaly Detector (Layer 06)
//!
//! Maintains a per-session sliding window of the last `window_size`
//! actions (default 100). On every recorded action, returns a list of
//! [`AnomalyFlag`]s describing any threshold breaches the action
//! triggered:
//!
//!   1. **RateLimit**        — more than `max_calls_per_minute`
//!                             actions in the last 60 s
//!   2. **BreadthAnomaly**   — more than `max_unique_tools_in_30s`
//!                             distinct tool ids in the last 30 s
//!   3. **InjectionAttempts** — more than `max_injection_attempts`
//!                             [`record_injection_attempt`] calls in
//!                             the session
//!   4. **TaintViolation**   — at least one
//!                             [`record_taint_violation`] call in the
//!                             session (any sink violation is a strong
//!                             signal)
//!
//! The detector is `Send + Sync` via [`dashmap::DashMap`] and is
//! driven explicitly by the orchestrator (or the SGNL engine) — the
//! call sites decide what `at_ms` timestamp to pass so tests don't
//! need `tokio::time::sleep`.
//!
//! See `doc/phases/PHASE_6.md` §5.5 (AnomalyDetector) for the spec.

use dashmap::DashMap;
use std::collections::{HashSet, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

/// What a flag is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnomalyKind {
    /// Too many tool calls in a short time window.
    RateLimit,
    /// Too many distinct tools in a short time window.
    BreadthAnomaly,
    /// Too many prompt-injection attempts in this session.
    InjectionAttempts,
    /// At least one taint-sink violation attempt in this session.
    TaintViolation,
}

impl AnomalyKind {
    /// Stable, lowercase string for audit logs and dashboards.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RateLimit        => "rate_limit",
            Self::BreadthAnomaly   => "breadth_anomaly",
            Self::InjectionAttempts => "injection_attempts",
            Self::TaintViolation   => "taint_violation",
        }
    }
}

impl std::fmt::Display for AnomalyKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One anomaly observation, returned by [`AnomalyDetector::record_action`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnomalyFlag {
    pub kind: AnomalyKind,
    pub detail: String,
    pub timestamp_ms: i64,
}

/// Tunable thresholds. All time-based windows are hard-coded to the
/// spec values (60 s, 30 s) so that we don't accidentally drift.
#[derive(Debug, Clone)]
pub struct AnomalyConfig {
    /// Window of recent actions kept per session.
    pub window_size: usize,
    /// Flag if more than this many actions happened in the last 60 s.
    pub max_calls_per_minute: usize,
    /// Flag if more than this many distinct tool ids appeared in the last 30 s.
    pub max_unique_tools_in_30s: usize,
    /// Flag if more than this many injection attempts happened in the session.
    pub max_injection_attempts: usize,
}

impl Default for AnomalyConfig {
    fn default() -> Self {
        Self {
            window_size: 100,
            max_calls_per_minute: 50,
            max_unique_tools_in_30s: 5,
            max_injection_attempts: 3,
        }
    }
}

/// Per-session state.
#[derive(Debug, Default)]
struct SessionWindow {
    /// Most-recent first iteration is more useful, so we use
    /// `VecDeque` and `push_back` to keep chronological order.
    actions: VecDeque<ActionRecord>,
    injection_attempts: u32,
    taint_violations: u32,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // `success` is part of the per-action record; reserved for future
                     // audit and "deny after N consecutive failures" logic.
struct ActionRecord {
    timestamp_ms: i64,
    tool_id: String,
    success: bool,
}

/// Concurrent per-session anomaly detector.
#[derive(Debug)]
pub struct AnomalyDetector {
    windows: DashMap<String, SessionWindow>,
    config: AnomalyConfig,
}

impl Default for AnomalyDetector {
    fn default() -> Self { Self::new() }
}

impl AnomalyDetector {
    pub fn new() -> Self {
        Self::with_config(AnomalyConfig::default())
    }

    pub fn with_config(config: AnomalyConfig) -> Self {
        Self { windows: DashMap::new(), config }
    }

    /// Record a tool invocation at the given wall-clock millisecond
    /// timestamp. Returns any anomaly flags this action triggered
    /// (may be empty). The flag list is a snapshot — subsequent
    /// calls will see updated state.
    ///
    /// `at_ms` is injected to keep tests deterministic; the caller
    /// is expected to use [`now_ms`] in production.
    pub fn record_action(
        &self,
        session_id: &str,
        tool_id: &str,
        success: bool,
        at_ms: i64,
    ) -> Vec<AnomalyFlag> {
        let mut flags = Vec::new();

        // Update window
        {
            let mut w = self.windows.entry(session_id.to_string()).or_default();
            w.actions.push_back(ActionRecord {
                timestamp_ms: at_ms,
                tool_id: tool_id.to_string(),
                success,
            });
            while w.actions.len() > self.config.window_size {
                w.actions.pop_front();
            }
        }

        // Re-borrow immutably to read state.
        let w = self.windows.get(session_id).expect("just inserted");

        // Flag 1: rate limit
        let last_60s: usize = w.actions.iter()
            .rev()
            .take_while(|a| at_ms - a.timestamp_ms <= 60_000)
            .count();
        if last_60s > self.config.max_calls_per_minute {
            flags.push(AnomalyFlag {
                kind: AnomalyKind::RateLimit,
                detail: format!(
                    "{} tool calls in 60s (max: {})",
                    last_60s, self.config.max_calls_per_minute
                ),
                timestamp_ms: at_ms,
            });
        }

        // Flag 2: breadth
        let last_30s: HashSet<&str> = w.actions.iter()
            .rev()
            .take_while(|a| at_ms - a.timestamp_ms <= 30_000)
            .map(|a| a.tool_id.as_str())
            .collect();
        if last_30s.len() > self.config.max_unique_tools_in_30s {
            flags.push(AnomalyFlag {
                kind: AnomalyKind::BreadthAnomaly,
                detail: format!(
                    "{} unique tools in 30s (max: {})",
                    last_30s.len(), self.config.max_unique_tools_in_30s
                ),
                timestamp_ms: at_ms,
            });
        }

        // Flag 3: injection attempts threshold
        if w.injection_attempts as usize > self.config.max_injection_attempts {
            flags.push(AnomalyFlag {
                kind: AnomalyKind::InjectionAttempts,
                detail: format!(
                    "{} injection attempts in session (max: {})",
                    w.injection_attempts, self.config.max_injection_attempts
                ),
                timestamp_ms: at_ms,
            });
        }

        // Flag 4: taint violation (any count > 0)
        if w.taint_violations > 0 {
            flags.push(AnomalyFlag {
                kind: AnomalyKind::TaintViolation,
                detail: format!(
                    "{} taint violation(s) in session",
                    w.taint_violations
                ),
                timestamp_ms: at_ms,
            });
        }

        flags
    }

    /// Record one prompt-injection attempt (from the sanitizer).
    pub fn record_injection_attempt(&self, session_id: &str) {
        let mut w = self.windows.entry(session_id.to_string()).or_default();
        w.injection_attempts += 1;
    }

    /// Record one taint-sink violation attempt.
    pub fn record_taint_violation(&self, session_id: &str) {
        let mut w = self.windows.entry(session_id.to_string()).or_default();
        w.taint_violations += 1;
    }

    /// Number of injection attempts in `session_id`'s window.
    pub fn injection_attempts(&self, session_id: &str) -> u32 {
        self.windows.get(session_id).map(|w| w.injection_attempts).unwrap_or(0)
    }

    /// Number of taint violations in `session_id`'s window.
    pub fn taint_violations(&self, session_id: &str) -> u32 {
        self.windows.get(session_id).map(|w| w.taint_violations).unwrap_or(0)
    }

    /// Total actions recorded in the window.
    pub fn action_count(&self, session_id: &str) -> usize {
        self.windows.get(session_id).map(|w| w.actions.len()).unwrap_or(0)
    }

    /// True if any anomaly thresholds have been breached for the session.
    pub fn has_flags(&self, session_id: &str, at_ms: i64) -> bool {
        !self.record_action(session_id, "__probe__", true, at_ms).is_empty()
    }

    /// Drop all session state. Useful for tests.
    pub fn clear(&self) {
        self.windows.clear();
    }

    /// Borrow the active configuration.
    pub fn config(&self) -> &AnomalyConfig {
        &self.config
    }
}

/// Current wall-clock time in milliseconds since the Unix epoch.
/// Uses the OS clock directly; safe to call from any thread.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "sess-1";
    const T0: i64 = 1_700_000_000_000;

    #[test]
    fn clean_session_produces_no_flags() {
        let d = AnomalyDetector::new();
        let flags = d.record_action(SESSION, "echo", true, T0);
        assert!(flags.is_empty());
        assert_eq!(d.action_count(SESSION), 1);
    }

    #[test]
    fn rate_limit_triggers_after_threshold() {
        let d = AnomalyDetector::new();
        // 50 actions within 60 s should NOT trigger
        for i in 0..50 {
            let f = d.record_action(SESSION, "echo", true, T0 + i * 1000);
            assert!(!f.iter().any(|x| x.kind == AnomalyKind::RateLimit),
                "triggered too early at i={i}");
        }
        // 51st should trigger
        let f = d.record_action(SESSION, "echo", true, T0 + 50_500);
        assert!(f.iter().any(|x| x.kind == AnomalyKind::RateLimit));
    }

    #[test]
    fn rate_limit_resets_after_60s() {
        let d = AnomalyDetector::new();
        // Fill the window at T0
        for i in 0..51 {
            d.record_action(SESSION, "echo", true, T0 + i);
        }
        // 10 minutes later, 5 fresh calls should NOT trigger
        let later = T0 + 600_000;
        for i in 0..5 {
            let f = d.record_action(SESSION, "echo", true, later + i);
            assert!(!f.iter().any(|x| x.kind == AnomalyKind::RateLimit),
                "rate limit still firing at later+i={i}");
        }
    }

    #[test]
    fn breadth_triggers_when_too_many_distinct_tools() {
        let d = AnomalyDetector::new();
        // 6 different tools within 30 s should trigger breadth
        for i in 0..6 {
            let name = format!("tool-{i}");
            d.record_action(SESSION, &name, true, T0 + i * 1000);
        }
        let f = d.record_action(SESSION, "tool-6", true, T0 + 6_500);
        assert!(f.iter().any(|x| x.kind == AnomalyKind::BreadthAnomaly));
    }

    #[test]
    fn breadth_does_not_trigger_for_repeats() {
        let d = AnomalyDetector::new();
        for i in 0..20 {
            d.record_action(SESSION, "echo", true, T0 + i * 100);
        }
        // Still 1 unique tool — no breadth flag
        let f = d.record_action(SESSION, "echo", true, T0 + 2_100);
        assert!(!f.iter().any(|x| x.kind == AnomalyKind::BreadthAnomaly));
    }

    #[test]
    fn injection_attempts_threshold() {
        let d = AnomalyDetector::new();
        d.record_action(SESSION, "echo", true, T0);
        // 3 attempts not yet a flag
        for _ in 0..3 {
            d.record_injection_attempt(SESSION);
        }
        let f = d.record_action(SESSION, "echo", true, T0 + 1000);
        assert!(!f.iter().any(|x| x.kind == AnomalyKind::InjectionAttempts));
        // 4th attempt crosses threshold
        d.record_injection_attempt(SESSION);
        let f = d.record_action(SESSION, "echo", true, T0 + 2000);
        assert!(f.iter().any(|x| x.kind == AnomalyKind::InjectionAttempts));
    }

    #[test]
    fn taint_violation_flag_appears_on_first() {
        let d = AnomalyDetector::new();
        d.record_taint_violation(SESSION);
        let f = d.record_action(SESSION, "echo", true, T0);
        assert!(f.iter().any(|x| x.kind == AnomalyKind::TaintViolation));
    }

    #[test]
    fn window_is_bounded() {
        let cfg = AnomalyConfig { window_size: 5, ..AnomalyConfig::default() };
        let d = AnomalyDetector::with_config(cfg);
        for i in 0..20 {
            d.record_action(SESSION, "echo", true, T0 + i);
        }
        assert_eq!(d.action_count(SESSION), 5);
    }

    #[test]
    fn sessions_are_isolated() {
        let d = AnomalyDetector::new();
        for _ in 0..4 {
            d.record_injection_attempt("sess-A");
        }
        d.record_injection_attempt("sess-B");
        assert_eq!(d.injection_attempts("sess-A"), 4);
        assert_eq!(d.injection_attempts("sess-B"), 1);
    }

    #[test]
    fn clear_resets_state() {
        let d = AnomalyDetector::new();
        for _ in 0..10 {
            d.record_injection_attempt(SESSION);
        }
        d.clear();
        assert_eq!(d.injection_attempts(SESSION), 0);
    }

    #[test]
    fn anomaly_kind_as_str() {
        assert_eq!(AnomalyKind::RateLimit.as_str(), "rate_limit");
        assert_eq!(AnomalyKind::BreadthAnomaly.as_str(), "breadth_anomaly");
        assert_eq!(AnomalyKind::InjectionAttempts.as_str(), "injection_attempts");
        assert_eq!(AnomalyKind::TaintViolation.as_str(), "taint_violation");
    }
}
