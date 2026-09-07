//! A durable, sequential two-tool coding loop for the shared Platform worker.
#![doc = include_str!("../README.md")]

mod config;
mod driver;
mod environment;
mod model;
mod prompt;
mod sessions;
mod state;
mod tools;

pub use config::{Config, Environment, ExecutionTarget, ReasoningLevel, config_schema};
use execution_client::ExecutionClient;
use futures_util::future::BoxFuture;
use harness_runtime::{Execution, Harness};
use llm_client::LlmClient;
pub use model::supported_models;
use platform_runtime_client::Result;

pub const ID: &str = "unified-exec-only-harness";

/// Cheaply cloned clients are injected once by the worker. No per-harness server,
/// registration side effects, credential discovery, or database connection.
#[derive(Clone)]
pub struct UnifiedExecOnlyHarness {
    llm: LlmClient,
    execution: ExecutionClient,
}
impl UnifiedExecOnlyHarness {
    pub fn new(llm: LlmClient, execution: ExecutionClient) -> Self {
        Self { llm, execution }
    }
}
impl Harness for UnifiedExecOnlyHarness {
    fn run(&self, execution: Execution) -> BoxFuture<'static, Result<()>> {
        let harness = self.clone();
        Box::pin(async move { driver::run(harness, execution).await })
    }
}
