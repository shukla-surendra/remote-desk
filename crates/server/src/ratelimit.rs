use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

/// Sliding-window counter of failed logins per client IP.
/// Phase 1 is single-instance; this moves to Redis when the control plane scales out.
pub struct FailureLimiter {
    max: usize,
    window: Duration,
    hits: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

impl FailureLimiter {
    pub fn new(max: usize, window: Duration) -> Self {
        Self {
            max,
            window,
            hits: Mutex::new(HashMap::new()),
        }
    }

    pub fn is_limited(&self, ip: IpAddr) -> bool {
        let mut hits = self.hits.lock().unwrap();
        let now = Instant::now();
        let limited = match hits.get_mut(&ip) {
            Some(v) => {
                v.retain(|t| now.duration_since(*t) < self.window);
                v.len() >= self.max
            }
            None => false,
        };
        // Opportunistic cleanup so the map can't grow without bound.
        if hits.len() > 10_000 {
            hits.retain(|_, v| v.iter().any(|t| now.duration_since(*t) < self.window));
        }
        limited
    }

    pub fn record_failure(&self, ip: IpAddr) {
        self.hits
            .lock()
            .unwrap()
            .entry(ip)
            .or_default()
            .push(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_after_max_failures() {
        let l = FailureLimiter::new(2, Duration::from_secs(60));
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(!l.is_limited(ip));
        l.record_failure(ip);
        l.record_failure(ip);
        assert!(l.is_limited(ip));
        assert!(!l.is_limited("10.0.0.2".parse().unwrap()));
    }
}
