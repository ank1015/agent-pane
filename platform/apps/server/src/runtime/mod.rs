//! Durable application and worker runtime operations.
mod admin;
mod admin_http;
mod background;
mod commits;
mod configuration;
mod coordination;
mod error;
mod http;
mod model;
mod mutations;
mod queries;
mod receipts;
mod stream;
mod waits;
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
}

impl RuntimeService {
    pub fn new(pool: PgPool) -> Self {
        let (signals, _) = broadcast::channel(256);
        Self { pool, signals }
    }

    fn notify(&self, run: Uuid) {
        let _ = self.signals.send(run);
    }
}
