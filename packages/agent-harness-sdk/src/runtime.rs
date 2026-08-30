use agent_contracts::HarnessOperation;
use async_trait::async_trait;

use crate::ActiveTurn;

/// Harness-specific execution invoked by the common server runner.
///
/// Implementations own all harness behavior and lifecycle decisions. Returning
/// an error asks the server to NAK the broker delivery for redelivery.
#[async_trait]
pub trait HarnessRuntime: Send + Sync + 'static {
    type Error: std::error::Error + Send + Sync + 'static;

    async fn execute(&self, turn: &ActiveTurn) -> Result<TurnOutcome, Self::Error>;
}

#[derive(Clone, Debug, PartialEq)]
pub enum TurnOutcome {
    Command(HarnessOperation),
    Cancelled,
    Stale,
}
