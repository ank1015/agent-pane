//! A durable, sequential four-tool coding loop for the shared Platform worker.
#![doc = include_str!("../README.md")]

mod config;
mod driver;
mod environment;
mod model;
mod prompt;
mod publisher;
mod sessions;
mod state;
mod tools;
pub use publisher::GcsImagePublisher;
use std::sync::Arc;
use tool_view_image::ImagePublisher;

pub use config::{Config, Environment, ExecutionTarget, ReasoningLevel, config_schema};
use execution_client::ExecutionClient;
use futures_util::future::BoxFuture;
use harness_runtime::{Execution, Harness};
use llm_client::LlmClient;
pub use model::supported_models;
use platform_runtime_client::Result;

pub const ID: &str = "basic-codex-tools-harness";

/// Cheaply cloned clients are injected once by the worker. No per-harness server,
/// registration side effects, credential discovery, or database connection.
#[derive(Clone)]
pub struct BasicCodexToolsHarness {
    llm: LlmClient,
    execution: ExecutionClient,
    publisher: Arc<dyn ImagePublisher>,
}
impl BasicCodexToolsHarness {
    pub fn new(
        llm: LlmClient,
        execution: ExecutionClient,
        publisher: Arc<dyn ImagePublisher>,
    ) -> Self {
        Self {
            llm,
            execution,
            publisher,
        }
    }
}
impl Harness for BasicCodexToolsHarness {
    fn run(&self, execution: Execution) -> BoxFuture<'static, Result<()>> {
        let harness = self.clone();
        Box::pin(async move { driver::run(harness, execution).await })
    }
}
