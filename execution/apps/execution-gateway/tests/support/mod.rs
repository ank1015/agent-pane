use async_trait::async_trait;
use execution_api::ExecutionHost;
use execution_core::{
    CreateDirectoryRequest, ExecutionError, ExecutionErrorCode, ExecutionHandle,
    ExecutionHostDescriptor, ExecutionResult, ExecutionRuntime, FileMetadata, FileSystem,
    ListDirectoryRequest, ListDirectoryResult, OperationContext, ProcessRuntime,
    ReadExecutionRequest, ReadExecutionResult, ReadFileRequest, ReadFileResult, RemovePathRequest,
    RemovePathResult, ResizePtyRequest, SignalExecutionRequest, StartExecutionRequest, StatRequest,
    TerminateExecutionRequest, TerminateExecutionResult, WriteFileRequest, WriteFileResult,
    WriteProcessInputRequest, WriteProcessInputResult,
};
use execution_wire::{
    Operation, OperationResult, PROTOCOL_VERSION, RequestEnvelope, RequestId, ResponseEnvelope,
};
use reqwest::{Client, StatusCode};
use uuid::Uuid;

pub struct HostedGatewayRuntime {
    client: Client,
    base_url: String,
    api_token: String,
    host_id: Uuid,
    descriptor: ExecutionHostDescriptor,
}

impl HostedGatewayRuntime {
    pub fn new(base_url: String, api_token: &str, host: &ExecutionHost) -> anyhow::Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url,
            api_token: api_token.to_owned(),
            host_id: host.id,
            descriptor: host
                .descriptor
                .clone()
                .ok_or_else(|| anyhow::anyhow!("ready host has no descriptor"))?,
        })
    }

    pub async fn call(
        &self,
        context: &OperationContext,
        operation: Operation,
    ) -> ExecutionResult<OperationResult> {
        context.checkpoint()?;
        let request = RequestEnvelope::new(RequestId::generate(), operation);
        let request_id = request.request_id.clone();
        let response = self
            .client
            .post(format!(
                "{}/hosts/{}/operations",
                self.base_url, self.host_id
            ))
            .bearer_auth(&self.api_token)
            .json(&request)
            .send()
            .await
            .map_err(http_execution_error)?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(http_execution_error)?;
        if !status.is_success() {
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
        match response {
            ResponseEnvelope::Success { result, .. } => Ok(result),
            ResponseEnvelope::Error { error, .. } => Err(error),
        }
    }
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
        StatusCode::CONFLICT | StatusCode::SERVICE_UNAVAILABLE => ExecutionErrorCode::Unavailable,
        _ => ExecutionErrorCode::Io,
    }
}
