//! # hydragent-security
//!
//! Phase 6 security pipeline. Track 6.1 ships:
//! - [`signer::AgentSigner`] — Ed25519 keypair for non-repudiable action receipts
//! - [`merkle::MerkleAuditChain`] — tamper-evident, SHA-256-chained SQLite audit log
//!
//! Track 6.2 ships:
//! - [`taint::SinkPolicy`] — YAML-driven policy that decides which taint
//!   categories may reach which outbound sinks
//!
//! Track 6.3 ships:
//! - [`sanitizer::InputSanitizer`] — YAML-driven prompt-injection
//!   pattern library with a hot-path [`regex::RegexSet`] for matching
//! - [`anomaly::AnomalyDetector`] — per-session sliding-window
//!   detector for rate-limit, breadth, injection, and taint anomalies
//! - [`sgnl::ContinuousAuthEngine`] — SGNL-inspired continuous
//!   authorization that maps a session's risk score to a per-tool
//!   [`sgnl::AuthDecision`]
//!
//! The shared audit-record types ([`AuditEvent`], [`AuditEventType`],
//! [`SignedToolCall`], [`SignedResponse`]) and the shared taint primitives
//! ([`TaintCategory`], [`TaintSet`], [`TaintedValue`]) live in
//! `hydragent-types` and are re-exported here so callers can
//! `use hydragent_security::{AuditEvent, TaintCategory, ...}` without
//! depending on `hydragent-types` directly`.
//!
//! Upcoming tracks:
//! - 6.4: Column-level AES for sensitive DB columns + mlock/VirtualLock + credential rotation
//!
//! See `doc/phases/PHASE_6.md` for the full specification.

pub mod anomaly;
pub mod merkle;
pub mod sanitizer;
pub mod sgnl;
pub mod signer;
pub mod taint;

pub use hydragent_types::{
    AuditEvent, AuditEventType, SignedResponse, SignedToolCall, TaintCategory, TaintSet,
    TaintedValue,
};
pub use anomaly::{now_ms, AnomalyConfig, AnomalyDetector, AnomalyFlag, AnomalyKind};
pub use merkle::{ChainRow, MerkleAuditChain, VerificationResult, GENESIS_HASH};
pub use sanitizer::{
    InjectionPattern, InjectionPatternsFile, InputSanitizer, PatternSeverity,
    SanitizationResult, SanitizerError,
};
pub use sgnl::{
    AuthDecision, ContinuousAuthEngine, PolicyConfig, PolicyError, ToolPolicy,
};
pub use signer::{AgentSigner, SignerError};
pub use taint::{SinkPolicy, SinkRule, TaintError, TaintSink, TaintViolation};
