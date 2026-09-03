use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use execution_api::{E2bAccount, E2bSnapshot, ExecutionHost, ExecutionHostState, SnapshotState};
use execution_conformance::{ConformanceConfig, run_all};
use execution_core::{
    CreateDirectoryRequest, ExecutionError, ExecutionErrorCode, ExecutionHandle,
    ExecutionHostDescriptor, ExecutionResult, ExecutionRuntime, FileMetadata, FileSystem,
    ListDirectoryRequest, ListDirectoryResult, OperationContext, ProcessRuntime,
    ReadExecutionRequest, ReadExecutionResult, ReadFileRequest, ReadFileResult, RemovePathRequest,
    RemovePathResult, ResizePtyRequest, SignalExecutionRequest, StartExecutionRequest, StatRequest,
    TerminateExecutionRequest, TerminateExecutionResult, WriteFileRequest, WriteFileResult,
    WriteProcessInputRequest, WriteProcessInputResult,
};
use execution_gateway::{
    AppState, CredentialVault, Database, DynE2bProvider, E2bProviderSettings, HostConnections,
    LifecycleReconciler, RealE2bProvider, SecurityControls, router,
};
use execution_wire::{
    Operation, OperationResult, PROTOCOL_VERSION, RequestEnvelope, RequestId, ResponseEnvelope,
};
use reqwest::{Client, StatusCode};
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
    let hosted = HostedGatewayRuntime::new(&api, &host)?;
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
        let base_runtime = HostedGatewayRuntime::new(&api, &base_host)?;
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
        let restored_runtime = HostedGatewayRuntime::new(&api, &restored_host)?;
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
        let resumed_runtime = HostedGatewayRuntime::new(&api, &resumed)?;
        resumed_runtime
            .call(&OperationContext::new(), Operation::Describe)
            .await?;
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

struct HostedGatewayRuntime {
    api: GatewayApi,
    host_id: Uuid,
    descriptor: ExecutionHostDescriptor,
}

impl HostedGatewayRuntime {
    fn new(api: &GatewayApi, host: &ExecutionHost) -> anyhow::Result<Self> {
        Ok(Self {
            api: api.clone(),
            host_id: host.id,
            descriptor: host
                .descriptor
                .clone()
                .ok_or_else(|| anyhow::anyhow!("ready host has no descriptor"))?,
        })
    }

    async fn call(
        &self,
        context: &OperationContext,
        operation: Operation,
    ) -> ExecutionResult<OperationResult> {
        context.checkpoint()?;
        let request = RequestEnvelope::new(RequestId::generate(), operation);
        let request_id = request.request_id.clone();
        let mut delay = Duration::from_millis(250);
        loop {
            context.checkpoint()?;
            let response = self
                .api
                .client
                .post(format!(
                    "{}/hosts/{}/operations",
                    self.api.base_url, self.host_id
                ))
                .bearer_auth(self.api.api_token.as_ref())
                .json(&request)
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(error) if retryable_http_error(&error) && context.remaining().is_some() => {
                    retry_delay(context, delay).await?;
                    delay = delay.saturating_mul(2).min(Duration::from_secs(5));
                    continue;
                }
                Err(error) => return Err(http_execution_error(error)),
            };
            let status = response.status();
            let bytes = response.bytes().await.map_err(http_execution_error)?;
            if !status.is_success() {
                if retryable_gateway_status(status) && context.remaining().is_some() {
                    retry_delay(context, delay).await?;
                    delay = delay.saturating_mul(2).min(Duration::from_secs(5));
                    continue;
                }
                return Err(ExecutionError::new(
                    status_to_execution_code(status),
                    format!(
                        "gateway returned {status}: {}",
                        String::from_utf8_lossy(&bytes)
                    ),
                ));
            }
            let response: ResponseEnvelope = serde_json::from_slice(&bytes).map_err(|error| {
                ExecutionError::new(
                    ExecutionErrorCode::Internal,
                    format!("gateway returned invalid wire JSON: {error}"),
                )
            })?;
            if response.version() != PROTOCOL_VERSION || response.request_id() != &request_id {
                return Err(ExecutionError::new(
                    ExecutionErrorCode::Internal,
                    "gateway returned an uncorrelated execution response",
                ));
            }
            return match response {
                ResponseEnvelope::Success { result, .. } => Ok(result),
                ResponseEnvelope::Error { error, .. } => Err(error),
            };
        }
    }
}

async fn retry_delay(context: &OperationContext, delay: Duration) -> ExecutionResult<()> {
    let remaining = context
        .remaining()
        .ok_or_else(ExecutionError::deadline_exceeded)?;
    if remaining <= delay {
        return Err(ExecutionError::deadline_exceeded());
    }
    tokio::time::sleep(delay).await;
    context.checkpoint()
}

fn retryable_http_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_body()
}

fn retryable_gateway_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::CONFLICT
            | StatusCode::REQUEST_TIMEOUT
            | StatusCode::TOO_MANY_REQUESTS
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

impl ExecutionRuntime for HostedGatewayRuntime {
    fn descriptor(&self) -> &ExecutionHostDescriptor {
        &self.descriptor
    }

    fn filesystem(&self) -> &dyn FileSystem {
        self
    }

    fn processes(&self) -> &dyn ProcessRuntime {
        self
    }
}

#[async_trait]
impl FileSystem for HostedGatewayRuntime {
    async fn stat(
        &self,
        context: &OperationContext,
        request: StatRequest,
    ) -> ExecutionResult<FileMetadata> {
        expect_result(
            "filesystem.stat",
            self.call(context, Operation::FilesystemStat(request))
                .await?,
            |result| match result {
                OperationResult::FileMetadata(value) => Some(value),
                _ => None,
            },
        )
    }

    async fn read(
        &self,
        context: &OperationContext,
        request: ReadFileRequest,
    ) -> ExecutionResult<ReadFileResult> {
        expect_result(
            "filesystem.read",
            self.call(context, Operation::FilesystemRead(request))
                .await?,
            |result| match result {
                OperationResult::ReadFile(value) => Some(value),
                _ => None,
            },
        )
    }

    async fn write(
        &self,
        context: &OperationContext,
        request: WriteFileRequest,
    ) -> ExecutionResult<WriteFileResult> {
        expect_result(
            "filesystem.write",
            self.call(context, Operation::FilesystemWrite(request))
                .await?,
            |result| match result {
                OperationResult::WriteFile(value) => Some(value),
                _ => None,
            },
        )
    }

    async fn create_directory(
        &self,
        context: &OperationContext,
        request: CreateDirectoryRequest,
    ) -> ExecutionResult<()> {
        expect_unit(
            "filesystem.create_directory",
            self.call(context, Operation::FilesystemCreateDirectory(request))
                .await?,
        )
    }

    async fn remove(
        &self,
        context: &OperationContext,
        request: RemovePathRequest,
    ) -> ExecutionResult<RemovePathResult> {
        expect_result(
            "filesystem.remove",
            self.call(context, Operation::FilesystemRemove(request))
                .await?,
            |result| match result {
                OperationResult::RemovePath(value) => Some(value),
                _ => None,
            },
        )
    }

    async fn list(
        &self,
        context: &OperationContext,
        request: ListDirectoryRequest,
    ) -> ExecutionResult<ListDirectoryResult> {
        expect_result(
            "filesystem.list",
            self.call(context, Operation::FilesystemList(request))
                .await?,
            |result| match result {
                OperationResult::ListDirectory(value) => Some(value),
                _ => None,
            },
        )
    }
}

#[async_trait]
impl ProcessRuntime for HostedGatewayRuntime {
    async fn start(
        &self,
        context: &OperationContext,
        request: StartExecutionRequest,
    ) -> ExecutionResult<ExecutionHandle> {
        expect_result(
            "process.start",
            self.call(context, Operation::ProcessStart(request)).await?,
            |result| match result {
                OperationResult::ExecutionHandle(value) => Some(value),
                _ => None,
            },
        )
    }

    async fn read(
        &self,
        context: &OperationContext,
        request: ReadExecutionRequest,
    ) -> ExecutionResult<ReadExecutionResult> {
        expect_result(
            "process.read",
            self.call(context, Operation::ProcessRead(request)).await?,
            |result| match result {
                OperationResult::ReadExecution(value) => Some(value),
                _ => None,
            },
        )
    }

    async fn write(
        &self,
        context: &OperationContext,
        request: WriteProcessInputRequest,
    ) -> ExecutionResult<WriteProcessInputResult> {
        expect_result(
            "process.write",
            self.call(context, Operation::ProcessWrite(request)).await?,
            |result| match result {
                OperationResult::ProcessInput(value) => Some(value),
                _ => None,
            },
        )
    }

    async fn resize(
        &self,
        context: &OperationContext,
        request: ResizePtyRequest,
    ) -> ExecutionResult<()> {
        expect_unit(
            "process.resize",
            self.call(context, Operation::ProcessResize(request))
                .await?,
        )
    }

    async fn signal(
        &self,
        context: &OperationContext,
        request: SignalExecutionRequest,
    ) -> ExecutionResult<()> {
        expect_unit(
            "process.signal",
            self.call(context, Operation::ProcessSignal(request))
                .await?,
        )
    }

    async fn terminate(
        &self,
        context: &OperationContext,
        request: TerminateExecutionRequest,
    ) -> ExecutionResult<TerminateExecutionResult> {
        expect_result(
            "process.terminate",
            self.call(context, Operation::ProcessTerminate(request))
                .await?,
            |result| match result {
                OperationResult::TerminateExecution(value) => Some(value),
                _ => None,
            },
        )
    }
}

fn expect_result<T>(
    operation: &str,
    result: OperationResult,
    extract: impl FnOnce(OperationResult) -> Option<T>,
) -> ExecutionResult<T> {
    extract(result).ok_or_else(|| {
        ExecutionError::new(
            ExecutionErrorCode::Internal,
            format!("gateway returned an unexpected result for {operation}"),
        )
    })
}

fn expect_unit(operation: &str, result: OperationResult) -> ExecutionResult<()> {
    if result == OperationResult::Unit {
        Ok(())
    } else {
        Err(ExecutionError::new(
            ExecutionErrorCode::Internal,
            format!("gateway returned an unexpected result for {operation}"),
        ))
    }
}

fn http_execution_error(error: reqwest::Error) -> ExecutionError {
    ExecutionError::new(
        if error.is_timeout() {
            ExecutionErrorCode::DeadlineExceeded
        } else {
            ExecutionErrorCode::Unavailable
        },
        format!("gateway HTTP request failed: {error}"),
    )
}

fn status_to_execution_code(status: StatusCode) -> ExecutionErrorCode {
    match status {
        StatusCode::NOT_FOUND => ExecutionErrorCode::ExecutionNotFound,
        StatusCode::GONE => ExecutionErrorCode::ExecutionLost,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ExecutionErrorCode::PermissionDenied,
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => {
            ExecutionErrorCode::DeadlineExceeded
        }
        StatusCode::TOO_MANY_REQUESTS => ExecutionErrorCode::ResourceExhausted,
        StatusCode::CONFLICT => ExecutionErrorCode::Unavailable,
        _ => ExecutionErrorCode::Io,
    }
}
