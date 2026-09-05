use std::{
    future::Future,
    sync::{Arc, OnceLock},
    time::Duration,
};

use async_trait::async_trait;
use execution_core::{
    CreateDirectoryRequest, ExecutionError, ExecutionErrorCode, ExecutionHandle,
    ExecutionHostDescriptor, ExecutionRuntime, FileMetadata, FileSystem, ListDirectoryRequest,
    ListDirectoryResult, OperationContext, ProcessRuntime, ReadExecutionRequest,
    ReadExecutionResult, ReadFileRequest, ReadFileResult, RemovePathRequest, RemovePathResult,
    ResizePtyRequest, SignalExecutionRequest, StartExecutionRequest, StatRequest,
    SupervisorGenerationId, TerminateExecutionRequest, TerminateExecutionResult, WriteFileRequest,
    WriteFileResult, WriteProcessInputRequest, WriteProcessInputResult,
};
use execution_wire::{
    Operation, OperationResult, PROTOCOL_NAME, PROTOCOL_VERSION, RequestEnvelope, RequestId,
    ResponseEnvelope,
};
use serde::Deserialize;

use crate::{
    E2bControlClient, E2bEnvdClient, E2bError, E2bResult, E2bRuntimeConfig, EnvdProcessRequest,
    SupervisorBinary,
};

const RPC_TIMEOUT_MARGIN: Duration = Duration::from_secs(5);

/// An E2B execution host backed exclusively by `execution-supervisor`.
pub struct E2bExecutionRuntime {
    descriptor: ExecutionHostDescriptor,
    transport: Arc<SupervisorTransport>,
}

impl E2bExecutionRuntime {
    /// Connects/resumes the sandbox, starts the supervisor when necessary, and
    /// completes a protocol handshake before returning a usable runtime.
    pub async fn connect(config: E2bRuntimeConfig) -> E2bResult<Self> {
        config.validate()?;
        let handshake_context = OperationContext::with_timeout(config.e2b.request_timeout);
        let control = E2bControlClient::new(config.e2b.clone())?;
        let transport = Arc::new(SupervisorTransport {
            control,
            config,
            generation: OnceLock::new(),
        });
        let response = transport
            .call_with_retry(&handshake_context, Operation::Describe)
            .await
            .map_err(E2bError::from_execution)?;
        let descriptor = match response {
            OperationResult::HostDescriptor(descriptor) => descriptor,
            result => {
                return Err(E2bError::protocol(format!(
                    "supervisor describe returned unexpected result {result:?}"
                )));
            }
        };
        if descriptor.host_id != transport.config.host_id {
            return Err(E2bError::protocol(format!(
                "supervisor host ID {} does not match requested host ID {}",
                descriptor.host_id, transport.config.host_id
            )));
        }
        Ok(Self {
            descriptor,
            transport,
        })
    }

    #[must_use]
    pub fn sandbox_id(&self) -> &str {
        &self.transport.config.sandbox_id
    }
}

impl ExecutionRuntime for E2bExecutionRuntime {
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
impl FileSystem for E2bExecutionRuntime {
    async fn stat(
        &self,
        context: &OperationContext,
        request: StatRequest,
    ) -> Result<FileMetadata, ExecutionError> {
        match self
            .transport
            .call_with_retry(context, Operation::FilesystemStat(request))
            .await?
        {
            OperationResult::FileMetadata(value) => Ok(value),
            value => Err(unexpected_result("filesystem.stat", value)),
        }
    }

    async fn read(
        &self,
        context: &OperationContext,
        request: ReadFileRequest,
    ) -> Result<ReadFileResult, ExecutionError> {
        match self
            .transport
            .call_with_retry(context, Operation::FilesystemRead(request))
            .await?
        {
            OperationResult::ReadFile(value) => Ok(value),
            value => Err(unexpected_result("filesystem.read", value)),
        }
    }

    async fn write(
        &self,
        context: &OperationContext,
        request: WriteFileRequest,
    ) -> Result<WriteFileResult, ExecutionError> {
        match self
            .transport
            .call_with_retry(context, Operation::FilesystemWrite(request))
            .await?
        {
            OperationResult::WriteFile(value) => Ok(value),
            value => Err(unexpected_result("filesystem.write", value)),
        }
    }

    async fn create_directory(
        &self,
        context: &OperationContext,
        request: CreateDirectoryRequest,
    ) -> Result<(), ExecutionError> {
        match self
            .transport
            .call_with_retry(context, Operation::FilesystemCreateDirectory(request))
            .await?
        {
            OperationResult::Unit => Ok(()),
            value => Err(unexpected_result("filesystem.create_directory", value)),
        }
    }

    async fn remove(
        &self,
        context: &OperationContext,
        request: RemovePathRequest,
    ) -> Result<RemovePathResult, ExecutionError> {
        match self
            .transport
            .call_with_retry(context, Operation::FilesystemRemove(request))
            .await?
        {
            OperationResult::RemovePath(value) => Ok(value),
            value => Err(unexpected_result("filesystem.remove", value)),
        }
    }

    async fn list(
        &self,
        context: &OperationContext,
        request: ListDirectoryRequest,
    ) -> Result<ListDirectoryResult, ExecutionError> {
        match self
            .transport
            .call_with_retry(context, Operation::FilesystemList(request))
            .await?
        {
            OperationResult::ListDirectory(value) => Ok(value),
            value => Err(unexpected_result("filesystem.list", value)),
        }
    }
}

#[async_trait]
impl ProcessRuntime for E2bExecutionRuntime {
    async fn start(
        &self,
        context: &OperationContext,
        request: StartExecutionRequest,
    ) -> Result<ExecutionHandle, ExecutionError> {
        match self
            .transport
            .call_with_retry(context, Operation::ProcessStart(request))
            .await?
        {
            OperationResult::ExecutionHandle(value) => Ok(value),
            value => Err(unexpected_result("process.start", value)),
        }
    }

    async fn read(
        &self,
        context: &OperationContext,
        request: ReadExecutionRequest,
    ) -> Result<ReadExecutionResult, ExecutionError> {
        match self
            .transport
            .call_with_retry(context, Operation::ProcessRead(request))
            .await?
        {
            OperationResult::ReadExecution(value) => Ok(value),
            value => Err(unexpected_result("process.read", value)),
        }
    }

    async fn write(
        &self,
        context: &OperationContext,
        request: WriteProcessInputRequest,
    ) -> Result<WriteProcessInputResult, ExecutionError> {
        match self
            .transport
            .call_with_retry(context, Operation::ProcessWrite(request))
            .await?
        {
            OperationResult::ProcessInput(value) => Ok(value),
            value => Err(unexpected_result("process.write", value)),
        }
    }

    async fn resize(
        &self,
        context: &OperationContext,
        request: ResizePtyRequest,
    ) -> Result<(), ExecutionError> {
        match self
            .transport
            .call_with_retry(context, Operation::ProcessResize(request))
            .await?
        {
            OperationResult::Unit => Ok(()),
            value => Err(unexpected_result("process.resize", value)),
        }
    }

    async fn signal(
        &self,
        context: &OperationContext,
        request: SignalExecutionRequest,
    ) -> Result<(), ExecutionError> {
        match self
            .transport
            .call_with_retry(context, Operation::ProcessSignal(request))
            .await?
        {
            OperationResult::Unit => Ok(()),
            value => Err(unexpected_result("process.signal", value)),
        }
    }

    async fn terminate(
        &self,
        context: &OperationContext,
        request: TerminateExecutionRequest,
    ) -> Result<TerminateExecutionResult, ExecutionError> {
        match self
            .transport
            .call_with_retry(context, Operation::ProcessTerminate(request))
            .await?
        {
            OperationResult::TerminateExecution(value) => Ok(value),
            value => Err(unexpected_result("process.terminate", value)),
        }
    }
}

struct SupervisorTransport {
    control: E2bControlClient,
    config: E2bRuntimeConfig,
    generation: OnceLock<SupervisorGenerationId>,
}

impl SupervisorTransport {
    async fn call_with_retry(
        &self,
        context: &OperationContext,
        operation: Operation,
    ) -> Result<OperationResult, ExecutionError> {
        context.checkpoint()?;
        let request = RequestEnvelope::new(RequestId::generate(), operation);
        let mut attempt = 1;
        loop {
            let result = within_context(context, self.call_once(&request)).await;
            match result {
                Ok(response) => return response_to_result(&request, response),
                Err(error)
                    if error.retryable
                        && attempt < self.config.e2b.retry.max_attempts
                        && !context.is_cancelled() =>
                {
                    let delay = self.config.e2b.retry.delay_for_retry(attempt - 1);
                    within_context(context, async {
                        tokio::time::sleep(delay).await;
                        Ok(())
                    })
                    .await
                    .map_err(E2bError::into_execution)?;
                    attempt += 1;
                }
                Err(error) => return Err(error.into_execution()),
            }
        }
    }

    async fn call_once(&self, request: &RequestEnvelope) -> E2bResult<ResponseEnvelope> {
        let envd = self.connected_envd().await?;
        let descriptor = ensure_supervisor(&envd, &self.config).await?;
        if descriptor.host_id != self.config.host_id {
            return Err(E2bError::protocol(format!(
                "running supervisor belongs to host {}, expected {}",
                descriptor.host_id, self.config.host_id
            )));
        }
        if let Some(expected) = self.generation.get() {
            if expected != &descriptor.supervisor_generation_id {
                return Err(E2bError::new(
                    crate::E2bErrorKind::NotFound,
                    format!(
                        "execution supervisor generation changed from {expected} to {}; reconnect the runtime",
                        descriptor.supervisor_generation_id
                    ),
                ));
            }
        } else {
            let _ = self
                .generation
                .set(descriptor.supervisor_generation_id.clone());
        }
        invoke_rpc(&envd, &self.config, request).await
    }

    async fn connected_envd(&self) -> E2bResult<E2bEnvdClient> {
        let connection = self
            .control
            .ensure_connected(&self.config.sandbox_id)
            .await?;
        let envd = E2bEnvdClient::new(&self.config.e2b, &connection)?;
        crate::control::retry(&self.config.e2b, || envd.health()).await?;
        Ok(envd)
    }
}

async fn ensure_supervisor(
    envd: &E2bEnvdClient,
    config: &E2bRuntimeConfig,
) -> E2bResult<ExecutionHostDescriptor> {
    if let Ok(descriptor) = supervisor_health(envd, config).await {
        if descriptor.host_id == config.host_id {
            return Ok(descriptor);
        }
        replace_inherited_supervisor(envd, config, &descriptor).await?;
    }

    if let SupervisorBinary::Upload {
        local_path,
        remote_path,
    } = &config.supervisor.binary
    {
        let bytes = tokio::fs::read(local_path).await.map_err(|error| {
            E2bError::new(
                crate::E2bErrorKind::Io,
                format!(
                    "cannot read supervisor binary {}: {error}",
                    local_path.display()
                ),
            )
        })?;
        envd.upload_file(remote_path, bytes).await?;
        let mut chmod = EnvdProcessRequest::new("/bin/chmod");
        chmod.arguments = vec!["0755".to_owned(), remote_path.clone()];
        require_success(envd.run_process(chmod).await?, "chmod supervisor")?;
    }

    verify_supervisor_binary(envd, config).await?;
    let supervisor = &config.supervisor;
    let mut serve = EnvdProcessRequest::new(supervisor.binary.remote_path());
    serve.arguments = vec![
        "serve".to_owned(),
        "--host-id".to_owned(),
        config.host_id.to_string(),
        "--root".to_owned(),
        supervisor.workspace_path.clone(),
        "--root-id".to_owned(),
        supervisor.root_id.to_string(),
        "--root-name".to_owned(),
        supervisor.root_name.clone(),
        "--socket".to_owned(),
        supervisor.socket_path.clone(),
        "--state-dir".to_owned(),
        supervisor.state_directory.clone(),
    ];
    if supervisor.read_only {
        serve.arguments.push("--read-only".to_owned());
    }
    serve.tag = Some(format!("execution-supervisor-{}", config.host_id));
    envd.start_detached(serve).await?;

    let deadline = tokio::time::Instant::now() + supervisor.startup_timeout;
    let mut delay = Duration::from_millis(50);
    loop {
        match supervisor_health(envd, config).await {
            Ok(descriptor) => return Ok(descriptor),
            Err(error) if tokio::time::Instant::now() < deadline => {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                tokio::time::sleep(delay.min(remaining)).await;
                delay = delay.saturating_mul(2).min(Duration::from_secs(1));
                if remaining.is_zero() {
                    return Err(error);
                }
            }
            Err(error) => {
                return Err(E2bError::unavailable(format!(
                    "execution supervisor did not become healthy: {error}"
                )));
            }
        }
    }
}

async fn replace_inherited_supervisor(
    envd: &E2bEnvdClient,
    config: &E2bRuntimeConfig,
    descriptor: &ExecutionHostDescriptor,
) -> E2bResult<()> {
    let processes = envd.list_processes().await?;
    let matches: Vec<_> = processes
        .iter()
        .filter(|process| inherited_supervisor_process(process, config, descriptor))
        .collect();
    if matches.is_empty() {
        return Err(E2bError::protocol(format!(
            "a supervisor for inherited host {} is healthy, but its envd process could not be identified safely",
            descriptor.host_id
        )));
    }
    for process in matches {
        envd.kill_process(process.pid).await?;
    }

    let deadline = tokio::time::Instant::now() + config.supervisor.startup_timeout;
    loop {
        let processes = envd.list_processes().await?;
        if !processes
            .iter()
            .any(|process| inherited_supervisor_process(process, config, descriptor))
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(E2bError::unavailable(format!(
                "inherited supervisor for host {} did not stop",
                descriptor.host_id
            )));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn inherited_supervisor_process(
    process: &crate::envd::EnvdProcessInfo,
    config: &E2bRuntimeConfig,
    descriptor: &ExecutionHostDescriptor,
) -> bool {
    process.config.cmd == config.supervisor.binary.remote_path()
        && flag_value(&process.config.args, "--host-id") == Some(descriptor.host_id.as_str())
        && flag_value(&process.config.args, "--socket")
            == Some(config.supervisor.socket_path.as_str())
        && process
            .config
            .args
            .first()
            .is_some_and(|value| value == "serve")
}

fn flag_value<'a>(arguments: &'a [String], flag: &str) -> Option<&'a str> {
    arguments
        .windows(2)
        .find(|values| values[0] == flag)
        .map(|values| values[1].as_str())
}

async fn verify_supervisor_binary(
    envd: &E2bEnvdClient,
    config: &E2bRuntimeConfig,
) -> E2bResult<()> {
    let mut request = EnvdProcessRequest::new(config.supervisor.binary.remote_path());
    request.arguments.push("version".to_owned());
    let output = require_success(envd.run_process(request).await?, "query supervisor version")?;
    let version: VersionReport = serde_json::from_slice(&output.stdout).map_err(|error| {
        E2bError::protocol(format!(
            "supervisor returned invalid version report: {error}"
        ))
    })?;
    if version.program != "execution-supervisor"
        || version.version != env!("CARGO_PKG_VERSION")
        || version.protocol != PROTOCOL_NAME
        || version.protocol_version != PROTOCOL_VERSION
    {
        return Err(E2bError::protocol(format!(
            "incompatible supervisor: program={}, version={}, protocol={} v{}",
            version.program, version.version, version.protocol, version.protocol_version
        )));
    }
    Ok(())
}

async fn supervisor_health(
    envd: &E2bEnvdClient,
    config: &E2bRuntimeConfig,
) -> E2bResult<ExecutionHostDescriptor> {
    let mut request = EnvdProcessRequest::new(config.supervisor.binary.remote_path());
    request.arguments = vec![
        "health".to_owned(),
        "--socket".to_owned(),
        config.supervisor.socket_path.clone(),
    ];
    let output = require_success(envd.run_process(request).await?, "probe supervisor")?;
    let report: HealthReport = serde_json::from_slice(&output.stdout).map_err(|error| {
        E2bError::protocol(format!(
            "supervisor returned invalid health report: {error}"
        ))
    })?;
    if report.status != "ok"
        || report.supervisor_version != env!("CARGO_PKG_VERSION")
        || report.protocol != PROTOCOL_NAME
        || report.protocol_version != PROTOCOL_VERSION
    {
        return Err(E2bError::protocol(format!(
            "incompatible supervisor health report: status={}, version={}, protocol={} v{}",
            report.status, report.supervisor_version, report.protocol, report.protocol_version
        )));
    }
    Ok(report.descriptor)
}

async fn invoke_rpc(
    envd: &E2bEnvdClient,
    config: &E2bRuntimeConfig,
    request: &RequestEnvelope,
) -> E2bResult<ResponseEnvelope> {
    let timeout = rpc_timeout(request, config.e2b.request_timeout);
    let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    let mut process = EnvdProcessRequest::new(config.supervisor.binary.remote_path());
    process.arguments = vec![
        "rpc".to_owned(),
        "--socket".to_owned(),
        config.supervisor.socket_path.clone(),
        "--timeout-ms".to_owned(),
        timeout_ms.to_string(),
    ];
    process.stdin = serde_json::to_vec(request)?;
    process.stdin.push(b'\n');
    process.timeout = Some(timeout.saturating_add(RPC_TIMEOUT_MARGIN));
    process.tag = Some(format!("execution-rpc-{}", request.request_id));
    let output = envd.run_process(process).await?;
    require_success(output.clone(), "invoke supervisor RPC")?;
    let line = output
        .stdout
        .split(|byte| *byte == b'\n')
        .find(|line| !line.is_empty())
        .ok_or_else(|| E2bError::protocol("supervisor RPC returned no response"))?;
    serde_json::from_slice(line)
        .map_err(|error| E2bError::protocol(format!("invalid supervisor response: {error}")))
}

fn rpc_timeout(request: &RequestEnvelope, baseline: Duration) -> Duration {
    let wait = match &request.operation {
        Operation::ProcessRead(request) => request.wait_ms.map(Duration::from_millis),
        _ => None,
    };
    wait.map_or(baseline, |wait| baseline.max(wait + RPC_TIMEOUT_MARGIN))
}

fn require_success(
    output: crate::EnvdProcessOutput,
    operation: &str,
) -> E2bResult<crate::EnvdProcessOutput> {
    if output.exit_code == 0 {
        return Ok(output);
    }
    Err(E2bError::protocol(format!(
        "failed to {operation} (exit {}): {}",
        output.exit_code,
        String::from_utf8_lossy(&output.stderr)
    )))
}

fn response_to_result(
    request: &RequestEnvelope,
    response: ResponseEnvelope,
) -> Result<OperationResult, ExecutionError> {
    if response.version() != PROTOCOL_VERSION {
        return Err(ExecutionError::new(
            ExecutionErrorCode::Internal,
            format!(
                "supervisor protocol version {} does not match {}",
                response.version(),
                PROTOCOL_VERSION
            ),
        ));
    }
    if response.request_id() != &request.request_id {
        return Err(ExecutionError::new(
            ExecutionErrorCode::Internal,
            "supervisor response request ID did not match the request",
        ));
    }
    match response {
        ResponseEnvelope::Success { result, .. } => Ok(result),
        ResponseEnvelope::Error { error, .. } => Err(error),
    }
}

fn unexpected_result(operation: &str, result: OperationResult) -> ExecutionError {
    ExecutionError::new(
        ExecutionErrorCode::Internal,
        format!("{operation} returned unexpected supervisor result {result:?}"),
    )
}

async fn within_context<T>(
    context: &OperationContext,
    future: impl Future<Output = E2bResult<T>>,
) -> E2bResult<T> {
    if context.is_cancelled() {
        return Err(E2bError::from_execution(ExecutionError::cancelled()));
    }
    if let Some(remaining) = context.remaining() {
        if remaining.is_zero() {
            return Err(E2bError::from_execution(ExecutionError::deadline_exceeded()));
        }
        tokio::select! {
            biased;
            () = context.cancelled() => Err(E2bError::from_execution(ExecutionError::cancelled())),
            result = tokio::time::timeout(remaining, future) => match result {
                Ok(result) => result,
                Err(_) => Err(E2bError::from_execution(ExecutionError::deadline_exceeded())),
            },
        }
    } else {
        tokio::select! {
            biased;
            () = context.cancelled() => Err(E2bError::from_execution(ExecutionError::cancelled())),
            result = future => result,
        }
    }
}

#[derive(Deserialize)]
struct HealthReport {
    status: String,
    supervisor_version: String,
    protocol: String,
    protocol_version: u32,
    descriptor: ExecutionHostDescriptor,
}

#[derive(Deserialize)]
struct VersionReport {
    program: String,
    version: String,
    protocol: String,
    protocol_version: u32,
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use axum::{
        Json, Router,
        body::{Body, Bytes},
        extract::State,
        http::Response as HttpResponse,
        routing::{get, post},
    };
    use base64::{Engine, engine::general_purpose::STANDARD};
    use execution_core::ExecutionHostId;
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use url::Url;

    use crate::{E2bApiKey, E2bConfig, RetryPolicy, SupervisorConfig};

    use super::*;

    #[tokio::test]
    async fn connects_through_envd_and_completes_the_supervisor_handshake() {
        #[derive(Clone)]
        struct MockState {
            descriptor: Arc<Value>,
        }

        async fn connect() -> Json<Value> {
            Json(json!({
                "sandboxID":"sandbox-1",
                "envdAccessToken":"envd-token",
                "domain":"test.invalid"
            }))
        }

        async fn health() -> &'static str {
            "ok"
        }

        async fn process_start(State(state): State<MockState>, body: Bytes) -> HttpResponse<Body> {
            let request = decode_request(&body);
            let arguments = request["process"]["args"].as_array().unwrap();
            let command = arguments[0].as_str().unwrap();
            let stdout = match command {
                "health" => serde_json::to_vec(&json!({
                    "status":"ok",
                    "supervisor_version":env!("CARGO_PKG_VERSION"),
                    "protocol":PROTOCOL_NAME,
                    "protocol_version":PROTOCOL_VERSION,
                    "descriptor":state.descriptor.as_ref(),
                }))
                .unwrap(),
                "rpc" => {
                    let tag = request["tag"].as_str().unwrap();
                    let request_id = tag.strip_prefix("execution-rpc-").unwrap();
                    serde_json::to_vec(&json!({
                        "status":"success",
                        "version":PROTOCOL_VERSION,
                        "request_id":request_id,
                        "result":{
                            "result":"host_descriptor",
                            "value":state.descriptor.as_ref(),
                        }
                    }))
                    .unwrap()
                }
                other => panic!("unexpected command {other}"),
            };

            let mut response = Vec::new();
            response.extend(frame(json!({"event":{"start":{"pid":11}}}), 0));
            response.extend(frame(
                json!({"event":{"data":{"stdout":STANDARD.encode(stdout)}}}),
                0,
            ));
            response.extend(frame(
                json!({"event":{"end":{"exitCode":0,"exited":true,"status":"exited"}}}),
                0,
            ));
            response.extend(frame(json!({}), 2));
            HttpResponse::builder()
                .status(200)
                .header("content-type", "application/connect+json")
                .body(Body::from(response))
                .unwrap()
        }

        async fn unary() -> &'static str {
            "{}"
        }

        let descriptor = Arc::new(json!({
            "host_id":"host-1",
            "supervisor_generation_id":"generation-1",
            "operating_system":{"type":"linux"},
            "architecture":"x86_64",
            "path_convention":"unix",
            "roots":[{
                "id":"workspace",
                "name":"Workspace",
                "native_path":"/home/user",
                "read_only":false
            }],
            "features":{
                "pty":true,
                "process_signals":true,
                "file_revisions":true
            },
            "limits":{}
        }));
        let app = Router::new()
            .route("/sandboxes/sandbox-1/connect", post(connect))
            .route("/health", get(health))
            .route("/process.Process/Start", post(process_start))
            .route("/process.Process/SendInput", post(unary))
            .route("/process.Process/CloseStdin", post(unary))
            .with_state(MockState {
                descriptor: Arc::clone(&descriptor),
            });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base_url = Url::parse(&format!("http://{address}/")).unwrap();

        let mut e2b = E2bConfig::new(E2bApiKey::new("secret").unwrap()).unwrap();
        e2b.control_base_url = base_url.clone();
        e2b.envd_base_url_override = Some(base_url);
        e2b.request_timeout = Duration::from_secs(2);
        e2b.retry = RetryPolicy {
            max_attempts: 1,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        };
        let runtime = E2bExecutionRuntime::connect(E2bRuntimeConfig {
            e2b,
            sandbox_id: "sandbox-1".to_owned(),
            host_id: ExecutionHostId::new("host-1").unwrap(),
            supervisor: SupervisorConfig::default(),
        })
        .await
        .unwrap();

        assert_eq!(runtime.sandbox_id(), "sandbox-1");
        assert_eq!(runtime.descriptor().host_id.as_str(), "host-1");
        assert_eq!(
            runtime.descriptor().supervisor_generation_id.as_str(),
            "generation-1"
        );
    }

    fn decode_request(bytes: &[u8]) -> Value {
        let length = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
        serde_json::from_slice(&bytes[5..5 + length]).unwrap()
    }

    fn frame(value: Value, flags: u8) -> Vec<u8> {
        let payload = serde_json::to_vec(&value).unwrap();
        let mut bytes = vec![flags];
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&payload);
        bytes
    }

    #[test]
    fn inherited_supervisor_matching_requires_exact_identity_and_endpoint() {
        let config = E2bRuntimeConfig {
            e2b: E2bConfig::new(E2bApiKey::new("secret").unwrap()).unwrap(),
            sandbox_id: "sandbox-1".to_owned(),
            host_id: ExecutionHostId::new("new-host").unwrap(),
            supervisor: SupervisorConfig::default(),
        };
        let descriptor: ExecutionHostDescriptor = serde_json::from_value(json!({
            "host_id":"old-host",
            "supervisor_generation_id":"generation-1",
            "operating_system":{"type":"linux"},
            "architecture":"x86_64",
            "path_convention":"unix",
            "roots":[{
                "id":"workspace",
                "name":"Workspace",
                "native_path":"/home/user",
                "read_only":false
            }],
            "features":{
                "pty":true,
                "process_signals":true,
                "file_revisions":true
            },
            "limits":{}
        }))
        .unwrap();
        let process = crate::envd::EnvdProcessInfo {
            config: crate::envd::EnvdProcessConfig {
                cmd: "/usr/local/bin/execution-supervisor".to_owned(),
                args: vec![
                    "serve".to_owned(),
                    "--host-id".to_owned(),
                    "old-host".to_owned(),
                    "--socket".to_owned(),
                    "/tmp/agent-pane-execution/supervisor.sock".to_owned(),
                ],
            },
            pid: 42,
            tag: Some("execution-supervisor-old-host".to_owned()),
        };

        assert!(inherited_supervisor_process(&process, &config, &descriptor));
        let mut unrelated = process;
        unrelated.config.args[2] = "different-host".to_owned();
        assert!(!inherited_supervisor_process(
            &unrelated,
            &config,
            &descriptor
        ));
    }
}
