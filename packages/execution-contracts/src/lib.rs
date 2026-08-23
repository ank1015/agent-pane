//! Serializable contracts for controlling local and remote execution environments.
//!
//! This crate intentionally contains no transport, persistence, or operating-system
//! implementation. The same types can be used by machine daemons, cloud gateways,
//! local runtimes, connector adapters, and clients.

pub mod artifacts;
pub mod capabilities;
pub mod common;
pub mod environment;
pub mod error;
pub mod filesystem;
pub mod identifiers;
pub mod path;
pub mod process;
pub mod validation;
pub mod workspace;

pub use artifacts::*;
pub use capabilities::*;
pub use common::*;
pub use environment::*;
pub use error::*;
pub use filesystem::*;
pub use identifiers::*;
pub use path::*;
pub use process::*;
pub use validation::{
    ContractError, Validate, ValidationError, ValidationIssue, from_json_value, parse_json,
};
pub use workspace::*;
