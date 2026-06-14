//! Integration tests for the `hydragent-security` crate.
//!
//! These tests exercise the **on-disk** behaviour of `MerkleAuditChain` —
//! the in-memory contract is covered exhaustively in the unit tests in
//! `merkle.rs` and `signer.rs`. The integration suite focuses on:
//!
//! 1. **Persistence**: events survive a close/reopen cycle.
//! 2. **Cross-process tampering**: a row modified out-of-band (e.g. by a
//!    malicious DBA, a crashed writer, or a backup-restore gone wrong)
//!    is detected on the next verify.
//! 3. **Key rotation**: a chain signed by key A remains verifiable by
//!    holders of key A's public key after the agent rotates to key B.
//! 4. **Mixed event types**: every `AuditEventType` variant round-trips
//!    through the `event_type` SQLite column unchanged.
//!
//! DB files land in `target/tmp/audit-int-<pid>-<nanos>.db` and are
//! best-effort cleaned up at the end of each test.

use hydragent_security::{
    merkle::VerificationResult, AgentSigner, AuditEvent, AuditEventType, MerkleAuditChain,
};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Allocate a fresh DB path under `target/tmp/`.
fn fresh_db_path(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let dir = std::path::PathBuf::from("target/tmp");
    std::fs::create_dir_all(&dir).expect("create target/tmp");
    dir.join(format!("audit-int-{label}-{pid}-{nanos}.db"))
        .to_string_lossy()
        .into_owned()
}

fn fresh_signer() -> Arc<AgentSigner> {
    Arc::new(AgentSigner::generate())
}

#[tokio::test]
async fn file_persists_across_reopen() {
    let db = fresh_db_path("persist");
    let signer = fresh_signer();

    // Round 1: append 50 events.
    {
        let chain = MerkleAuditChain::connect(&db, signer.clone()).await.unwrap();
        assert_eq!(chain.count().await.unwrap(), 0);
        for i in 0..50 {
            let ev = AuditEvent::now(
                AuditEventType::ToolCall,
                format!("agent:cli:run-{i}"),
            )
            .with_page(format!("page-{i}"))
            .with_detail(serde_json::json!({"i": i, "tool": "echo"}));
            chain.append(ev).await.unwrap();
        }
        assert_eq!(chain.count().await.unwrap(), 50);
        // Pool drops here; SQLite file is flushed.
    }

    // Round 2: reopen with the **same** signer — signatures must still verify.
    let chain2 = MerkleAuditChain::connect(&db, signer.clone()).await.unwrap();
    assert_eq!(chain2.count().await.unwrap(), 50);
    let head_after_reopen = chain2.head_hash().await.unwrap().unwrap();
    let v = chain2.verify(true).await.unwrap();
    assert!(
        v.is_valid(),
        "chain should still verify after reopen: {v:?}"
    );
    assert_eq!(v.event_count(), 50);

    // Head hash survives across reopens: compute it again from a fresh
    // connection and confirm we land on the exact same value.
    let head_fresh = {
        let c = MerkleAuditChain::connect(&db, signer.clone()).await.unwrap();
        c.head_hash().await.unwrap().unwrap()
    };
    assert_eq!(head_fresh, head_after_reopen);

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn file_tamper_detected_after_reopen() {
    let db = fresh_db_path("tamper");
    let signer = fresh_signer();

    {
        let chain = MerkleAuditChain::connect(&db, signer.clone()).await.unwrap();
        for i in 0..10 {
            chain
                .append(AuditEvent::now(
                    AuditEventType::Outbound,
                    format!("user:alice:{i}"),
                ))
                .await
                .unwrap();
        }
    }

    // Out-of-band tamper: change the `event_json` of seq_id=5 directly
    // (simulating a malicious DB edit). All other rows are untouched.
    {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{db}?mode=rw"))
            .await
            .unwrap();
        sqlx::query("UPDATE audit_chain SET event_json = ? WHERE seq_id = 5")
            .bind(r#"{"event_type":"outbound","actor":"attacker","page_id":null,"detail":"pwned","timestamp_ms":0}"#)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    // Reopen: the next verify must flag seq_id=5 as Tampered.
    let chain = MerkleAuditChain::connect(&db, signer.clone()).await.unwrap();
    let v = chain.verify(true).await.unwrap();
    match v {
        VerificationResult::Tampered { seq_id, detail } => {
            assert_eq!(seq_id, 5, "tamper at seq_id=5 must be reported");
            assert!(
                detail.contains("event_hash mismatch")
                    || detail.contains("chain_hash mismatch")
                    || detail.contains("Ed25519"),
                "unexpected detail: {detail}"
            );
        }
        other => panic!("expected Tampered, got {other:?}"),
    }

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn external_key_verifies_old_chain_after_rotation() {
    let db = fresh_db_path("rotate");

    // Original signer — kept around for verification after rotation.
    let original = AgentSigner::generate();
    let original_vk = original.verifying_key();

    // Phase 1: sign 7 events with key A.
    {
        let chain = MerkleAuditChain::connect(&db, Arc::new(original)).await.unwrap();
        for i in 0..7 {
            chain
                .append(AuditEvent::now(
                    AuditEventType::AuthDecision,
                    format!("sgnl:policy-{}", i % 2),
                ))
                .await
                .unwrap();
        }
    }

    // Phase 2: the agent rotates to a new key. The old private key may
    // be discarded; only the public key is preserved for historical
    // verification. Open a brand-new chain (simulating the new signer
    // taking over) but ask it to verify the *old* signatures against
    // key A's public key.
    let rotated = AgentSigner::generate();
    let chain_rotated = MerkleAuditChain::connect(&db, Arc::new(rotated)).await.unwrap();
    let v = chain_rotated
        .verify_with_key(true, Some(&original_vk))
        .await
        .unwrap();
    assert!(
        v.is_valid(),
        "old chain should still verify against key A: {v:?}"
    );
    assert_eq!(v.event_count(), 7);

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn mixed_event_types_listed_in_order() {
    let db = fresh_db_path("types");
    let chain = MerkleAuditChain::connect(&db, fresh_signer()).await.unwrap();

    let types = [
        AuditEventType::AgentBoot,
        AuditEventType::Inbound,
        AuditEventType::ToolCall,
        AuditEventType::ToolCallComplete,
        AuditEventType::VaultAccess,
        AuditEventType::InjectionBlocked,
        AuditEventType::AuthDecision,
        AuditEventType::RiskUpdate,
        AuditEventType::TaintViolation,
        AuditEventType::ResponseSigned,
        AuditEventType::Outbound,
        AuditEventType::Other,
    ];
    for (i, t) in types.iter().enumerate() {
        chain
            .append(AuditEvent::now(*t, format!("actor-{i}")))
            .await
            .unwrap();
    }
    assert_eq!(chain.count().await.unwrap(), types.len() as u64);

    // ASC (oldest first): the first row must be AgentBoot.
    let asc = chain.list(20, 0, false).await.unwrap();
    assert_eq!(asc.len(), types.len());
    assert_eq!(asc[0].event_type, AuditEventType::AgentBoot.as_str());
    assert_eq!(asc.last().unwrap().event_type, AuditEventType::Other.as_str());

    // DESC (newest first): mirror image.
    let desc = chain.list(20, 0, true).await.unwrap();
    assert_eq!(desc[0].event_type, AuditEventType::Other.as_str());
    assert_eq!(
        desc.last().unwrap().event_type,
        AuditEventType::AgentBoot.as_str()
    );

    // Each event_type round-trips through the SQLite column without
    // losing its snake_case identity.
    for (i, expected) in types.iter().enumerate() {
        assert_eq!(asc[i].event_type, expected.as_str());
    }

    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn verify_with_key_rejects_tampered_signature() {
    let db = fresh_db_path("sigtamper");
    let signer = fresh_signer();

    {
        let chain = MerkleAuditChain::connect(&db, signer.clone()).await.unwrap();
        for i in 0..5 {
            chain
                .append(AuditEvent::now(AuditEventType::Other, format!("a{i}")))
                .await
                .unwrap();
        }
    }

    // Replace the signature on seq_id=2 with a valid-length but
    // completely wrong 64-byte payload. The hash chain stays intact
    // (so hash-checks pass) but the Ed25519 verify must fail.
    {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{db}?mode=rw"))
            .await
            .unwrap();
        let garbage = hex::encode([0xAAu8; 64]);
        sqlx::query("UPDATE audit_chain SET agent_signature = ? WHERE seq_id = 2")
            .bind(&garbage)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    let chain = MerkleAuditChain::connect(&db, signer.clone()).await.unwrap();
    // Without signature checks: chain looks valid.
    let v_no_sig = chain.verify(false).await.unwrap();
    assert!(v_no_sig.is_valid(), "hash chain alone should be valid");

    // With signature checks: must report Tampered at seq_id=2.
    let v_with_sig = chain.verify(true).await.unwrap();
    match v_with_sig {
        VerificationResult::Tampered { seq_id, detail } => {
            assert_eq!(seq_id, 2);
            assert!(
                detail.contains("Ed25519 signature invalid"),
                "got: {detail}"
            );
        }
        other => panic!("expected Tampered, got {other:?}"),
    }

    let _ = std::fs::remove_file(&db);
}
