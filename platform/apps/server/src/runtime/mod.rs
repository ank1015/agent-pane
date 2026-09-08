//! Durable application and worker runtime operations.
mod admin;
mod admin_http;
mod background;
mod capabilities;
mod commits;
mod configuration;
mod coordination;
mod environments;
mod error;
mod http;
mod metrics;
mod model;
mod mutations;
mod project_harnesses;
mod queries;
mod receipts;
mod remote_operations;
mod run_outputs;
mod session_state;
mod stream;
mod waits;
mod work_available;
mod worker_http;
mod worker_model;
mod workers;

pub use admin_http::admin_router;
pub use error::RuntimeError;
pub use http::router;
pub use model::{AbortRun, CreateSession, ForkSession, PatchSession, StartRun, SubmitInput};
pub use worker_http::worker_router;

use sqlx::PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Clone)]
pub struct RuntimeService {
    pool: PgPool,
    signals: broadcast::Sender<Uuid>,
    environments: Option<crate::projects::environments::EnvironmentService>,
    providers: Option<crate::providers::ProviderService>,
    execution: Option<execution_client::ExecutionClient>,
    sites: Option<crate::sites::SitesClient>,
}

impl RuntimeService {
    pub fn new(pool: PgPool) -> Self {
        let (signals, _) = broadcast::channel(256);
        Self {
            pool,
            signals,
            environments: None,
            providers: None,
            execution: None,
            sites: None,
        }
    }

    pub fn with_environments(
        mut self,
        service: crate::projects::environments::EnvironmentService,
    ) -> Self {
        self.environments = Some(service);
        self
    }

    pub fn with_providers(mut self, service: crate::providers::ProviderService) -> Self {
        self.providers = Some(service);
        self
    }
    pub fn with_execution(mut self, client: execution_client::ExecutionClient) -> Self {
        self.execution = Some(client);
        self
    }

    pub fn with_sites(mut self, client: crate::sites::SitesClient) -> Self {
        self.sites = Some(client);
        self
    }

    fn notify(&self, run: Uuid) {
        let _ = self.signals.send(run);
    }
}

mod site_sdk;
mod sites_authoring;
pub use site_sdk::SiteScope;
