use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::{ExecutionError, ExecutionResult};

/// Process-local cancellation and deadline controls for one operation.
#[derive(Clone, Debug)]
pub struct OperationContext {
    cancellation: CancellationToken,
    deadline: Option<Instant>,
}

impl OperationContext {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cancellation: CancellationToken::new(),
            deadline: None,
        }
    }

    #[must_use]
    pub fn with_deadline(deadline: Instant) -> Self {
        Self {
            cancellation: CancellationToken::new(),
            deadline: Some(deadline),
        }
    }

    #[must_use]
    pub fn with_timeout(timeout: Duration) -> Self {
        Self::with_deadline(Instant::now() + timeout)
    }

    #[must_use]
    pub fn child(&self) -> Self {
        Self {
            cancellation: self.cancellation.child_token(),
            deadline: self.deadline,
        }
    }

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

    pub async fn cancelled(&self) {
        self.cancellation.cancelled().await;
    }

    /// Fails before an expensive phase when the operation can no longer proceed.
    pub fn checkpoint(&self) -> ExecutionResult<()> {
        if self.is_cancelled() {
            return Err(ExecutionError::cancelled());
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(ExecutionError::deadline_exceeded());
        }
        Ok(())
    }
}

impl Default for OperationContext {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::ExecutionErrorCode;

    use super::*;

    #[test]
    fn cancelled_context_fails_checkpoint() {
        let context = OperationContext::new();
        context.cancel();
        let error = context
            .checkpoint()
            .expect_err("cancelled context should fail");
        assert_eq!(error.code, ExecutionErrorCode::Cancelled);
    }

    #[test]
    fn expired_context_fails_checkpoint() {
        let context = OperationContext::with_timeout(Duration::ZERO);
        let error = context
            .checkpoint()
            .expect_err("expired context should fail");
        assert_eq!(error.code, ExecutionErrorCode::DeadlineExceeded);
    }
}
