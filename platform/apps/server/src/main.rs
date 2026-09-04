use platform_server::{
    config::AppConfig,
    machines::ExecutionGatewayClient,
    projects::{self, ProjectDatabase, ProjectService},
    providers::{self, ChatGptLoginService, LlmGatewayClient},
    router,
};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = AppConfig::from_env()?;
    let bind_address = config.bind_address;
    let database = ProjectDatabase::connect(
        &config.database_url,
        config.database_min_connections,
        config.database_max_connections,
        config.database_acquire_timeout,
    )
    .await?;
    database.migrate().await?;
    let project_service = ProjectService::new(database.pool().clone());
    let execution_gateway = ExecutionGatewayClient::new(
        config.execution_gateway_url,
        &config.execution_gateway_token,
        config.execution_gateway_timeout,
    )?;
    let llm_gateway = LlmGatewayClient::new(
        config.llm_gateway_url,
        &config.llm_gateway_admin_token,
        config.llm_gateway_timeout,
    )?;
    let listener = TcpListener::bind(bind_address).await?;
    let login_service = ChatGptLoginService::new(llm_gateway.clone())?;
    let login_service = match TcpListener::bind(providers::CALLBACK_ADDRESS).await {
        Ok(callback_listener) => {
            let callback_app = providers::callback_router(login_service.clone());
            tokio::spawn(async move {
                if axum::serve(callback_listener, callback_app).await.is_err() {
                    tracing::error!("ChatGPT callback listener stopped");
                }
            });
            Some(login_service)
        }
        Err(_) => {
            tracing::warn!(
                "Port 1455 is unavailable; ChatGPT sign-in is disabled. API-key providers remain available. Free the port and restart the server to enable sign-in."
            );
            None
        }
    };
    info!(%bind_address, "platform server listening");
    axum::serve(
        listener,
        router(execution_gateway, llm_gateway)
            .merge(providers::login_router(login_service))
            .merge(projects::router(project_service)),
    )
    .await?;
    Ok(())
}
