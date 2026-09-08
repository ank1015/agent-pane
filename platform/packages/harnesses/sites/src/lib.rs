//! Session-bound live Sites authoring and project orchestration.
#![doc = include_str!("../README.md")]
mod browser;
mod config;
mod driver;
mod model;
mod prompt;
mod state;
mod tools;
pub use browser::Browser;
pub use config::{Config, config_schema};
use futures_util::future::BoxFuture;
use harness_runtime::{Execution, Harness};
use llm_client::LlmClient;
pub use model::{ReasoningLevel, supported_models};
use platform_agent_code_mode::AgentCodeMode;
use platform_runtime_client::{Error, Result, RunClient};
use std::path::PathBuf;
pub use tools::WebTools;
pub const ID: &str = "sites";

#[derive(Clone)]
pub struct SitesHarness {
    llm: LlmClient,
    guest: PathBuf,
    web: Option<WebTools>,
    browser: Option<Browser>,
}
impl SitesHarness {
    pub fn new(
        llm: LlmClient,
        guest: PathBuf,
        web: Option<WebTools>,
        browser: Option<Browser>,
    ) -> Self {
        Self {
            llm,
            guest,
            web,
            browser,
        }
    }
    fn code_mode(&self, client: RunClient) -> Result<AgentCodeMode> {
        let mut mode = AgentCodeMode::new(client, &self.guest)
            .and_then(AgentCodeMode::with_sites)
            .map_err(|_| Error::Invalid("invalid Sites code mode"))?;
        tools::register(
            &mut mode.registry,
            self.web.is_some(),
            self.browser.is_some(),
        )
        .map_err(|_| Error::Invalid("invalid Sites tool registry"))?;
        Ok(mode)
    }
}
impl Harness for SitesHarness {
    fn run(&self, execution: Execution) -> BoxFuture<'static, Result<()>> {
        let harness = self.clone();
        Box::pin(async move { driver::run(harness, execution).await })
    }
}

#[cfg(test)]
mod tests;
