//! Integration tests for `AgentMailbox` — file-based inter-agent messaging.
//!
//! These tests exercise the public surface (write/read/list_inbox/clear)
//! using real on-disk mailboxes in a per-test `tempdir`. They are
//! parallel-safe because each test uses its own root dir.

use std::time::Duration;

use hydragent_swarm::mailbox::{AgentMailbox, MailMessage};
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

#[tokio::test]
async fn diamond_pattern_two_parents_to_one_child() {
    // Mirrors the canonical diamond DAG (A→B, A→C, B+C→D): A and C
    // both write to B's inbox, and B can read both before acting.
    let (_dir, mb) = new_mb().await;
    mb.write("swarm-1", "B", "A", &m("research", "actix is fast")).await.unwrap();
    mb.write("swarm-1", "B", "C", &m("research", "axum is ergonomic")).await.unwrap();

    let inbox = mb.list_inbox("swarm-1", "B").await.unwrap();
    assert_eq!(inbox.len(), 2, "B should see mail from both A and C");
    let senders: Vec<_> = inbox.iter().map(|e| e.from.clone()).collect();
    assert!(senders.contains(&"A".to_string()));
    assert!(senders.contains(&"C".to_string()));
}

#[tokio::test]
async fn many_concurrent_writes_all_succeed() {
    let (_dir, mb) = new_mb().await;
    let mut handles = vec![];
    for i in 0..20 {
        let mb2 = mb.clone();
        handles.push(tokio::spawn(async move {
            mb2.write("s", "inbox", &format!("sender-{i}"), &m("k", &format!("msg {i}"))).await
        }));
    }
    for h in handles {
        let seq = h.await.unwrap().unwrap();
        assert!(seq >= 1, "sequence must be >= 1 (got {seq})");
    }
    let inbox = mb.list_inbox("s", "inbox").await.unwrap();
    assert_eq!(inbox.len(), 20, "all 20 senders' messages should be visible");
}

#[tokio::test]
async fn sequence_numbers_are_per_pair_and_monotonic() {
    let (_dir, mb) = new_mb().await;
    assert_eq!(mb.write("s", "B", "A", &m("k", "1")).await.unwrap(), 1);
    assert_eq!(mb.write("s", "B", "A", &m("k", "2")).await.unwrap(), 2);
    assert_eq!(mb.write("s", "B", "A", &m("k", "3")).await.unwrap(), 3);
    // A different sender starts fresh at 1.
    assert_eq!(mb.write("s", "B", "C", &m("k", "x")).await.unwrap(), 1);
    // And A's counter didn't get touched by C's write.
    assert_eq!(mb.write("s", "B", "A", &m("k", "4")).await.unwrap(), 4);
}

#[tokio::test]
async fn mailbox_path_layout_matches_documented_contract() {
    let dir = tempfile::tempdir().unwrap();
    let mb = AgentMailbox::new(dir.path()).await.unwrap();
    mb.write("s1", "B", "A", &m("k", "x")).await.unwrap();

    let expected_msg = dir.path().join("s1").join("mailbox").join("B").join("A.json");
    let expected_seq = dir.path().join("s1").join("mailbox").join("B").join("A.seq");
    assert!(expected_msg.exists(), "expected {} to exist", expected_msg.display());
    assert!(expected_seq.exists(), "expected {} to exist", expected_seq.display());

    let seq_content = std::fs::read_to_string(&expected_seq).unwrap();
    assert_eq!(seq_content.trim(), "1");
}

#[tokio::test]
async fn read_after_external_modification_picks_up_changes() {
    // Simulates the "another process wrote a file" case.
    let dir = tempfile::tempdir().unwrap();
    let mb = AgentMailbox::new(dir.path()).await.unwrap();

    let target = dir.path().join("s1").join("mailbox").join("B");
    std::fs::create_dir_all(&target).unwrap();
    let msg = serde_json::json!({
        "seq": 1u64,
        "message": {
            "kind": "external",
            "content": "written by another process",
            "refs": [],
            "at_ms": 1234567890i64,
        }
    });
    std::fs::write(target.join("ext.json"), msg.to_string()).unwrap();

    let got = mb.read("s1", "B", "ext").await.unwrap().unwrap();
    assert_eq!(got.message.kind, "external");
    assert_eq!(got.message.content, "written by another process");
    assert_eq!(got.message.at_ms, 1_234_567_890);
}

#[tokio::test]
async fn wait_for_inbox_wakes_up_within_200ms_of_a_write() {
    let (_dir, mb) = new_mb().await;
    let mb2 = mb.clone();
    let waiter = tokio::spawn(async move {
        mb2.wait_for_inbox("s", "B").await.unwrap();
    });
    // Give the waiter time to start blocking on the Notify.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiter.is_finished());
    let start = std::time::Instant::now();
    mb.write("s", "B", "A", &m("k", "wake")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), waiter).await
        .expect("wait_for_inbox did not wake up after write")
        .unwrap();
    // Should be well under 2s — we just want to assert it actually
    // returned in a reasonable time.
    assert!(start.elapsed() < Duration::from_millis(200));
}

#[tokio::test]
async fn clear_inbox_does_not_touch_other_recipients() {
    let (_dir, mb) = new_mb().await;
    mb.write("s", "B", "A", &m("k", "for B")).await.unwrap();
    mb.write("s", "B", "C", &m("k", "for B")).await.unwrap();
    mb.write("s", "D", "A", &m("k", "for D")).await.unwrap();
    // 2 senders × 2 files (message + seq counter) = 4 files in B's inbox.
    let removed = mb.clear_inbox("s", "B").await.unwrap();
    assert_eq!(removed, 4);
    assert!(mb.list_inbox("s", "B").await.unwrap().is_empty());
    // D's inbox should be untouched.
    assert_eq!(mb.list_inbox("s", "D").await.unwrap().len(), 1);
}
