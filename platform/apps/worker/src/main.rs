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
    let mut registry = Registry::default();
    let exec_only_enabled = std::env::var("UNIFIED_EXEC_ONLY_ENABLED").as_deref() == Ok("true");
    let codex_enabled = std::env::var("BASIC_CODEX_TOOLS_ENABLED").as_deref() == Ok("true");
    let basic_enabled = std::env::var("BASIC_CC_TOOLS_ENABLED").as_deref() == Ok("true");
    let environments_enabled = std::env::var("ENVIRONMENTS_ENABLED").as_deref() == Ok("true");
    if basic_enabled || codex_enabled || exec_only_enabled || environments_enabled {
        let mut llm_config = llm_client::LlmClientConfig::new(
            std::env::var("LLM_GATEWAY_URL")?.parse()?,
            std::env::var("LLM_GATEWAY_API_TOKEN")?,
        );
        let mut execution_config = execution_client::ExecutionClientConfig::new(
            std::env::var("EXECUTION_GATEWAY_URL")?.parse()?,
            std::env::var("EXECUTION_GATEWAY_API_TOKEN")?,
        );
        let insecure = std::env::var("GATEWAYS_ALLOW_INSECURE_HTTP").as_deref() == Ok("true");
        llm_config.allow_insecure_http = insecure;
        execution_config.allow_insecure_http = insecure;
        let llm = llm_client::LlmClient::new(llm_config)?;
        let execution = execution_client::ExecutionClient::new(execution_config)?;
        if exec_only_enabled {
            registry.register(
                unified_exec_only_harness::ID,
                Arc::new(unified_exec_only_harness::UnifiedExecOnlyHarness::new(
                    llm.clone(),
                    execution.clone(),
                )),
            )?;
        }
        if codex_enabled {
            let publisher = basic_codex_tools_harness::GcsImagePublisher::new(std::env::var(
                "BASIC_CODEX_IMAGE_BUCKET",
            )?)?;
            registry.register(
                basic_codex_tools_harness::ID,
                Arc::new(basic_codex_tools_harness::BasicCodexToolsHarness::new(
                    llm.clone(),
                    execution.clone(),
                    Arc::new(publisher),
                )),
            )?;
        }
        if basic_enabled {
            registry.register(
                basic_cc_tools_harness::ID,
                Arc::new(basic_cc_tools_harness::BasicCcToolsHarness::new(
                    llm.clone(),
                    execution.clone(),
                )),
            )?;
        }
        if environments_enabled {
            let web = match std::env::var("FIRECRAWL_API_KEY") {
                Ok(key) if key.is_empty() => None,
                Ok(key) => Some(environments_harness::WebTools {
                    search: tool_firecrawl_search::FirecrawlSearchToolContext::new(key.clone())?,
                    scrape: tool_firecrawl_scrape::FirecrawlScrapeToolContext::new(key)?,
                }),
                Err(std::env::VarError::NotPresent) => None,
                Err(error) => return Err(error.into()),
            };
            registry.register(
                environments_harness::ID,
                Arc::new(environments_harness::EnvironmentsHarness::new(
                    llm, execution, web,
                )),
            )?;
        }
    }
    let supervisor = Supervisor::new(client, registry, settings)?;
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
