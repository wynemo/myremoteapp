use std::time::{Duration, Instant};

// Stay below the session/client's ten-second frame timeout. A persistent capture
// failure must still terminate with an error rather than freezing indefinitely.
const GRACE: Duration = Duration::from_secs(5);

pub struct DesktopRecovery {
    enabled: bool,
    since: Option<Instant>,
}
impl DesktopRecovery {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            since: None,
        }
    }
    pub fn retry(&mut self, now: Instant) -> bool {
        self.enabled && now.duration_since(*self.since.get_or_insert(now)) < GRACE
    }
    pub fn reset(&mut self) {
        self.since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transient_failure_recovers_but_persistent_failure_expires() {
        let now = Instant::now();
        let mut recovery = DesktopRecovery::new(true);
        assert!(recovery.retry(now));
        assert!(recovery.retry(now + GRACE - Duration::from_millis(1)));
        assert!(!recovery.retry(now + GRACE));
        assert!(!recovery.retry(now + GRACE * 2));
        recovery.reset();
        assert!(recovery.retry(now + GRACE * 3));
        assert!(!recovery.retry(now + GRACE * 4));
    }
    #[test]
    fn ordinary_host_does_not_enable_secure_desktop_recovery() {
        assert!(!DesktopRecovery::new(false).retry(Instant::now()));
    }
}
