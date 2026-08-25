//! Serializable contracts for controlling local and remote execution machines.
//!
//! This crate intentionally contains no transport, persistence, or operating-system
//! implementation. The same types can be used by machine daemons, cloud gateways,
//! local runtimes, connector adapters, and clients.

pub mod artifacts;
pub mod capabilities;
pub mod common;
pub mod error;
pub mod filesystem;
pub mod identifiers;
pub mod machine;
pub mod path;
pub mod process;
pub mod validation;
pub mod workspace;

pub use artifacts::*;
pub use capabilities::*;
pub use common::*;
pub use error::*;
pub use filesystem::*;
pub use identifiers::*;
pub use machine::*;
pub use path::*;
pub use process::*;
pub use validation::{
    ContractError, Validate, ValidationError, ValidationIssue, from_json_value, parse_json,
};
pub use workspace::*;
