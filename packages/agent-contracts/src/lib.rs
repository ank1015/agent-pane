//! Shared wire contracts for the Agent service and harness runtimes.
//!
//! [`harness_protocol`] defines the durable lifecycle protocol, while
//! [`RunEvent`] and [`HarnessRunEvent`] define the replayable public event log
//! and non-authoritative harness observations. Session transcript reads and
//! writes intentionally remain an HTTP API.

mod abort;
mod error;
mod harness;
pub mod harness_protocol;
mod message;
mod run;
mod run_event;
mod session;
mod validation;
mod wait;

pub use abort::*;
pub use error::*;
pub use harness::*;
pub use harness_protocol::*;
pub use message::*;
pub use run::*;
pub use run_event::*;
pub use session::*;
pub use wait::*;
