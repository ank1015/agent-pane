use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

/// Process-local controls that accompany, but are not serialized into, an operation.
///
/// Transport adapters create a context for each incoming operation. Backends
/// should check cancellation and deadlines before expensive phases and propagate
/// them to provider APIs when supported.
#[derive(Clone, Debug)]
pub struct OperationContext {
    cancellation: CancellationToken,
    deadline: Option<Instant>,
}

impl OperationContext {
    /// Creates an operation without a deadline.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cancellation: CancellationToken::new(),
            deadline: None,
        }
    }

    /// Creates an operation with an absolute process-local deadline.
    #[must_use]
    pub fn with_deadline(deadline: Instant) -> Self {
        Self {
            cancellation: CancellationToken::new(),
            deadline: Some(deadline),
        }
    }

    /// Creates an operation whose deadline is relative to now.
    #[must_use]
    pub fn with_timeout(timeout: Duration) -> Self {
        Self::with_deadline(Instant::now() + timeout)
    }

    /// Returns a child context cancelled when this context is cancelled.
    #[must_use]
    pub fn child(&self) -> Self {
        Self {
            cancellation: self.cancellation.child_token(),
            deadline: self.deadline,
        }
    }

    /// Requests cooperative cancellation.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    #[must_use]
    pub fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    /// Resolves when cooperative cancellation is requested.
    pub async fn cancelled(&self) {
        self.cancellation.cancelled().await;
    }
}

impl Default for OperationContext {
    fn default() -> Self {
        Self::new()
    }
}
