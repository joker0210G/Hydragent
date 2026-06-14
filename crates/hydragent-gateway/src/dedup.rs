use std::sync::Mutex;
use lru::LruCache;
use sha2::{Sha256, Digest};
use std::num::NonZeroUsize;

pub struct Deduplicator {
    cache: Mutex<LruCache<[u8; 32], std::time::Instant>>,
}

impl Deduplicator {
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::new(1000).unwrap());
        Self {
            cache: Mutex::new(LruCache::new(cap)),
        }
    }

    pub fn is_duplicate(&self, channel_id: &str, user_id: &str, content: &str, request_id: &str) -> bool {
        let mut hasher = Sha256::new();
        hasher.update(channel_id.as_bytes());
        hasher.update(user_id.as_bytes());
        hasher.update(content.as_bytes());
        // Include the per-request UUID so two distinct requests with
        // identical (channel, user, content) are NOT treated as replays
        // of each other. Only a true *replay* of the *same* JSON-RPC
        // request id within the window will be deduped.
        hasher.update(request_id.as_bytes());
        let hash: [u8; 32] = hasher.finalize().into();

        let mut cache = self.cache.lock().unwrap();
        let now = std::time::Instant::now();

        if let Some(&prev_time) = cache.get(&hash) {
            if now.duration_since(prev_time) < std::time::Duration::from_secs(30) {
                return true;
            }
        }

        cache.put(hash, now);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deduplicator() {
        let dedup = Deduplicator::new(2);

        // First message should not be a duplicate
        assert!(!dedup.is_duplicate("chan1", "user1", "hello", "req-1"));

        // Same (channel, user, content) AND same request_id within the
        // 30s window = a true replay = duplicate.
        assert!(dedup.is_duplicate("chan1", "user1", "hello", "req-1"));

        // Different user same content should not be duplicate
        assert!(!dedup.is_duplicate("chan1", "user2", "hello", "req-2"));

        // Different content same user should not be duplicate
        assert!(!dedup.is_duplicate("chan1", "user1", "world", "req-3"));

        // Regression: identical text but a *different* request id must
        // NOT be dropped. (The previous version dropped this and
        // caused the bus client to replay the previous Hydra response
        // when the user sent a second turn with new text — because the
        // dedup was the only place that could silently drop a frame.)
        assert!(!dedup.is_duplicate("chan1", "user1", "hello", "req-4"));
    }
}

