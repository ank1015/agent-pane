#![doc = include_str!("../README.md")]

mod client;
mod config;
mod error;
mod types;

pub use client::LlmClient;
pub use config::{ConfigError, LlmClientConfig, WaitOptions};
pub use error::{ClientError, ClientResult};
pub use types::{
    CompletionRequest, CompletionResponse, GatewayError, GatewayErrorKind, GatewayFailure,
    IdempotencyKey, InvalidIdempotencyKey, Run, RunState,
};
