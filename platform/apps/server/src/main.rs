use platform_server::{
    config::AppConfig,
    machines::ExecutionGatewayClient,
    projects::{self, ProjectDatabase, ProjectService},
    providers::{self, ChatGptLoginService, LlmGatewayClient},
    router, runtime,
};
use std::{future::IntoFuture, time::Duration};
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
    let environment_service = projects::environments::EnvironmentService::new(
        database.pool().clone(),
        projects::environments::EnvironmentGateway::new(
            config.execution_gateway_url.clone(),
            &config.execution_gateway_token,
            config.execution_gateway_timeout,
        )?,
    );
    let llm_gateway = LlmGatewayClient::new(
        config.llm_gateway_url,
        &config.llm_gateway_admin_token,
        config.llm_gateway_timeout,
    )?;
    let mut remote_config = execution_client::ExecutionClientConfig::new(
        config.execution_gateway_url.clone(),
        config.execution_gateway_token.clone(),
    );
    // Use the server's existing operator-selected gateway URL policy, including
    // private development HTTP deployments supported by the gateway proxy.
    remote_config.allow_insecure_http = config.execution_gateway_url.scheme() == "http";
    remote_config.request_timeout = std::time::Duration::from_secs(10);
    remote_config.max_response_bytes = 1024 * 1024;
    let remote_client = execution_client::ExecutionClient::new(remote_config)?;
    let mut runtime_service = runtime::RuntimeService::new(database.pool().clone())
        .with_environments(environment_service.clone())
        .with_providers(providers::ProviderService::new(llm_gateway.clone()))
        .with_execution(remote_client);
    let registration_token = zeroize::Zeroizing::new(
        std::env::var("PLATFORM_WORKER_REGISTRATION_TOKEN").unwrap_or_default(),
    );
    if !registration_token.is_empty()
        && (!(32..=256).contains(&registration_token.len())
            || !registration_token.bytes().all(|b| (33..=126).contains(&b)))
    {
        return Err(
            "PLATFORM_WORKER_REGISTRATION_TOKEN must contain 32–256 printable ASCII characters without spaces".into(),
        );
    }
    if registration_token.is_empty() {
        tracing::warn!(
            "Worker registration is disabled: set PLATFORM_WORKER_REGISTRATION_TOKEN to enable it"
        );
    }
    let sites_url = std::env::var("PLATFORM_SITES_SERVICE_URL").unwrap_or_default();
    let sites_token =
        zeroize::Zeroizing::new(std::env::var("PLATFORM_SITES_SERVICE_TOKEN").unwrap_or_default());
    let capability_token = zeroize::Zeroizing::new(
        std::env::var("PLATFORM_SITES_CAPABILITY_TOKEN").unwrap_or_default(),
    );
    if !sites_url.is_empty() {
        runtime_service = runtime_service.with_sites(platform_server::sites::SitesClient::new(
            url::Url::parse(&sites_url)?,
            &sites_token,
        )?);
    }
    let worker_app = runtime::worker_router(runtime_service.clone(), registration_token.as_str());
    let admin_token =
        zeroize::Zeroizing::new(std::env::var("PLATFORM_RUNTIME_ADMIN_TOKEN").unwrap_or_default());
    if !admin_token.is_empty()
        && (!(32..=256).contains(&admin_token.len())
            || !admin_token.bytes().all(|b| (33..=126).contains(&b)))
    {
        return Err("PLATFORM_RUNTIME_ADMIN_TOKEN must contain 32–256 printable ASCII characters without spaces".into());
    }
    if admin_token.is_empty() {
        tracing::warn!(
            "Runtime administration is disabled: set PLATFORM_RUNTIME_ADMIN_TOKEN to enable it"
        );
    } else if admin_token.as_str() == registration_token.as_str() {
        return Err("Runtime admin and worker registration credentials must be different".into());
    }
    let admin_app = runtime::admin_router(runtime_service.clone(), admin_token.as_str());
    let reconciler = runtime_service.spawn_reconciler();
    let remote_reconciler = runtime_service.spawn_remote_reconciler();
    let notifications = runtime_service.spawn_notification_listener();
    let execution_gateway = ExecutionGatewayClient::new(
        config.execution_gateway_url,
        &config.execution_gateway_token,
        config.execution_gateway_timeout,
    )?;
    let listener = TcpListener::bind(bind_address).await?;
    let bootstrap_app =
        projects::bootstrap::router(projects::bootstrap::ProjectBootstrapService::new(
            database.pool().clone(),
            providers::ProviderService::new(llm_gateway.clone()),
            environment_service.clone(),
        ));
    let sites = if sites_url.is_empty() && sites_token.is_empty() && capability_token.is_empty() {
        None
    } else {
        if sites_token.as_str() == capability_token.as_str()
            || sites_token.as_str() == admin_token.as_str()
            || sites_token.as_str() == registration_token.as_str()
            || capability_token.as_str() == registration_token.as_str()
        {
            return Err("Sites service credentials must be distinct from each other and other service roles".into());
        }
        Some(platform_server::sites::SitesService::new(
            database.pool().clone(),
            platform_server::sites::SitesClient::new(url::Url::parse(&sites_url)?, &sites_token)?,
            runtime_service.clone(),
            providers::ProviderService::new(llm_gateway.clone()),
            &capability_token,
            &admin_token,
        )?)
    };
    let sites_reconciler = sites.as_ref().map(|s| s.spawn_reconciler());
    let site_callbacks = sites.as_ref().map(|s| s.spawn_callback_dispatcher());
    let sites_app = sites
        .map(platform_server::sites::router)
        .unwrap_or_default();
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
    let (stopping, stopped) = tokio::sync::oneshot::channel();
    let server = axum::serve(
        listener,
        router(execution_gateway, llm_gateway)
            .merge(providers::login_router(login_service))
            .merge(projects::router(project_service))
            .merge(bootstrap_app)
            .merge(sites_app)
            .merge(runtime::router(runtime_service))
            .merge(worker_app)
            .merge(admin_app)
            .merge(projects::environments::router(environment_service)),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        let _ = stopping.send(());
    })
    .into_future();
    tokio::pin!(server);
    let result = tokio::select! {
        result = &mut server => result,
        _ = stopped => {
            // Long-lived SSE clients must not hold a deployment open forever.
            match tokio::time::timeout(Duration::from_secs(10), &mut server).await {
                Ok(result) => result,
                Err(_) => {
                    tracing::info!("HTTP shutdown grace period elapsed");
                    Ok(())
                }
            }
        }
    };
    if let Some(task) = sites_reconciler {
        task.abort();
        let _ = task.await;
    }
    if let Some(task) = site_callbacks {
        task.abort();
        let _ = task.await;
    }
    reconciler.abort();
    remote_reconciler.abort();
    notifications.abort();
    let _ = tokio::join!(reconciler, notifications, remote_reconciler);
    result?;
    Ok(())
}

async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = interrupt => {}, _ = terminate => {} }
}
