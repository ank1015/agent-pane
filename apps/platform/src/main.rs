use std::error::Error;

use platform::{
    config::AppConfig,
    providers::chatgpt_oauth::{ChatGptLoginService, callback_router},
    router,
    upstream::llm_gateway::LlmGatewayClient,
};
use tokio::{net::TcpListener, signal};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::dotenv().ok();
    init_tracing();

    let config = AppConfig::from_env()?;
    let gateway = LlmGatewayClient::new(
        config.llm_gateway_url.clone(),
        &config.llm_gateway_admin_token,
        config.llm_gateway_timeout,
    )?;
    let chatgpt_login = ChatGptLoginService::new(
        gateway.clone(),
        config.chatgpt_oauth_client_id,
        config.chatgpt_oauth_issuer,
        config.chatgpt_oauth_redirect_uri,
        config.dashboard_origin,
    );
    let listener = TcpListener::bind(config.bind_address).await?;
    let callback_listener = TcpListener::bind(config.chatgpt_oauth_callback_address).await?;

    tracing::info!(
        address = %config.bind_address,
        llm_gateway_url = %config.llm_gateway_url,
        "platform server listening"
    );
    tracing::info!(
        address = %config.chatgpt_oauth_callback_address,
        "ChatGPT OAuth callback listening"
    );

    let platform_server = axum::serve(
        listener,
        router(gateway, chatgpt_login.clone(), config.max_request_bytes),
    );
    let callback_server = axum::serve(callback_listener, callback_router(chatgpt_login));
    tokio::select! {
        result = platform_server => result?,
        result = callback_server => result?,
        () = shutdown_signal() => {}
    }

    Ok(())
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("platform=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn shutdown_signal() {
    match signal::ctrl_c().await {
        Ok(()) => tracing::info!("shutdown signal received"),
        Err(error) => tracing::error!(%error, "could not listen for shutdown signal"),
    }
}
