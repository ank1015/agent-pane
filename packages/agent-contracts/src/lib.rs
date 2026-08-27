//! Shared wire contracts for the Agent service and harness runtimes.
//!
//! [`harness_protocol`] defines the durable broker protocol. Session transcript
//! reads and writes intentionally remain an HTTP API and have lease-free
//! request contracts in that module.

mod abort;
mod error;
mod harness;
pub mod harness_protocol;
mod message;
mod run;
mod session;
mod validation;
mod wait;

pub use abort::*;
pub use error::*;
pub use harness::*;
pub use harness_protocol::*;
pub use message::*;
pub use run::*;
pub use session::*;
pub use wait::*;
