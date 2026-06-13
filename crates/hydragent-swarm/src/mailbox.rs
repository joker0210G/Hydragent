//! # Agent Mailbox — file-based inter-agent messaging
//!
//! Phase 5 / Track 5.3. Sub-agents within a swarm (or across swarms) can
//! pass structured messages via a simple JSON-on-disk mailbox. The
//! mailbox is intentionally minimal — no schema registry, no consumers,
//! no envelope encryption — because the LLM-produced content is the
//! actual payload.
//!
//! ## Path layout
//!
//! ```text
//! {root}/
//!   {swarm_id}/
//!     mailbox/
//!       {to_agent_id}/
//!         {from_agent_id}.json     // latest message from "from" to "to"
//!         {from_agent_id}.seq      // monotonic seq counter (per from→to)
//! ```
//!
//! Why one file per `from→to` pair? The mailbox is a latest-wins
//! communication channel. Sibling agents that depend on each other's
//! progress typically only care about the most recent message; storing
//! a sequence (`.seq`) lets us detect "I haven't seen anything new since
//! last poll" without scanning every file.
//!
//! ## In-process notifications
//!
//! In addition to disk persistence (so messages survive process restarts
//! and can be observed by external tools), each `AgentMailbox` instance
//! has an in-process `tokio::sync::Notify` keyed by `to_agent_id`.
//! [`wait_for_inbox`](Self::wait_for_inbox) is a cheap way to block on
//! new mail arriving without busy-polling the filesystem.
//!
//! ## Example
//!
//! ```no_run
//! use hydragent_swarm::mailbox::{AgentMailbox, MailMessage};
//!
//! # async fn demo() -> anyhow::Result<()> {
//! let mb = AgentMailbox::new("data/swarm").await?;
//! mb.write("swarm-1", "agent-B", "agent-A", &MailMessage {
//!     kind: "research_finding".into(),
//!     content: "Actix-Web benchmarks 1.2x faster than Axum".into(),
//!     refs: vec!["https://...".into()],
//!     at_ms: 0,
//! }).await?;
//!
//! let inbox = mb.list_inbox("swarm-1", "agent-B").await?;
//! assert_eq!(inbox.len(), 1);
//! # Ok(()) }
//! ```

use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Notify};
use tracing::{debug, info, warn};

/// A single message between two agents.
///
/// `kind` is a free-form short tag the agents agree on (e.g.,
/// `"research_finding"`, `"request"`, `"escalation"`). `content` is the
/// human/LLM-readable body. `refs` is an optional list of resource
/// pointers (URLs, file paths, previous message ids).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MailMessage {
    pub kind: String,
    pub content: String,
    #[serde(default)]
    pub refs: Vec<String>,
    /// Unix epoch milliseconds (filled in by `write` if zero).
    #[serde(default)]
    pub at_ms: i64,
}

/// An entry in an agent's inbox view: who sent it + the message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxEntry {
    pub from: String,
    pub message: MailMessage,
    /// Monotonic counter for this (from, to) pair. Starts at 1.
    pub seq: u64,
    /// Disk mtime (epoch ms) of the file at the time it was listed.
    pub file_mtime_ms: i64,
}

/// Errors specific to mailbox operations.
#[derive(Debug, thiserror::Error)]
pub enum MailboxError {
    #[error("invalid id: {0}")]
    InvalidId(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("mailbox error: {0}")]
    Other(String),
}

const MAILBOX_SUBDIR: &str = "mailbox";
const SEQ_SUFFIX: &str = ".seq";

/// Validates a swarm_id / agent_id for safe path use. Disallows `..`,
/// `/`, `\`, and NUL bytes; allows alphanumerics, `-`, `_`, `.`.
pub fn validate_id(id: &str) -> Result<(), MailboxError> {
    if id.is_empty() {
        return Err(MailboxError::InvalidId("empty".into()));
    }
    if id.contains("..") {
        return Err(MailboxError::InvalidId(format!("contains '..': {id}")));
    }
    if id.contains('/') || id.contains('\\') || id.contains('\0') {
        return Err(MailboxError::InvalidId(format!("contains path separator or NUL: {id}")));
    }
    // Anything else (Unicode, spaces) is fine since the file lives in a
    // dedicated mailbox root; we only block traversal.
    Ok(())
}

/// The mailbox facade. Cheap to clone — all mutable state is in `Arc`s.
#[derive(Clone)]
pub struct AgentMailbox {
    root: PathBuf,
    /// Per-(swarm, to_agent) Notify so `wait_for_inbox` can wake up.
    notifiers: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
}

impl AgentMailbox {
    /// Create a mailbox rooted at `root`. The directory is created if
    /// it does not exist. Safe to call multiple times — creation is
    /// idempotent.
    pub async fn new<P: AsRef<Path>>(root: P) -> Result<Self, MailboxError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).await
            .map_err(|e| MailboxError::Other(format!(
                "creating mailbox root {}: {e}", root.display()
            )))?;
        info!(root = %root.display(), "AgentMailbox initialized");
        Ok(Self {
            root,
            notifiers: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Path to the per-swarm mailbox directory.
    pub fn swarm_dir(&self, swarm_id: &str) -> PathBuf {
        self.root.join(swarm_id)
    }

    /// Path to an agent's inbox directory inside a swarm.
    pub fn inbox_dir(&self, swarm_id: &str, to_agent: &str) -> PathBuf {
        self.swarm_dir(swarm_id).join(MAILBOX_SUBDIR).join(to_agent)
    }

    /// Path to a specific message file (latest message from `from` to `to`).
    pub fn message_path(&self, swarm_id: &str, to_agent: &str, from_agent: &str) -> PathBuf {
        self.inbox_dir(swarm_id, to_agent).join(format!("{from_agent}.json"))
    }

    /// Path to the per-(from,to) monotonic sequence file.
    pub fn seq_path(&self, swarm_id: &str, to_agent: &str, from_agent: &str) -> PathBuf {
        self.inbox_dir(swarm_id, to_agent).join(format!("{from_agent}{SEQ_SUFFIX}"))
    }

    /// Write a message from `from_agent` to `to_agent` inside `swarm_id`.
    /// Atomically replaces the existing message file (via `.tmp` rename)
    /// and increments the per-pair sequence counter. Fires an in-process
    /// notification so any `wait_for_inbox` callers wake up.
    pub async fn write(
        &self,
        swarm_id: &str,
        to_agent: &str,
        from_agent: &str,
        message: &MailMessage,
    ) -> Result<u64, MailboxError> {
        validate_id(swarm_id)?;
        validate_id(to_agent)?;
        validate_id(from_agent)?;
        if to_agent == from_agent {
            return Err(MailboxError::Other(
                "self-mail is not allowed (from == to)".into(),
            ));
        }

        let inbox = self.inbox_dir(swarm_id, to_agent);
        fs::create_dir_all(&inbox).await.map_err(|e| MailboxError::Other(format!(
            "creating inbox dir {}: {e}", inbox.display()
        )))?;

        // Bump sequence first (so the on-disk file agrees with the
        // message's `seq` we return).
        let seq_path = self.seq_path(swarm_id, to_agent, from_agent);
        let seq = bump_seq(&seq_path).await?;

        // Build the message we will persist: stamp `at_ms` if caller
        // didn't, attach the seq.
        let mut msg = message.clone();
        if msg.at_ms == 0 {
            msg.at_ms = chrono::Utc::now().timestamp_millis();
        }

        let on_disk = PersistedMessage { seq, message: msg };
        let json = serde_json::to_string_pretty(&on_disk)
            .map_err(MailboxError::Json)?;

        // Atomic write: write tmp, then rename.
        let final_path = self.message_path(swarm_id, to_agent, from_agent);
        let tmp_path = final_path.with_extension("json.tmp");
        {
            let mut f = fs::File::create(&tmp_path).await.map_err(|e| {
                MailboxError::Other(format!("creating tmp {}: {e}", tmp_path.display()))
            })?;
            f.write_all(json.as_bytes()).await?;
            f.sync_all().await.ok();
        }
        // Best-effort rename (on Windows, rename-over requires the
        // destination not to be open, which is always the case for
        // a per-pair file).
        if let Err(e) = fs::rename(&tmp_path, &final_path).await {
            // Fallback: remove the destination and retry.
            warn!(error = %e, "rename failed, retrying after delete");
            let _ = fs::remove_file(&final_path).await;
            fs::rename(&tmp_path, &final_path).await.map_err(|e| {
                MailboxError::Other(format!(
                    "renaming {} -> {}: {e}",
                    tmp_path.display(), final_path.display()
                ))
            })?;
        }

        debug!(
            swarm = %swarm_id,
            from = %from_agent,
            to = %to_agent,
            seq,
            kind = %on_disk.message.kind,
            "Mailbox message written"
        );

        // Fire in-process notification.
        self.notify(swarm_id, to_agent).await;

        Ok(seq)
    }

    /// Read the latest message from `from_agent` to `to_agent` (if any).
    pub async fn read(
        &self,
        swarm_id: &str,
        to_agent: &str,
        from_agent: &str,
    ) -> Result<Option<InboxEntry>, MailboxError> {
        validate_id(swarm_id)?;
        validate_id(to_agent)?;
        validate_id(from_agent)?;
        let path = self.message_path(swarm_id, to_agent, from_agent);
        match fs::read(&path).await {
            Ok(bytes) => {
                let persisted: PersistedMessage = serde_json::from_slice(&bytes)
                    .map_err(|e| MailboxError::Json(e))?;
                let mtime = file_mtime_ms(&path).await.unwrap_or(0);
                Ok(Some(InboxEntry {
                    from: from_agent.to_string(),
                    message: persisted.message,
                    seq: persisted.seq,
                    file_mtime_ms: mtime,
                }))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(MailboxError::Io(e)),
        }
    }

    /// List all messages currently in `to_agent`'s inbox.
    pub async fn list_inbox(
        &self,
        swarm_id: &str,
        to_agent: &str,
    ) -> Result<Vec<InboxEntry>, MailboxError> {
        validate_id(swarm_id)?;
        validate_id(to_agent)?;
        let inbox = self.inbox_dir(swarm_id, to_agent);
        if !inbox.exists() {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        let mut rd = fs::read_dir(&inbox).await?;
        while let Some(entry) = rd.next_entry().await? {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.ends_with(".json") || name.ends_with(".json.tmp") {
                continue;
            }
            let from = name.trim_end_matches(".json").to_string();
            let path = entry.path();
            let bytes = match fs::read(&path).await {
                Ok(b) => b,
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "skipping unreadable mail file");
                    continue;
                }
            };
            let persisted: PersistedMessage = match serde_json::from_slice(&bytes) {
                Ok(p) => p,
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "skipping unparseable mail file");
                    continue;
                }
            };
            let mtime = file_mtime_ms(&path).await.unwrap_or(0);
            out.push(InboxEntry {
                from,
                message: persisted.message,
                seq: persisted.seq,
                file_mtime_ms: mtime,
            });
        }
        // Most recent first.
        out.sort_by(|a, b| b.file_mtime_ms.cmp(&a.file_mtime_ms));
        Ok(out)
    }

    /// Delete the entire inbox of `to_agent` in `swarm_id` (all senders).
    /// Returns the number of files removed.
    pub async fn clear_inbox(
        &self,
        swarm_id: &str,
        to_agent: &str,
    ) -> Result<usize, MailboxError> {
        validate_id(swarm_id)?;
        validate_id(to_agent)?;
        let inbox = self.inbox_dir(swarm_id, to_agent);
        if !inbox.exists() {
            return Ok(0);
        }
        let mut count = 0;
        let mut rd = fs::read_dir(&inbox).await?;
        while let Some(entry) = rd.next_entry().await? {
            if fs::remove_file(entry.path()).await.is_ok() {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Wait (asynchronously) for **any** new message in `to_agent`'s inbox
    /// inside `swarm_id`. Returns immediately if there is already at
    /// least one message; otherwise blocks until someone calls `write`
    /// with the same `to_agent`.
    ///
    /// Useful for sibling coordination: "B can sit on
    /// `mailbox.wait_for_inbox("swarm-1", "B")` while A is researching."
    pub async fn wait_for_inbox(
        &self,
        swarm_id: &str,
        to_agent: &str,
    ) -> Result<(), MailboxError> {
        validate_id(swarm_id)?;
        validate_id(to_agent)?;
        // Fast path: if there's already mail, return immediately.
        if !self.list_inbox(swarm_id, to_agent).await?.is_empty() {
            return Ok(());
        }
        let key = format!("{swarm_id}\x00{to_agent}");
        let notify = {
            let mut map = self.notifiers.lock().await;
            map.entry(key)
                .or_insert_with(|| Arc::new(Notify::new()))
                .clone()
        };
        notify.notified().await;
        Ok(())
    }

    /// Fire the in-process notification for `to_agent` in `swarm_id`.
    async fn notify(&self, swarm_id: &str, to_agent: &str) {
        let key = format!("{swarm_id}\x00{to_agent}");
        let notify = {
            let mut map = self.notifiers.lock().await;
            map.entry(key)
                .or_insert_with(|| Arc::new(Notify::new()))
                .clone()
        };
        notify.notify_waiters();
    }
}

/// On-disk message envelope. The `seq` is redundant with the `.seq`
/// file (it is also persisted here so a single `read` is self-contained),
/// but the `.seq` file is the source of truth for monotonicity.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedMessage {
    seq: u64,
    message: MailMessage,
}

/// Atomically increment and return the new sequence value for a
/// (from, to) pair. Sequence starts at 1.
async fn bump_seq(path: &Path) -> Result<u64, MailboxError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let current = match fs::read_to_string(path).await {
        Ok(s) => s.trim().parse::<u64>().unwrap_or(0),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(MailboxError::Io(e)),
    };
    let next = current + 1;
    let tmp = path.with_extension("seq.tmp");
    {
        let mut f = fs::File::create(&tmp).await?;
        f.write_all(next.to_string().as_bytes()).await?;
        f.sync_all().await.ok();
    }
    if let Err(_) = fs::rename(&tmp, path).await {
        let _ = fs::remove_file(path).await;
        fs::rename(&tmp, path).await?;
    }
    Ok(next)
}

/// File mtime as epoch milliseconds. Returns 0 on error.
async fn file_mtime_ms(path: &Path) -> std::io::Result<i64> {
    let meta = fs::metadata(path).await?;
    let modified = meta.modified()?;
    let dur = modified
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Ok(dur.as_millis() as i64)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn new_mb() -> (TempDir, AgentMailbox) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mb = AgentMailbox::new(dir.path()).await.expect("mailbox init");
        (dir, mb)
    }

    fn m(kind: &str, content: &str) -> MailMessage {
        MailMessage {
            kind: kind.to_string(),
            content: content.to_string(),
            refs: vec![],
            at_ms: 0,
        }
    }

    #[test]
    fn validate_id_rejects_empty_and_traversal() {
        assert!(validate_id("").is_err());
        assert!(validate_id("..").is_err());
        assert!(validate_id("../foo").is_err());
        assert!(validate_id("a/b").is_err());
        assert!(validate_id("a\\b").is_err());
        assert!(validate_id("a\0b").is_err());
        assert!(validate_id("agent-007").is_ok());
        assert!(validate_id("swarm_id_42").is_ok());
    }

    #[tokio::test]
    async fn write_then_read_returns_message() {
        let (_dir, mb) = new_mb().await;
        let msg = MailMessage {
            kind: "research_finding".into(),
            content: "actix is fast".into(),
            refs: vec![],
            at_ms: 0,
        };
        let seq = mb.write("s1", "B", "A", &msg).await.unwrap();
        assert_eq!(seq, 1);
        let got = mb.read("s1", "B", "A").await.unwrap().unwrap();
        assert_eq!(got.from, "A");
        assert_eq!(got.message.kind, "research_finding");
        assert_eq!(got.message.content, "actix is fast");
        assert_eq!(got.seq, 1);
        assert!(got.message.at_ms > 0);
    }

    #[tokio::test]
    async fn write_overwrites_previous_message_from_same_sender() {
        let (_dir, mb) = new_mb().await;
        let m1 = MailMessage { kind: "k".into(), content: "first".into(), refs: vec![], at_ms: 1 };
        let m2 = MailMessage { kind: "k".into(), content: "second".into(), refs: vec![], at_ms: 2 };
        assert_eq!(mb.write("s1", "B", "A", &m1).await.unwrap(), 1);
        assert_eq!(mb.write("s1", "B", "A", &m2).await.unwrap(), 2);
        let got = mb.read("s1", "B", "A").await.unwrap().unwrap();
        assert_eq!(got.message.content, "second");
        assert_eq!(got.seq, 2);
    }

    #[tokio::test]
    async fn inbox_lists_messages_from_multiple_senders() {
        let (_dir, mb) = new_mb().await;
        mb.write("s1", "B", "A", &m("k", "from A")).await.unwrap();
        mb.write("s1", "B", "C", &m("k", "from C")).await.unwrap();
        mb.write("s1", "B", "D", &m("k", "from D")).await.unwrap();
        let inbox = mb.list_inbox("s1", "B").await.unwrap();
        assert_eq!(inbox.len(), 3);
        let senders: Vec<_> = inbox.iter().map(|e| e.from.clone()).collect();
        assert!(senders.contains(&"A".to_string()));
        assert!(senders.contains(&"C".to_string()));
        assert!(senders.contains(&"D".to_string()));
    }

    #[tokio::test]
    async fn read_returns_none_for_unknown_sender() {
        let (_dir, mb) = new_mb().await;
        let got = mb.read("s1", "B", "ghost").await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn inbox_separated_per_swarm() {
        let (_dir, mb) = new_mb().await;
        mb.write("s1", "B", "A", &m("k", "x")).await.unwrap();
        mb.write("s2", "B", "A", &m("k", "y")).await.unwrap();
        assert_eq!(mb.list_inbox("s1", "B").await.unwrap().len(), 1);
        assert_eq!(mb.list_inbox("s2", "B").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn inbox_separated_per_recipient() {
        let (_dir, mb) = new_mb().await;
        mb.write("s1", "B", "A", &m("k", "for B")).await.unwrap();
        mb.write("s1", "C", "A", &m("k", "for C")).await.unwrap();
        let b = mb.list_inbox("s1", "B").await.unwrap();
        let c = mb.list_inbox("s1", "C").await.unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(c.len(), 1);
        assert_eq!(b[0].message.content, "for B");
        assert_eq!(c[0].message.content, "for C");
    }

    #[tokio::test]
    async fn self_mail_is_rejected() {
        let (_dir, mb) = new_mb().await;
        let res = mb.write("s1", "A", "A", &m("k", "x")).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn clear_inbox_removes_all_messages() {
        let (_dir, mb) = new_mb().await;
        mb.write("s1", "B", "A", &m("k", "1")).await.unwrap();
        mb.write("s1", "B", "C", &m("k", "2")).await.unwrap();
        // 2 senders × 2 files (message + seq counter) = 4 files in B's inbox.
        assert_eq!(mb.list_inbox("s1", "B").await.unwrap().len(), 2);
        let n = mb.clear_inbox("s1", "B").await.unwrap();
        assert_eq!(n, 4);
        assert_eq!(mb.list_inbox("s1", "B").await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn wait_for_inbox_returns_immediately_when_mail_exists() {
        let (_dir, mb) = new_mb().await;
        mb.write("s1", "B", "A", &m("k", "hi")).await.unwrap();
        let start = std::time::Instant::now();
        mb.wait_for_inbox("s1", "B").await.unwrap();
        assert!(start.elapsed().as_millis() < 100);
    }

    #[tokio::test]
    async fn wait_for_inbox_blocks_until_write() {
        let (_dir, mb) = new_mb().await;
        let mb2 = mb.clone();
        let handle = tokio::spawn(async move {
            mb2.wait_for_inbox("s1", "B").await.unwrap();
        });
        // No message yet — give the task a moment to start blocking.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!handle.is_finished());
        mb.write("s1", "B", "A", &m("k", "wake up")).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("wait_for_inbox did not wake up after write")
            .unwrap();
    }

    #[tokio::test]
    async fn message_survives_mailbox_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        {
            let mb = AgentMailbox::new(&path).await.unwrap();
            mb.write("s1", "B", "A", &m("k", "persistent")).await.unwrap();
        }
        let mb2 = AgentMailbox::new(&path).await.unwrap();
        let got = mb2.read("s1", "B", "A").await.unwrap().unwrap();
        assert_eq!(got.message.content, "persistent");
        assert_eq!(got.seq, 1);
    }

    #[tokio::test]
    async fn list_inbox_returns_empty_for_unknown_swarm() {
        let (_dir, mb) = new_mb().await;
        let inbox = mb.list_inbox("never-seen", "B").await.unwrap();
        assert!(inbox.is_empty());
    }
}
