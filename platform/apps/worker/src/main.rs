use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use platform_runtime_client::{ClientConfig, PlatformClient};
use platform_worker::{Registry, Settings, Snapshot, Supervisor};
use rand::RngCore;
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::{RwLock, watch};
use tracing_subscriber::EnvFilter;

type Health = Arc<RwLock<Snapshot>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let url = std::env::var("PLATFORM_URL").unwrap_or_else(|_| "http://127.0.0.1:3100".into());
    let bootstrap = zeroize::Zeroizing::new(
        std::env::var("PLATFORM_WORKER_REGISTRATION_TOKEN")
            .map_err(|_| "PLATFORM_WORKER_REGISTRATION_TOKEN is required")?,
    );
    let bind: SocketAddr = std::env::var("WORKER_BIND_ADDRESS")
        .unwrap_or_else(|_| "127.0.0.1:3101".into())
        .parse()?;
    let mut settings = Settings::default();
    if let Ok(capacity) = std::env::var("WORKER_CAPACITY") {
        settings.capacity = capacity.parse()?;
    }
    if let Ok(build) = std::env::var("WORKER_BUILD_ID") {
        settings.build_id = build;
    }
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let client = PlatformClient::new(&url, uuid::Uuid::new_v4(), token, ClientConfig::default())?;
    // No production harnesses. Adding one means compiling/registering a library,
    // not starting another independently deployed HTTP server.
    let supervisor = Supervisor::new(client, Registry::default(), settings)?;
    let health = supervisor.snapshot();
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let app = Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/readyz", get(ready))
        .route("/status", get(status))
        .with_state(health);
    let (stop, stopped) = watch::channel(false);
    let signal = tokio::spawn(async move {
        shutdown_signal().await;
        let _ = stop.send(true);
    });
    let http = tokio::spawn(async move { axum::serve(listener, app).await });
    tracing::info!(%bind, "worker health listener started");
    let result = supervisor.run(bootstrap, stopped).await;
    signal.abort();
    http.abort();
    let _ = tokio::join!(signal, http);
    result?;
    Ok(())
}
async fn ready(State(health): State<Health>) -> StatusCode {
    let state = health.read().await;
    if state.registered && state.heartbeat_ok && !state.draining {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
async fn status(State(health): State<Health>) -> Json<Snapshot> {
    Json(health.read().await.clone())
}
async fn shutdown_signal() {
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate => {} }
}
