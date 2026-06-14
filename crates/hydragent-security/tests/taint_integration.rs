// crates/hydragent-security/tests/taint_integration.rs
//
// Integration tests for the taint sink policy module. These test
// the public API surface as re-exported from the crate root, plus
// file-based YAML loading.

use std::collections::BTreeMap;
use std::io::Write;

use hydragent_security::{
    SinkPolicy, SinkRule, TaintCategory, TaintError, TaintSet, TaintSink, TaintViolation,
};

#[test]
fn re_exports_are_constructible() {
    // Verify that the re-exports from the crate root are usable
    // without referring to the inner `taint` module.
    let _policy: SinkPolicy = SinkPolicy::default_policy();
    let _rule: SinkRule = SinkRule::default();
    let _cat: TaintCategory = TaintCategory::Secret;
    let _set: TaintSet = TaintSet::singleton(TaintCategory::PII);
    let _sink: TaintSink = TaintSink::Log;
    let _err: TaintError = TaintError::UnsupportedVersion { found: 1, max: 1 };
    let _v: TaintViolation = TaintViolation {
        sink: TaintSink::Log,
        forbidden: vec![TaintCategory::Secret],
    };
}

#[test]
fn default_policy_rejects_secret_everywhere() {
    let policy = SinkPolicy::default_policy();
    let secret = TaintSet::singleton(TaintCategory::Secret);
    for sink in TaintSink::ALL {
        let result = policy.enforce(sink, &secret);
        assert!(result.is_err(), "Secret must be blocked at sink {sink}");
        let v = result.unwrap_err();
        assert_eq!(v.sink, sink);
        assert!(v.forbidden.contains(&TaintCategory::Secret));
    }
}

#[test]
fn default_policy_redacts_pii_at_log() {
    let policy = SinkPolicy::default_policy();
    let pii = TaintSet::singleton(TaintCategory::PII);
    assert!(policy.requires_redact(TaintSink::Log, &pii));
    // But PII is allowed at Log (it's not in the forbid list).
    assert!(policy.enforce(TaintSink::Log, &pii).is_ok());
}

#[test]
fn yaml_loader_handles_all_sinks_from_file() {
    // Write a minimal policy to a temp file and load it back.
    let dir = std::env::temp_dir().join("hydragent_security_taint_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("policy.yaml");
    let yaml = r#"
version: 1
sinks:
  log:
    forbid: [secret]
    redact: [pii]
  outbound_network:
    forbid: [secret, pii, system_internal, llm_output]
    redact: []
"#;
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(yaml.as_bytes()).unwrap();
    drop(f);

    let contents = std::fs::read_to_string(&path).unwrap();
    let policy = SinkPolicy::load_yaml(&contents).unwrap();

    assert_eq!(policy.version, 1);
    assert_eq!(policy.sinks.len(), 2);

    // Verify the file-based policy has the same effect as the
    // default for the sinks it covers.
    let secret = TaintSet::singleton(TaintCategory::Secret);
    assert!(policy.enforce(TaintSink::Log, &secret).is_err());
    assert!(policy.enforce(TaintSink::OutboundNetwork, &secret).is_err());

    // Sinks not in the file should be allowed (no rule = allow).
    let pii = TaintSet::singleton(TaintCategory::PII);
    assert!(policy.enforce(TaintSink::LlmRequestMessages, &pii).is_ok());

    // Cleanup
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn yaml_loader_rejects_unsupported_version() {
    let yaml = "version: 999\nsinks: {}\n";
    let err = SinkPolicy::load_yaml(yaml).unwrap_err();
    match err {
        TaintError::UnsupportedVersion { found, max } => {
            assert_eq!(found, 999);
            assert_eq!(max, SinkPolicy::SUPPORTED_VERSION);
        }
        other => panic!("expected UnsupportedVersion, got {other:?}"),
    }
}

#[test]
fn yaml_loader_accepts_empty_sinks_map() {
    let yaml = "version: 1\nsinks: {}\n";
    let policy = SinkPolicy::load_yaml(yaml).unwrap();
    assert_eq!(policy.sinks, BTreeMap::new());
    // With no rules, every taint is allowed.
    let everything: TaintSet = TaintCategory::ALL.into_iter().collect();
    for sink in TaintSink::ALL {
        assert!(policy.enforce(sink, &everything).is_ok());
        assert!(!policy.requires_redact(sink, &everything));
    }
}

#[test]
fn end_to_end_secret_leak_blocked() {
    // Simulate the full lifecycle: an agent fetches a value tagged
    // as Secret, attempts to send it through outbound network, and
    // the policy blocks the operation.
    let policy = SinkPolicy::default_policy();
    let leaked_value = TaintSet::singleton(TaintCategory::Secret);
    let attempt = policy.enforce(TaintSink::OutboundNetwork, &leaked_value);
    let violation = attempt.expect_err("Secret must be blocked at outbound_network");
    assert_eq!(violation.sink, TaintSink::OutboundNetwork);
    assert!(violation.to_string().contains("secret"));
    assert!(violation.to_string().contains("outbound_network"));
}

#[test]
fn end_to_end_pii_logged_after_redaction_marker() {
    // PII is allowed at Log, but the caller must redact it first.
    // The policy's `requires_redact` is the signal to do so.
    let policy = SinkPolicy::default_policy();
    let pii_email = TaintSet::singleton(TaintCategory::PII);
    assert!(policy.enforce(TaintSink::Log, &pii_email).is_ok());
    assert!(policy.requires_redact(TaintSink::Log, &pii_email));
    // The caller is then responsible for actually redacting.
    // (The redactor itself is out of scope for Track 6.2.)
}
