use platform_sites_service::{
    SiteService,
    config::Config,
    content::{self, ContentHost},
    router_with_content,
};
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("--backend-runtime") {
        return platform_sites_service::runtime::guest_main();
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve())
}

async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = Config::from_env()?;
    let service = SiteService::open_with_platform(
        config.data_dir,
        platform_sites_service::platform::PlatformClient::from_env()?,
    )
    .await?;
    let content_host = config
        .content
        .map(|settings| ContentHost::new(settings, &config.api_token))
        .transpose()?;
    let content_listener = match &content_host {
        Some(host) => Some(tokio::net::TcpListener::bind(host.config.bind_address).await?),
        None => None,
    };
    let app = router_with_content(service.clone(), &config.api_token, content_host.clone())?;
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    let (stop, stopped) = watch::channel(false);
    let reconciler_service = service.clone();
    let reconciler = tokio::spawn(async move { reconciler_service.run_reconciler(stopped).await });
    tracing::info!(bind = %listener.local_addr()?, "sites service listening");
    let mut servers = tokio::task::JoinSet::new();
    let shutdown = stop.subscribe();
    servers.spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(wait_shutdown(shutdown))
            .await
    });
    if let (Some(listener), Some(host)) = (content_listener, content_host) {
        tracing::info!(bind = %listener.local_addr()?, origin = %host.config.public_origin, "site content listener started");
        let app = content::router(service.clone(), host);
        let shutdown = stop.subscribe();
        servers.spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(wait_shutdown(shutdown))
                .await
        });
    }
    let first = tokio::select! {
        _ = shutdown_signal() => None,
        completed = servers.join_next() => completed,
    };
    let _ = stop.send(true);
    let mut server_error = first.and_then(|result| match result {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(error) => Some(error.to_string()),
    });
    while let Some(result) = servers.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => server_error = Some(error.to_string()),
            Err(error) => server_error = Some(error.to_string()),
        }
    }
    reconciler.await?;
    service.close().await;
    if let Some(error) = server_error {
        return Err(error.into());
    }
    Ok(())
}

async fn wait_shutdown(mut shutdown: watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
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
