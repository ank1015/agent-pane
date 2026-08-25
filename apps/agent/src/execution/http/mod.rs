mod aborts;
mod runs;
mod waits;
mod worker;

use axum::Router;

use crate::app::AppState;

pub fn router() -> Router<AppState> {
    runs::router()
        .merge(waits::router())
        .merge(aborts::router())
}

pub fn worker_router() -> Router<AppState> {
    worker::router()
}
