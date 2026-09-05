use std::{fs::File, process::Stdio, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use execution_api::{ExecutionHost, ExecutionHostState, RegisteredHostRegistration};
use execution_conformance::{ConformanceConfig, run_all};
use execution_core::{ExecutionRuntime as _, OperationContext};
use execution_gateway::{
    AppState, CredentialVault, Database, DynE2bProvider, E2bProviderSettings, HostConnections,
    LifecycleReconciler, RealE2bProvider, SecurityControls, router,
};
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde_json::json;
use sqlx::PgPool;
use tokio::{net::TcpListener, process::Command, task::JoinHandle};
use url::Url;
use uuid::Uuid;

use execution_client::{ExecutionClient, ExecutionClientConfig};

const API_TOKEN: &str = "production-daemon-e2e-token";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires EXECUTION_GATEWAY_TEST_DATABASE_URL pointing to an isolated PostgreSQL database"]
async fn production_daemon_routes_every_gateway_operation() -> anyhow::Result<()> {
    let database_url = std::env::var("EXECUTION_GATEWAY_TEST_DATABASE_URL")?;
    let database = Database::connect(&database_url, 5).await?;
    database.migrate().await?;
    let inspection = PgPool::connect(&database_url).await?;
    sqlx::query(
        "truncate table idempotency_records, registered_hosts, e2b_hosts, e2b_snapshots,
                        execution_hosts, e2b_accounts cascade",
    )
    .execute(&inspection)
    .await?;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let gateway_url = format!("http://{address}");
    let api_url = format!("{gateway_url}/v1");
    let provider: DynE2bProvider = Arc::new(RealE2bProvider::new(E2bProviderSettings {
        control_base_url: Url::parse("https://api.e2b.app/")?,
        envd_base_url_override: None,
        request_timeout: Duration::from_secs(1),
    }));
    let vault = CredentialVault::from_base64(&STANDARD.encode([29_u8; 32]))?;
    let reconciler = LifecycleReconciler::new(database.clone(), vault.clone(), provider.clone());
    let app = router(
        AppState {
            database,
            vault,
            provider,
            reconciler,
            api_token: Arc::from(API_TOKEN),
            default_host_timeout_seconds: 300,
            public_base_url: format!("{gateway_url}/").parse()?,
            registration_token_ttl: Duration::from_secs(900),
            heartbeat_interval: Duration::from_millis(100),
            connections: HostConnections::new(Duration::from_secs(30), 64),
            security: SecurityControls::new(32, 30, 60),
        },
        16 * 1024 * 1024,
    );
    let gateway = AbortOnDrop(tokio::spawn(
        async move { axum::serve(listener, app).await },
    ));
    let client = Client::new();

    let registration: RegisteredHostRegistration = decode(
        client
            .post(format!("{api_url}/registered-hosts"))
            .bearer_auth(API_TOKEN)
            .header("Idempotency-Key", "production-daemon-e2e")
            .json(&json!({"name":"production-daemon-e2e"}))
            .send()
            .await?,
    )
    .await?;

    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    let state = temporary.path().join("state");
    tokio::fs::create_dir_all(&workspace).await?;
    let config_path = temporary.path().join("execution-host.json");
    tokio::fs::write(
        &config_path,
        serde_json::to_vec_pretty(&json!({
            "gateway": gateway_url,
            "allow_insecure_http": true,
            "state_directory": state,
            "roots": [{
                "id": "workspace",
                "name": "Workspace",
                "path": workspace,
                "read_only": false
            }]
        }))?,
    )
    .await?;

    let binary = env!("CARGO_BIN_EXE_execution-host");
    let registered = Command::new(binary)
        .arg("--config")
        .arg(&config_path)
        .arg("register")
        .env(
            "EXECUTION_HOST_REGISTRATION_TOKEN",
            &registration.registration_token,
        )
        .output()
        .await?;
    anyhow::ensure!(
        registered.status.success(),
        "production daemon registration failed: stdout={} stderr={}",
        String::from_utf8_lossy(&registered.stdout),
        String::from_utf8_lossy(&registered.stderr)
    );

    let daemon_log = temporary.path().join("daemon.log");
    let log = File::create(&daemon_log)?;
    let mut daemon = Command::new(binary);
    daemon
        .arg("--config")
        .arg(&config_path)
        .arg("connect")
        .env("RUST_LOG", "debug")
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .kill_on_drop(true);
    let mut daemon = daemon.spawn()?;

    let ready = wait_for_host(
        &client,
        &api_url,
        registration.host.id,
        ExecutionHostState::Ready,
        &mut daemon,
        &daemon_log,
    )
    .await?;
    let mut config = ExecutionClientConfig::new(gateway_url.parse()?, API_TOKEN);
    config.allow_insecure_http = true;
    let hosted = ExecutionClient::new(config)?
        .connect_host(
            &OperationContext::with_timeout(Duration::from_secs(20)),
            ready.id.to_string().parse()?,
        )
        .await?;
    anyhow::ensure!(
        Some(hosted.descriptor()) == ready.descriptor.as_ref(),
        "live describe differs from the descriptor registered by the production daemon"
    );

    let conformance =
        ConformanceConfig::for_runtime(&hosted)?.with_operation_timeout(Duration::from_secs(30));
    let report = run_all(&hosted, &conformance).await?;
    anyhow::ensure!(
        report.skipped_checks().is_empty(),
        "production daemon skipped conformance checks: {:?}",
        report.skipped_checks()
    );
    anyhow::ensure!(
        report.passed_checks().contains(&"process_signal"),
        "the process_signal operation was not exercised"
    );

    let deleted: ExecutionHost = decode(
        client
            .delete(format!("{api_url}/hosts/{}", registration.host.id))
            .bearer_auth(API_TOKEN)
            .send()
            .await?,
    )
    .await?;
    anyhow::ensure!(deleted.state == ExecutionHostState::Deleted);
    let exit = tokio::time::timeout(Duration::from_secs(5), daemon.wait())
        .await
        .map_err(|_| anyhow::anyhow!("production daemon did not exit after host deletion"))??;
    anyhow::ensure!(
        exit.success(),
        "production daemon exited unsuccessfully after host deletion: {exit}; log={}",
        read_log(&daemon_log).await
    );

    drop(gateway);
    Ok(())
}

struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn wait_for_host(
    client: &Client,
    api_url: &str,
    host_id: Uuid,
    expected: ExecutionHostState,
    daemon: &mut tokio::process::Child,
    daemon_log: &std::path::Path,
) -> anyhow::Result<ExecutionHost> {
    for _ in 0..200 {
        if let Some(status) = daemon.try_wait()? {
            anyhow::bail!(
                "production daemon exited before host reached {expected:?}: {status}; log={}",
                read_log(daemon_log).await
            );
        }
        let host: ExecutionHost = decode(
            client
                .get(format!("{api_url}/hosts/{host_id}"))
                .bearer_auth(API_TOKEN)
                .send()
                .await?,
        )
        .await?;
        if host.state == expected {
            return Ok(host);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!(
        "host {host_id} did not reach {expected:?}; daemon log={}",
        read_log(daemon_log).await
    )
}

async fn read_log(path: &std::path::Path) -> String {
    tokio::fs::read_to_string(path)
        .await
        .unwrap_or_else(|error| format!("<failed to read daemon log: {error}>"))
}

async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> anyhow::Result<T> {
    let status = response.status();
    let bytes = response.bytes().await?;
    anyhow::ensure!(
        status.is_success(),
        "gateway returned {status}: {}",
        String::from_utf8_lossy(&bytes)
    );
    Ok(serde_json::from_slice(&bytes)?)
}
