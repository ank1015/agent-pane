//! A durable environment-building loop for the shared Platform worker.
#![doc = include_str!("../README.md")]

mod config;
mod driver;
mod prompt;
mod state;
mod tools;

pub use config::{Config, ReasoningLevel, config_schema};
use execution_client::ExecutionClient;
use futures_util::future::BoxFuture;
use harness_runtime::{Execution, Harness};
use llm_client::LlmClient;
use platform_runtime_client::Result;
pub use tools::{WebTools, definitions as tool_definitions};

pub const ID: &str = "environments";

/// Cheaply cloned clients are injected once by the worker. No per-harness server,
/// registration side effects, credential discovery, or database connection.
#[derive(Clone)]
pub struct EnvironmentsHarness {
    llm: LlmClient,
    execution: ExecutionClient,
    web: Option<WebTools>,
}
impl EnvironmentsHarness {
    pub fn new(llm: LlmClient, execution: ExecutionClient, web: Option<WebTools>) -> Self {
        Self {
            llm,
            execution,
            web,
        }
    }
}
impl Harness for EnvironmentsHarness {
    fn run(&self, execution: Execution) -> BoxFuture<'static, Result<()>> {
        let harness = self.clone();
        Box::pin(async move { driver::run(harness, execution).await })
    }
}
