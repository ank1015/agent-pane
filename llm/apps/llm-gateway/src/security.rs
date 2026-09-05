use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct AuthenticationFailureLimiter {
    inner: Arc<Mutex<Window>>,
    maximum: u32,
    duration: Duration,
}

#[derive(Clone, Copy)]
struct Window {
    started_at: Instant,
    attempts: u32,
}

impl AuthenticationFailureLimiter {
    #[must_use]
    pub fn per_minute(maximum: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Window {
                started_at: Instant::now(),
                attempts: 0,
            })),
            maximum,
            duration: Duration::from_secs(60),
        }
    }

    pub fn allow_failure(&self) -> bool {
        let now = Instant::now();
        let mut window = self.inner.lock().expect("authentication limiter poisoned");
        if now.duration_since(window.started_at) >= self.duration {
            *window = Window {
                started_at: now,
                attempts: 0,
            };
        }
        if window.attempts >= self.maximum {
            return false;
        }
        window.attempts = window.attempts.saturating_add(1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::AuthenticationFailureLimiter;

    #[test]
    fn limiter_only_allows_the_configured_number_of_failures() {
        let limiter = AuthenticationFailureLimiter::per_minute(2);
        assert!(limiter.allow_failure());
        assert!(limiter.allow_failure());
        assert!(!limiter.allow_failure());
    }
}
