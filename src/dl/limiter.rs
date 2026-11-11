//! A shared download speed limit: a token bucket that every HTTP connection draws from.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub struct Limiter {
    bps: AtomicU64,
    state: tokio::sync::Mutex<(f64, Instant)>, // tokens (may go negative: debt), last refill
}

impl Limiter {
    pub fn new(bps: u64) -> Self {
        Self { bps: AtomicU64::new(bps), state: tokio::sync::Mutex::new((0.0, Instant::now())) }
    }

    pub fn set(&self, bps: u64) {
        self.bps.store(bps, Ordering::Relaxed);
    }

    /// Wait until `n` bytes may be taken. Free when there's no limit.
    pub async fn acquire(&self, n: usize) {
        let bps = self.bps.load(Ordering::Relaxed);
        if bps == 0 {
            return;
        }
        let wait = {
            let mut s = self.state.lock().await;
            let now = Instant::now();
            let rate = bps as f64;
            // at most a quarter second of burst
            s.0 = (s.0 + now.duration_since(s.1).as_secs_f64() * rate).min(rate / 4.0);
            s.1 = now;
            s.0 -= n as f64;
            if s.0 < 0.0 { Duration::from_secs_f64(-s.0 / rate) } else { Duration::ZERO }
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}
