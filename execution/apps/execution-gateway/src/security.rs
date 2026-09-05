use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Clone)]
pub struct SecurityControls {
    operation_permits: Arc<Semaphore>,
    authentication_failures: FixedWindowLimiter,
    machine_entry_attempts: FixedWindowLimiter,
}

impl SecurityControls {
    #[must_use]
    pub fn new(
        max_operations_in_flight: usize,
        max_authentication_failures_per_minute: u32,
        max_machine_entry_attempts_per_minute: u32,
    ) -> Self {
        Self {
            operation_permits: Arc::new(Semaphore::new(max_operations_in_flight)),
            authentication_failures: FixedWindowLimiter::per_minute(
                max_authentication_failures_per_minute,
            ),
            machine_entry_attempts: FixedWindowLimiter::per_minute(
                max_machine_entry_attempts_per_minute,
            ),
        }
    }

    pub fn try_operation(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.operation_permits).try_acquire_owned().ok()
    }

    pub fn allow_authentication_failure(&self, request: &Request<Body>) -> bool {
        self.authentication_failures.allow(client_ip(request))
    }

    pub fn allow_machine_entry(&self, request: &Request<Body>) -> bool {
        self.machine_entry_attempts.allow(client_ip(request))
    }
}

#[derive(Clone)]
struct FixedWindowLimiter {
    inner: Arc<Mutex<HashMap<Option<IpAddr>, Window>>>,
    maximum: u32,
    duration: Duration,
}

#[derive(Clone, Copy)]
struct Window {
    started_at: Instant,
    attempts: u32,
}

impl FixedWindowLimiter {
    fn per_minute(maximum: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            maximum,
            duration: Duration::from_secs(60),
        }
    }

    fn allow(&self, client: Option<IpAddr>) -> bool {
        let now = Instant::now();
        let mut windows = self.inner.lock().expect("request limiter poisoned");
        if windows.len() > 4_096 {
            let retention = self.duration.saturating_mul(2);
            windows.retain(|_, window| now.duration_since(window.started_at) < retention);
        }
        let window = windows.entry(client).or_insert(Window {
            started_at: now,
            attempts: 0,
        });
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

fn client_ip(request: &Request<Body>) -> Option<IpAddr> {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|value| value.0.ip());
    if peer.is_some_and(|address| address.is_loopback()) {
        return forwarded_client_ip(request.headers()).or(peer);
    }
    peer
}

fn forwarded_client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .and_then(|value| value.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_respects_its_fixed_window() {
        let limiter = FixedWindowLimiter::per_minute(2);
        let client = Some(IpAddr::from([127, 0, 0, 1]));
        assert!(limiter.allow(client));
        assert!(limiter.allow(client));
        assert!(!limiter.allow(client));
    }

    #[test]
    fn forwarded_addresses_are_only_used_for_loopback_proxies() {
        let mut request = Request::new(Body::empty());
        request
            .headers_mut()
            .insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 8080))));
        assert_eq!(client_ip(&request), Some(IpAddr::from([203, 0, 113, 7])));

        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 4], 8080))));
        assert_eq!(client_ip(&request), Some(IpAddr::from([192, 0, 2, 4])));
    }
}
