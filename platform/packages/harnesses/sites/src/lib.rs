//! Session-bound live site authoring.
#![doc = include_str!("../README.md")]
mod browser;
mod config;
mod driver;
mod model;
mod prompt;
mod state;
mod tools;
pub use browser::BrowserConfig;
pub use config::{Config, config_schema};
use futures_util::future::BoxFuture;
use harness_runtime::{Execution, Harness};
use llm_client::LlmClient;
pub use model::{ReasoningLevel, supported_models};
use platform_runtime_client::{Error, Result, RunClient};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tool_code_mode::{
    Registry,
    live::{Notification, Session},
};
pub use tools::WebTools;
pub const ID: &str = "sites";

#[derive(Clone)]
pub struct SitesHarness {
    llm: LlmClient,
    guest: PathBuf,
    browser: BrowserConfig,
    web: WebTools,
}
impl SitesHarness {
    pub fn new(llm: LlmClient, guest: PathBuf, browser: BrowserConfig, web: WebTools) -> Self {
        Self {
            llm,
            guest,
            browser,
            web,
        }
    }
    fn code_mode(
        &self,
        client: RunClient,
        site_id: Arc<Mutex<Option<uuid::Uuid>>>,
    ) -> Result<(Session, tokio::sync::mpsc::Receiver<Notification>)> {
        let mut registry = Registry::default();
        tools::register(&mut registry)
            .map_err(|_| Error::Invalid("invalid Sites tool registry"))?;
        Session::new(
            self.guest.clone(),
            registry,
            Arc::new(tools::SitesDispatcher {
                client,
                site_id,
                browser: browser::BrowserSession::new(self.browser.clone()),
                web: self.web.clone(),
            }),
            vec![],
        )
        .map_err(|_| Error::Invalid("invalid live code-mode registry"))
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
