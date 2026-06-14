//! Tamper-evident, SHA-256-chained audit log backed by SQLite.
//!
//! # Chain structure
//!
//! For each event in order:
//!
//! ```text
//!   event_hash[n]     = SHA-256(canonical_json(event[n]))
//!   chain_hash[n]     = SHA-256(prev_hash[n] || event_hash[n])
//!   agent_signature[n]= Ed25519(signing_key, chain_hash[n])
//! ```
//!
//! The first event's `prev_hash` is [`GENESIS_HASH`]. Verification scans
//! the chain in `seq_id` order, recomputes every hash, and optionally
//! validates each Ed25519 signature. Any tampered or deleted row produces
//! a [`VerificationResult::Tampered`] with the exact `seq_id`.
//!
//! # Storage
//!
//! Backed by a *separate* SQLite database at `data/audit/chain.db`. This
//! keeps the audit chain out of the session/memory DB so a corruption in
//! one cannot poison the other, and so the chain can be archived
//! independently (see PHASE_6.md §10 — chain grows unbounded; 90-day
//! archival is a future track).
//!
//! # Why a separate `event_json` column?
//!
//! Storing the full event JSON verbatim alongside the hash means an
//! auditor with read-only DB access can recompute the hash and verify
//! integrity without needing the producer's in-memory state. This is
//! what makes the chain "publish the head hash, anyone can verify".

use ed25519_dalek::Verifier;
use hydragent_types::AuditEvent;
#[cfg(test)]
use hydragent_types::AuditEventType;
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};
use std::sync::Arc;

use crate::signer::AgentSigner;

/// Genesis hash used as `prev_hash` of the first audit event.
///
/// 64 hex zeros — 32 bytes of zero. Distinct from any real SHA-256 output
/// because SHA-256 always produces a value with a non-zero probability
/// distribution over `[0, 2^256)`. Encoding as a hex string keeps the
/// column type uniform (`TEXT`) for index scans.
pub const GENESIS_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// A single row of the `audit_chain` table, exposed for listing and
/// external export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainRow {
    pub seq_id: i64,
    pub event_type: String,
    pub actor: String,
    pub page_id: Option<String>,
    pub event_json: String,
    pub event_hash: String,
    pub prev_hash: String,
    pub chain_hash: String,
    pub agent_signature: String,
    pub timestamp_ms: i64,
}

/// Result of [`MerkleAuditChain::verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationResult {
    /// Chain is intact from genesis to the latest row.
    Valid { event_count: u64 },
    /// Chain is broken at `seq_id`; `detail` describes what failed.
    Tampered { seq_id: i64, detail: String },
}

impl VerificationResult {
    pub fn is_valid(&self) -> bool {
        matches!(self, VerificationResult::Valid { .. })
    }

    /// Number of events in the verified prefix. Always 0 for `Tampered`
    /// (we stop walking at the first break, so the count is unreliable).
    pub fn event_count(&self) -> u64 {
        match self {
            VerificationResult::Valid { event_count } => *event_count,
            VerificationResult::Tampered { .. } => 0,
        }
    }
}

/// Tamper-evident, cryptographically chained audit log.
///
/// Construct with [`MerkleAuditChain::connect`] (file-backed) or
/// [`MerkleAuditChain::new` (in-memory for tests)].
#[derive(Clone)]
pub struct MerkleAuditChain {
    pool: SqlitePool,
    signer: Arc<AgentSigner>,
}

impl MerkleAuditChain {
    /// Open a file-backed chain at `db_path`, ensuring the schema is
    /// present. Creates the file if it does not exist.
    pub async fn connect(
        db_path: &str,
        signer: Arc<AgentSigner>,
    ) -> anyhow::Result<Self> {
        let url = format!("sqlite://{db_path}?mode=rwc");
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await?;
        Self::initialize(&pool).await?;
        Ok(Self { pool, signer })
    }

    /// In-memory chain (for tests). Schema is initialized automatically.
    pub async fn in_memory(signer: Arc<AgentSigner>) -> anyhow::Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        Self::initialize(&pool).await?;
        Ok(Self { pool, signer })
    }

    /// Create the `audit_chain` table and its indexes. Idempotent.
    pub async fn initialize(pool: &SqlitePool) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS audit_chain (
                seq_id          INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type      TEXT    NOT NULL,
                actor           TEXT    NOT NULL,
                page_id         TEXT,
                event_json      TEXT    NOT NULL,
                event_hash      TEXT    NOT NULL,
                prev_hash       TEXT    NOT NULL,
                chain_hash      TEXT    NOT NULL,
                agent_signature TEXT    NOT NULL,
                timestamp_ms    INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_audit_chain_session ON audit_chain(page_id, seq_id);
            CREATE INDEX IF NOT EXISTS idx_audit_chain_type    ON audit_chain(event_type, timestamp_ms);
            "#,
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Append a new audit event. This is the **only** way to add events
    /// to the chain. Computes the full hash chain and Ed25519 signature
    /// in a single SQL transaction.
    pub async fn append(&self, event: AuditEvent) -> anyhow::Result<i64> {
        // 1. Canonical JSON of the event. serde_json uses the field
        //    declaration order of `AuditEvent`, which is stable.
        let event_json = serde_json::to_string(&event)?;

        // 2. event_hash = SHA-256(event_json)
        let event_hash = hex::encode(Sha256::digest(event_json.as_bytes()));

        // 3. prev_hash = chain_hash of the previous row (or GENESIS)
        let prev_hash: String = sqlx::query_scalar(
            "SELECT chain_hash FROM audit_chain ORDER BY seq_id DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or_else(|| GENESIS_HASH.to_string());

        // 4. chain_hash = SHA-256(prev_hash || event_hash)
        let mut hasher = Sha256::new();
        hasher.update(prev_hash.as_bytes());
        hasher.update(event_hash.as_bytes());
        let chain_hash = hex::encode(hasher.finalize());

        // 5. Sign chain_hash with the agent's Ed25519 signing key.
        let sig = self.signer.sign_bytes(chain_hash.as_bytes());
        let sig_hex = hex::encode(sig.to_bytes());

        let event_type = event.event_type.as_str().to_string();
        let actor = event.actor.clone();
        let page_id = event.page_id.clone();
        let timestamp_ms = event.timestamp_ms;

        // 6. Insert atomically. Use a transaction so partial state can't
        //    be observed by concurrent verifiers.
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
            INSERT INTO audit_chain
                (event_type, actor, page_id, event_json, event_hash,
                 prev_hash, chain_hash, agent_signature, timestamp_ms)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&event_type)
        .bind(&actor)
        .bind(&page_id)
        .bind(&event_json)
        .bind(&event_hash)
        .bind(&prev_hash)
        .bind(&chain_hash)
        .bind(&sig_hex)
        .bind(timestamp_ms)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let seq_id = result.last_insert_rowid();

        tracing::debug!(
            seq_id,
            event_type = %event_type,
            chain_head = &chain_hash[..16],
            "Audit event appended to Merkle chain"
        );

        Ok(seq_id)
    }

    /// Verify the entire chain from genesis to the latest row.
    ///
    /// If `verify_sigs` is true, also validate each row's Ed25519
    /// signature against this signer's public key. (For external
    /// verification with an arbitrary pubkey, use
    /// [`MerkleAuditChain::verify_with_key`].)
    pub async fn verify(&self, verify_sigs: bool) -> anyhow::Result<VerificationResult> {
        self.verify_with_key(verify_sigs, None).await
    }

    /// Verify the chain, optionally using a specific public key for
    /// Ed25519 checks. Pass `None` to use the chain's own signer.
    pub async fn verify_with_key(
        &self,
        verify_sigs: bool,
        external_key: Option<&ed25519_dalek::VerifyingKey>,
    ) -> anyhow::Result<VerificationResult> {
        let rows = sqlx::query(
            r#"
            SELECT seq_id, event_type, actor, page_id, event_json, event_hash,
                   prev_hash, chain_hash, agent_signature, timestamp_ms
            FROM audit_chain
            ORDER BY seq_id ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        if rows.is_empty() {
            return Ok(VerificationResult::Valid { event_count: 0 });
        }

        let mut prev_chain_hash = GENESIS_HASH.to_string();

        for row in &rows {
            let seq_id: i64 = row.try_get("seq_id")?;
            let event_json: String = row.try_get("event_json")?;
            let event_hash: String = row.try_get("event_hash")?;
            let prev_hash: String = row.try_get("prev_hash")?;
            let chain_hash: String = row.try_get("chain_hash")?;
            let agent_signature: String = row.try_get("agent_signature")?;

            // 1. event_hash = SHA-256(event_json) ?
            let computed_event_hash =
                hex::encode(Sha256::digest(event_json.as_bytes()));
            if computed_event_hash != event_hash {
                return Ok(VerificationResult::Tampered {
                    seq_id,
                    detail: format!(
                        "event_hash mismatch: expected {}, got {}",
                        computed_event_hash, event_hash
                    ),
                });
            }

            // 2. prev_hash matches previous chain_hash ?
            if prev_hash != prev_chain_hash {
                return Ok(VerificationResult::Tampered {
                    seq_id,
                    detail: format!(
                        "prev_hash chain broken: expected {}, got {}",
                        prev_chain_hash, prev_hash
                    ),
                });
            }

            // 3. chain_hash = SHA-256(prev_hash || event_hash) ?
            let mut hasher = Sha256::new();
            hasher.update(prev_hash.as_bytes());
            hasher.update(event_hash.as_bytes());
            let computed_chain_hash = hex::encode(hasher.finalize());
            if computed_chain_hash != chain_hash {
                return Ok(VerificationResult::Tampered {
                    seq_id,
                    detail: format!(
                        "chain_hash recomputation failed at seq_id {}",
                        seq_id
                    ),
                });
            }

            // 4. Optional: Ed25519 signature check
            if verify_sigs {
                let sig_bytes = hex::decode(&agent_signature)?;
                if sig_bytes.len() != 64 {
                    return Ok(VerificationResult::Tampered {
                        seq_id,
                        detail: format!(
                            "invalid signature length: {}",
                            sig_bytes.len()
                        ),
                    });
                }
                let sig_array: [u8; 64] = sig_bytes
                    .try_into()
                    .expect("length checked above");
                let sig = ed25519_dalek::Signature::from_bytes(&sig_array);
                let key = external_key
                    .copied()
                    .unwrap_or_else(|| self.signer.verifying_key());
                if let Err(e) = key.verify(chain_hash.as_bytes(), &sig) {
                    return Ok(VerificationResult::Tampered {
                        seq_id,
                        detail: format!("Ed25519 signature invalid: {e}"),
                    });
                }
            }

            prev_chain_hash = chain_hash;
        }

        Ok(VerificationResult::Valid {
            event_count: rows.len() as u64,
        })
    }

    /// Return the current chain head hash, or `None` if the chain is empty.
    pub async fn head_hash(&self) -> anyhow::Result<Option<String>> {
        let hash: Option<String> = sqlx::query_scalar(
            "SELECT chain_hash FROM audit_chain ORDER BY seq_id DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(hash)
    }

    /// Total number of events in the chain.
    pub async fn count(&self) -> anyhow::Result<u64> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_chain")
            .fetch_one(&self.pool)
            .await?;
        Ok(n.max(0) as u64)
    }

    /// List rows in `seq_id` order. Most recent last by default;
    /// pass `reverse = true` to get newest first (useful for the CLI).
    pub async fn list(
        &self,
        limit: u32,
        offset: u32,
        reverse: bool,
    ) -> anyhow::Result<Vec<ChainRow>> {
        let order = if reverse { "DESC" } else { "ASC" };
        let sql = format!(
            r#"
            SELECT seq_id, event_type, actor, page_id, event_json, event_hash,
                   prev_hash, chain_hash, agent_signature, timestamp_ms
            FROM audit_chain
            ORDER BY seq_id {order}
            LIMIT ? OFFSET ?
            "#
        );
        let rows = sqlx::query(&sql)
            .bind(limit as i64)
            .bind(offset as i64)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                Ok::<_, sqlx::Error>(ChainRow {
                    seq_id: r.try_get("seq_id")?,
                    event_type: r.try_get("event_type")?,
                    actor: r.try_get("actor")?,
                    page_id: r.try_get("page_id")?,
                    event_json: r.try_get("event_json")?,
                    event_hash: r.try_get("event_hash")?,
                    prev_hash: r.try_get("prev_hash")?,
                    chain_hash: r.try_get("chain_hash")?,
                    agent_signature: r.try_get("agent_signature")?,
                    timestamp_ms: r.try_get("timestamp_ms")?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Direct access to the underlying pool (for advanced callers / tests).
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

// `AuditEventType` is re-exported here so callers can pattern-match on
// the event type variant when building events.
pub use hydragent_types::AuditEventType as EventType;

#[cfg(test)]
mod tests {
    use super::*;
    use hydragent_types::AuditEvent;
    use std::sync::Arc;

    fn make_signer() -> Arc<AgentSigner> {
        Arc::new(AgentSigner::generate())
    }

    fn make_event(seq: usize) -> AuditEvent {
        AuditEvent::now(AuditEventType::Other, format!("test:{seq}"))
            .with_page(format!("page-{seq}"))
            .with_detail(serde_json::json!({"seq": seq}))
    }

    #[tokio::test]
    async fn empty_chain_verifies() {
        let chain = MerkleAuditChain::in_memory(make_signer()).await.unwrap();
        let r = chain.verify(false).await.unwrap();
        assert_eq!(r, VerificationResult::Valid { event_count: 0 });
        let r2 = chain.verify(true).await.unwrap();
        assert!(r2.is_valid());
    }

    #[tokio::test]
    async fn single_event_chains_from_genesis() {
        let chain = MerkleAuditChain::in_memory(make_signer()).await.unwrap();
        chain.append(make_event(0)).await.unwrap();

        let row = chain.list(1, 0, false).await.unwrap();
        assert_eq!(row.len(), 1);
        assert_eq!(row[0].prev_hash, GENESIS_HASH);

        let r = chain.verify(true).await.unwrap();
        assert_eq!(r, VerificationResult::Valid { event_count: 1 });
    }

    #[tokio::test]
    async fn chain_links_correctly_across_100_events() {
        let chain = MerkleAuditChain::in_memory(make_signer()).await.unwrap();
        for i in 0..100 {
            chain.append(make_event(i)).await.unwrap();
        }
        assert_eq!(chain.count().await.unwrap(), 100);

        // Each row's prev_hash must equal the previous row's chain_hash.
        let rows = chain.list(100, 0, false).await.unwrap();
        assert_eq!(rows.len(), 100);
        let mut expected_prev = GENESIS_HASH.to_string();
        for r in &rows {
            assert_eq!(r.prev_hash, expected_prev, "row seq_id={}", r.seq_id);
            expected_prev = r.chain_hash.clone();
        }

        // Full verify with signatures
        let v = chain.verify(true).await.unwrap();
        assert_eq!(v, VerificationResult::Valid { event_count: 100 });
    }

    #[tokio::test]
    async fn tampered_event_json_detected() {
        let chain = MerkleAuditChain::in_memory(make_signer()).await.unwrap();
        for i in 0..10 {
            chain.append(make_event(i)).await.unwrap();
        }

        // Tamper: replace event_json at seq_id=5 with garbage.
        sqlx::query("UPDATE audit_chain SET event_json = '{\"tampered\":true}' WHERE seq_id = 5")
            .execute(chain.pool())
            .await
            .unwrap();

        let v = chain.verify(false).await.unwrap();
        match v {
            VerificationResult::Tampered { seq_id, detail } => {
                assert_eq!(seq_id, 5);
                assert!(detail.contains("event_hash mismatch"), "got: {detail}");
            }
            other => panic!("expected Tampered, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn deleted_row_detected() {
        let chain = MerkleAuditChain::in_memory(make_signer()).await.unwrap();
        for i in 0..10 {
            chain.append(make_event(i)).await.unwrap();
        }

        // Delete row at seq_id=5. The next row's prev_hash will no longer
        // match row 4's chain_hash.
        sqlx::query("DELETE FROM audit_chain WHERE seq_id = 5")
            .execute(chain.pool())
            .await
            .unwrap();

        let v = chain.verify(false).await.unwrap();
        match v {
            VerificationResult::Tampered { seq_id, detail } => {
                // The detection point is the row AFTER the deletion (seq_id=6).
                assert_eq!(seq_id, 6);
                assert!(detail.contains("prev_hash chain broken"), "got: {detail}");
            }
            other => panic!("expected Tampered, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tampered_signature_detected() {
        let chain = MerkleAuditChain::in_memory(make_signer()).await.unwrap();
        for i in 0..5 {
            chain.append(make_event(i)).await.unwrap();
        }

        // Corrupt the signature at seq_id=3 (replace with valid-length garbage).
        let garbage = hex::encode([0xAAu8; 64]);
        sqlx::query("UPDATE audit_chain SET agent_signature = ? WHERE seq_id = 3")
            .bind(&garbage)
            .execute(chain.pool())
            .await
            .unwrap();

        let v = chain.verify(true).await.unwrap();
        match v {
            VerificationResult::Tampered { seq_id, detail } => {
                assert_eq!(seq_id, 3);
                assert!(detail.contains("Ed25519 signature invalid"), "got: {detail}");
            }
            other => panic!("expected Tampered, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn head_hash_returns_latest() {
        let chain = MerkleAuditChain::in_memory(make_signer()).await.unwrap();
        assert!(chain.head_hash().await.unwrap().is_none());

        for i in 0..3 {
            chain.append(make_event(i)).await.unwrap();
        }
        let head = chain.head_hash().await.unwrap().unwrap();
        let newest = chain.list(1, 0, true).await.unwrap();
        assert_eq!(head, newest[0].chain_hash);
    }

    #[tokio::test]
    async fn list_reverse_returns_newest_first() {
        let chain = MerkleAuditChain::in_memory(make_signer()).await.unwrap();
        for i in 0..5 {
            chain.append(make_event(i)).await.unwrap();
        }
        let asc = chain.list(5, 0, false).await.unwrap();
        let desc = chain.list(5, 0, true).await.unwrap();
        assert_eq!(asc.first().unwrap().seq_id, 1);
        assert_eq!(desc.first().unwrap().seq_id, 5);
    }

    #[tokio::test]
    async fn verify_with_external_key_rejects_wrong_pubkey() {
        let chain = MerkleAuditChain::in_memory(make_signer()).await.unwrap();
        chain.append(make_event(0)).await.unwrap();

        let wrong_pub: [u8; 32] = AgentSigner::generate().public_key_bytes();
        let wrong_key =
            ed25519_dalek::VerifyingKey::from_bytes(&wrong_pub).unwrap();
        let v = chain.verify_with_key(true, Some(&wrong_key)).await.unwrap();
        match v {
            VerificationResult::Tampered { seq_id, detail } => {
                assert_eq!(seq_id, 1);
                assert!(detail.contains("Ed25519 signature invalid"), "got: {detail}");
            }
            other => panic!("expected Tampered, got {other:?}"),
        }

        // Internal verify still passes.
        let v2 = chain.verify(true).await.unwrap();
        assert!(v2.is_valid());
    }
}
