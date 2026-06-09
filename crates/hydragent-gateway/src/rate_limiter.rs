use std::sync::Mutex;
use std::time::Instant;

pub struct RateLimiter {
    capacity: f64,
    tokens_per_sec: f64,
    tokens: Mutex<f64>,
    last_update: Mutex<Instant>,
}

impl RateLimiter {
    pub fn new(capacity: f64, tokens_per_sec: f64) -> Self {
        Self {
            capacity,
            tokens_per_sec,
            tokens: Mutex::new(capacity),
            last_update: Mutex::new(Instant::now()),
        }
    }

    pub fn default_for_channel(channel_id: &str) -> Self {
        if channel_id.starts_with("telegram") {
            Self::new(30.0, 30.0)
        } else if channel_id.starts_with("discord") {
            Self::new(5.0, 5.0)
        } else {
            Self::new(10.0, 10.0)
        }
    }

    pub fn try_acquire(&self) -> bool {
        let mut tokens = self.tokens.lock().unwrap();
        let mut last_update = self.last_update.lock().unwrap();
        let now = Instant::now();

        let elapsed = now.duration_since(*last_update).as_secs_f64();
        *last_update = now;

        let added_tokens = elapsed * self.tokens_per_sec;
        *tokens = (*tokens + added_tokens).min(self.capacity);

        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiter() {
        // Capacity = 2, refill rate = 10 tokens/sec (very fast)
        let limiter = RateLimiter::new(2.0, 10.0);

        // We can acquire 2 tokens immediately
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());

        // Third token immediately should fail
        assert!(!limiter.try_acquire());

        // Wait a short time (e.g. 110ms) to allow at least 1 token to refill
        std::thread::sleep(std::time::Duration::from_millis(110));

        // Refill should allow us to acquire again
        assert!(limiter.try_acquire());
    }
}

