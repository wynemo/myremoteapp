use std::time::{Duration, Instant};

/// Require a healthy capture across the desktop's unlock transition, rather
/// than declaring recovery complete as soon as start_capture returns.
#[derive(Default)]
pub struct CaptureReadiness {
    healthy_since: Option<Instant>,
}

impl CaptureReadiness {
    pub fn reset(&mut self) {
        self.healthy_since = None;
    }

    pub fn observe(&mut self, healthy: bool, now: Instant) -> bool {
        if !healthy {
            self.reset();
            return false;
        }
        let since = *self.healthy_since.get_or_insert(now);
        now.duration_since(since) >= Duration::from_secs(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlock_transition_and_relock_require_a_new_healthy_interval() {
        let mut readiness = CaptureReadiness::default();
        let start = Instant::now();
        assert!(!readiness.observe(false, start)); // Stream started, no frame.
        assert!(!readiness.observe(true, start + Duration::from_millis(100)));
        assert!(!readiness.observe(false, start + Duration::from_millis(900))); // Relock.
        assert!(!readiness.observe(true, start + Duration::from_secs(1)));
        assert!(!readiness.observe(true, start + Duration::from_millis(1900)));
        assert!(readiness.observe(true, start + Duration::from_secs(2)));
        readiness.reset(); // A replacement capture cannot inherit readiness.
        assert!(!readiness.observe(true, start + Duration::from_secs(3)));
    }
}
