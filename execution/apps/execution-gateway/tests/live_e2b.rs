use std::{sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use execution_api::{E2bAccount, E2bSnapshot, ExecutionHost, ExecutionHostState, SnapshotState};
use execution_client::{ExecutionClient, ExecutionClientConfig, GatewayHostRuntime};
use execution_conformance::{ConformanceConfig, run_all};
use execution_core::OperationContext;
use execution_gateway::{
    AppState, CredentialVault, Database, DynE2bProvider, E2bProviderSettings, HostConnections,
    LifecycleReconciler, RealE2bProvider, SecurityControls, router,
};
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::net::TcpListener;
use url::Url;
use uuid::Uuid;

const API_TOKEN: &str = "live-e2b-gateway-test-token";
const HOST_TIMEOUT_SECONDS: u64 = 3_600;

/// Runs the shared protocol conformance suite against an already-connected
/// Registered Host through the deployed gateway.
#[tokio::test]
#[ignore = "requires a deployed gateway, API token, and connected Registered Host"]
async fn real_registered_host_gateway_conformance() -> anyhow::Result<()> {
    let gateway_url = std::env::var("EXECUTION_GATEWAY_URL")?;
    let api_token = std::env::var("EXECUTION_GATEWAY_API_TOKEN")?;
    let host_id: Uuid = std::env::var("EXECUTION_REGISTERED_HOST_ID")?.parse()?;
    let api = GatewayApi::new_with_token(format!("{gateway_url}/v1"), api_token)?;
    let host: ExecutionHost = api.get(&format!("/hosts/{host_id}")).await?;
    anyhow::ensure!(
        host.state == ExecutionHostState::Ready,
        "registered host is {:?}, not ready",
        host.state
    );
    let hosted = api.connect_runtime(&host).await?;
    let conformance =
        ConformanceConfig::for_runtime(&hosted)?.with_operation_timeout(Duration::from_secs(60));
    let report = run_all(&hosted, &conformance).await?;
    println!(
        "registered host passed {} checks: {:?}",
        report.passed_checks().len(),
        report.passed_checks()
    );
    anyhow::ensure!(
        report.skipped_checks().is_empty(),
        "registered host skipped checks: {:?}",
        report.skipped_checks()
    );
    Ok(())
}

/// Creates real E2B resources through execution-gateway. Run explicitly with:
/// `E2B_API_KEY=... EXECUTION_GATEWAY_TEST_DATABASE_URL=... cargo test \
///    -p execution-gateway --test live_e2b -- --ignored --nocapture`
#[tokio::test]
#[ignore = "requires E2B_API_KEY, PostgreSQL, and creates temporary E2B resources"]
async fn real_e2b_gateway_lifecycle_snapshot_and_conformance() -> anyhow::Result<()> {
    let e2b_api_key = std::env::var("E2B_API_KEY")?;
    let database_url = std::env::var("EXECUTION_GATEWAY_TEST_DATABASE_URL")?;
    let database = Database::connect(&database_url, 5).await?;
    database.migrate().await?;
    let inspection = PgPool::connect(&database_url).await?;
    reset_database(&inspection).await?;

    let vault = CredentialVault::from_base64(&STANDARD.encode([21_u8; 32]))?;
    let provider: DynE2bProvider = Arc::new(RealE2bProvider::new(E2bProviderSettings {
        control_base_url: Url::parse("https://api.e2b.app/")?,
        envd_base_url_override: None,
        request_timeout: Duration::from_secs(30),
    }));
    let reconciler = LifecycleReconciler::new(database.clone(), vault.clone(), provider.clone());
    reconciler.clone().start(Duration::from_secs(1));
    let app = router(
        AppState {
            database,
            vault,
            provider,
            reconciler,
            api_token: Arc::from(API_TOKEN),
            default_host_timeout_seconds: HOST_TIMEOUT_SECONDS,
            public_base_url: "http://127.0.0.1/".parse()?,
            registration_token_ttl: Duration::from_secs(900),
            heartbeat_interval: Duration::from_secs(15),
            connections: HostConnections::new(Duration::from_secs(60), 64),
            security: SecurityControls::new(32, 30, 60),
        },
        16 * 1024 * 1024,
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            eprintln!("live gateway server failed: {error}");
        }
    });
    let api = GatewayApi::new(format!("http://{address}/v1"))?;

    let account: E2bAccount = api
        .post_json(
            "/e2b-accounts",
            &json!({
                "name": "live-e2b-conformance",
                "api_key": e2b_api_key,
                "is_default": true
            }),
            None,
        )
        .await?;

    let mut host_ids = Vec::new();
    let mut snapshot_ids = Vec::new();
    let result: anyhow::Result<()> = async {
        let base_host: ExecutionHost = api
            .post_json(
                "/hosts",
                &json!({
                    "name": "live-e2b-base",
                    "source": {"type": "base", "e2b_account_id": account.id},
                    "timeout_seconds": HOST_TIMEOUT_SECONDS
                }),
                Some("live-e2b-base-host"),
            )
            .await?;
        host_ids.push(base_host.id);
        let base_host = api
            .wait_for_host(base_host.id, ExecutionHostState::Ready)
            .await?;
        let base_runtime = api.connect_runtime(&base_host).await?;
        let base_config = ConformanceConfig::for_runtime(&base_runtime)?
            .with_operation_timeout(Duration::from_secs(180));
        let base_report = run_all(&base_runtime, &base_config).await?;
        println!(
            "base host passed {} checks: {:?}",
            base_report.passed_checks().len(),
            base_report.passed_checks()
        );
        anyhow::ensure!(
            base_report.skipped_checks().is_empty(),
            "base host skipped checks: {:?}",
            base_report.skipped_checks()
        );

        let snapshot: E2bSnapshot = api
            .post_json(
                &format!("/hosts/{}/snapshots", base_host.id),
                &json!({"name": "live-e2b-conformance-snapshot"}),
                Some("live-e2b-snapshot"),
            )
            .await?;
        snapshot_ids.push(snapshot.id);
        let snapshot = api
            .wait_for_snapshot(snapshot.id, SnapshotState::Ready)
            .await?;
        anyhow::ensure!(snapshot.e2b_snapshot_id.is_some(), "snapshot has no E2B ID");
        api.wait_for_host(base_host.id, ExecutionHostState::Paused)
            .await?;

        let restored_host: ExecutionHost = api
            .post_json(
                "/hosts",
                &json!({
                    "name": "live-e2b-restored",
                    "source": {"type": "snapshot", "snapshot_id": snapshot.id},
                    "timeout_seconds": HOST_TIMEOUT_SECONDS
                }),
                Some("live-e2b-restored-host"),
            )
            .await?;
        host_ids.push(restored_host.id);
        let restored_host = api
            .wait_for_host(restored_host.id, ExecutionHostState::Ready)
            .await?;
        let restored_runtime = api.connect_runtime(&restored_host).await?;
        let restored_config = ConformanceConfig::for_runtime(&restored_runtime)?
            .with_operation_timeout(Duration::from_secs(180));
        let restored_report = run_all(&restored_runtime, &restored_config).await?;
        println!(
            "restored host passed {} checks: {:?}",
            restored_report.passed_checks().len(),
            restored_report.passed_checks()
        );
        anyhow::ensure!(
            restored_report.skipped_checks().is_empty(),
            "restored host skipped checks: {:?}",
            restored_report.skipped_checks()
        );

        api.post_empty(&format!("/hosts/{}/pause", restored_host.id))
            .await?;
        api.wait_for_host(restored_host.id, ExecutionHostState::Paused)
            .await?;
        api.post_empty(&format!("/hosts/{}/resume", restored_host.id))
            .await?;
        let resumed = api
            .wait_for_host(restored_host.id, ExecutionHostState::Ready)
            .await?;
        api.connect_runtime(&resumed).await?;
        Ok(())
    }
    .await;

    let cleanup = cleanup(&api, &host_ids, &snapshot_ids).await;
    if cleanup.is_ok() {
        reset_database(&inspection).await?;
    }
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(test_error), Ok(())) => Err(test_error),
        (Ok(()), Err(cleanup_error)) => Err(cleanup_error),
        (Err(test_error), Err(cleanup_error)) => Err(anyhow::anyhow!(
            "test failed: {test_error:#}; cleanup also failed: {cleanup_error:#}"
        )),
    }
}

async fn cleanup(api: &GatewayApi, hosts: &[Uuid], snapshots: &[Uuid]) -> anyhow::Result<()> {
    let mut errors = Vec::new();
    for host_id in hosts.iter().rev() {
        if let Err(error) = api.delete(&format!("/hosts/{host_id}")).await {
            errors.push(format!("request deletion of host {host_id}: {error}"));
            continue;
        }
        if let Err(error) = api
            .wait_for_host(*host_id, ExecutionHostState::Deleted)
            .await
        {
            errors.push(format!("wait for deletion of host {host_id}: {error}"));
        }
    }
    for snapshot_id in snapshots.iter().rev() {
        if let Err(error) = api.delete(&format!("/snapshots/{snapshot_id}")).await {
            errors.push(format!(
                "request deletion of snapshot {snapshot_id}: {error}"
            ));
            continue;
        }
        if let Err(error) = api
            .wait_for_snapshot(*snapshot_id, SnapshotState::Deleted)
            .await
        {
            errors.push(format!(
                "wait for deletion of snapshot {snapshot_id}: {error}"
            ));
        }
    }
    anyhow::ensure!(errors.is_empty(), "cleanup failures: {}", errors.join("; "));
    Ok(())
}

async fn reset_database(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "truncate table idempotency_records, e2b_hosts, e2b_snapshots,
                        execution_hosts, e2b_accounts cascade",
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Clone)]
struct GatewayApi {
    client: Client,
    base_url: Arc<str>,
    api_token: Arc<str>,
}

impl GatewayApi {
    async fn connect_runtime(&self, host: &ExecutionHost) -> anyhow::Result<GatewayHostRuntime> {
        let base_url: Url = self
            .base_url
            .strip_suffix("/v1")
            .expect("test API URL ends in /v1")
            .parse()?;
        let mut config = ExecutionClientConfig::new(base_url.clone(), self.api_token.as_ref());
        config.allow_insecure_http = base_url.scheme() == "http";
        let client = ExecutionClient::new(config)?;
        Ok(client
            .connect_host(
                &OperationContext::with_timeout(Duration::from_secs(60)),
                host.id.to_string().parse()?,
            )
            .await?)
    }

    fn new(base_url: String) -> anyhow::Result<Self> {
        Ok(Self {
            client: Client::builder().timeout(Duration::from_secs(45)).build()?,
            base_url: Arc::from(base_url),
            api_token: Arc::from(API_TOKEN),
        })
    }

    fn new_with_token(base_url: String, api_token: String) -> anyhow::Result<Self> {
        Ok(Self {
            client: Client::builder().timeout(Duration::from_secs(45)).build()?,
            base_url: Arc::from(base_url),
            api_token: Arc::from(api_token),
        })
    }

    async fn post_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
        idempotency_key: Option<&str>,
    ) -> anyhow::Result<T> {
        let mut request = self
            .client
            .post(format!("{}{path}", self.base_url))
            .bearer_auth(self.api_token.as_ref())
            .json(body);
        if let Some(key) = idempotency_key {
            request = request.header("Idempotency-Key", key);
        }
        decode(request.send().await?).await
    }

    async fn post_empty(&self, path: &str) -> anyhow::Result<ExecutionHost> {
        decode(
            self.client
                .post(format!("{}{path}", self.base_url))
                .bearer_auth(self.api_token.as_ref())
                .send()
                .await?,
        )
        .await
    }

    async fn delete(&self, path: &str) -> anyhow::Result<Value> {
        decode(
            self.client
                .delete(format!("{}{path}", self.base_url))
                .bearer_auth(self.api_token.as_ref())
                .send()
                .await?,
        )
        .await
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        decode(
            self.client
                .get(format!("{}{path}", self.base_url))
                .bearer_auth(self.api_token.as_ref())
                .send()
                .await?,
        )
        .await
    }

    async fn wait_for_host(
        &self,
        id: Uuid,
        expected: ExecutionHostState,
    ) -> anyhow::Result<ExecutionHost> {
        for _ in 0..300 {
            let host: ExecutionHost = self.get(&format!("/hosts/{id}")).await?;
            if host.state == expected {
                return Ok(host);
            }
            if matches!(
                host.state,
                ExecutionHostState::Failed | ExecutionHostState::Lost
            ) {
                anyhow::bail!(
                    "host {id} reached {:?}: {}: {}",
                    host.state,
                    host.status_code.as_deref().unwrap_or("unknown"),
                    host.status_message.as_deref().unwrap_or("no message")
                );
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        anyhow::bail!("host {id} did not reach {expected:?} within 5 minutes")
    }

    async fn wait_for_snapshot(
        &self,
        id: Uuid,
        expected: SnapshotState,
    ) -> anyhow::Result<E2bSnapshot> {
        for _ in 0..300 {
            let snapshot: E2bSnapshot = self.get(&format!("/snapshots/{id}")).await?;
            if snapshot.state == expected {
                return Ok(snapshot);
            }
            if snapshot.state == SnapshotState::Failed {
                anyhow::bail!(
                    "snapshot {id} failed: {}: {}",
                    snapshot.status_code.as_deref().unwrap_or("unknown"),
                    snapshot.status_message.as_deref().unwrap_or("no message")
                );
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        anyhow::bail!("snapshot {id} did not reach {expected:?} within 5 minutes")
    }
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
