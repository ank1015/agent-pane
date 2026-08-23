use std::{pin::Pin, sync::Arc};

use execution_contracts::{ExecutionError, ExecutionErrorCode};
use execution_local::LocalExecutionEnvironment;
use execution_runtime::{ExecutionEnvironment, ExecutionResult, OperationContext};
use futures_util::{Stream, StreamExt};

use crate::protocol::{Operation, Response, StreamItem};

pub type ResponseStream = Pin<Box<dyn Stream<Item = ExecutionResult<StreamItem>> + Send + 'static>>;

#[allow(clippy::large_enum_variant)]
pub enum DispatchResult {
    Unary(Response),
    Stream(ResponseStream),
}

#[derive(Clone)]
pub struct Dispatcher {
    environment: Arc<LocalExecutionEnvironment>,
}

impl Dispatcher {
    pub fn new(environment: Arc<LocalExecutionEnvironment>) -> Self {
        Self { environment }
    }

    pub async fn dispatch(
        &self,
        context: &OperationContext,
        operation: Operation,
    ) -> ExecutionResult<DispatchResult> {
        let environment = self.environment.as_ref();
        let result = match operation {
            Operation::Describe => {
                DispatchResult::Unary(Response::Descriptor(environment.descriptor().clone()))
            }
            Operation::WorkspaceInspect(request) => DispatchResult::Unary(Response::FileMetadata(
                environment
                    .workspace_query()
                    .inspect(context, request)
                    .await?,
            )),
            Operation::WorkspaceInspectMany(request) => {
                DispatchResult::Unary(Response::InspectMany(
                    environment
                        .workspace_query()
                        .inspect_many(context, request)
                        .await?,
                ))
            }
            Operation::WorkspaceRead(request) => DispatchResult::Unary(Response::Read(
                environment.workspace_query().read(context, request).await?,
            )),
            Operation::WorkspaceList(request) => DispatchResult::Unary(Response::List(
                environment.workspace_query().list(context, request).await?,
            )),
            Operation::WorkspaceSearch(request) => DispatchResult::Unary(Response::Search(
                environment
                    .workspace_query()
                    .search(context, request)
                    .await?,
            )),
            Operation::MutationPrepare(request) => DispatchResult::Unary(
                Response::PreparedMutation(mutation(environment)?.prepare(context, request).await?),
            ),
            Operation::MutationCommit(request) => DispatchResult::Unary(Response::Mutation(
                mutation(environment)?.commit(context, request).await?,
            )),
            Operation::MutationAbort(request) => {
                mutation(environment)?.abort(context, request).await?;
                DispatchResult::Unary(Response::Unit)
            }
            Operation::MutationApply(request) => DispatchResult::Unary(Response::Mutation(
                mutation(environment)?.apply(context, request).await?,
            )),
            Operation::ProcessStart(request) => DispatchResult::Unary(Response::ExecutionHandle(
                processes(environment)?.start(context, request).await?,
            )),
            Operation::ProcessInspect(request) => DispatchResult::Unary(Response::ExecutionStatus(
                processes(environment)?.inspect(context, request).await?,
            )),
            Operation::ProcessAttach(request) => {
                let stream = processes(environment)?.attach(context, request).await?;
                DispatchResult::Stream(Box::pin(
                    stream.map(|item| item.map(StreamItem::ProcessEvent)),
                ))
            }
            Operation::ProcessWriteInput(request) => DispatchResult::Unary(Response::ProcessInput(
                processes(environment)?
                    .write_input(context, request)
                    .await?,
            )),
            Operation::ProcessResizePty(request) => {
                processes(environment)?.resize_pty(context, request).await?;
                DispatchResult::Unary(Response::Unit)
            }
            Operation::ProcessSignal(request) => {
                processes(environment)?.signal(context, request).await?;
                DispatchResult::Unary(Response::Unit)
            }
            Operation::ProcessTerminate(request) => DispatchResult::Unary(Response::Terminated(
                processes(environment)?.terminate(context, request).await?,
            )),
            Operation::ArtifactMetadata(request) => {
                DispatchResult::Unary(Response::ArtifactMetadata(
                    artifacts(environment)?.metadata(context, request).await?,
                ))
            }
            Operation::ArtifactOpen(request) => {
                let stream = artifacts(environment)?.open(context, request).await?;
                DispatchResult::Stream(Box::pin(
                    stream.map(|item| item.map(StreamItem::ArtifactChunk)),
                ))
            }
            Operation::FilesystemInspect(request) => DispatchResult::Unary(Response::FileMetadata(
                filesystem(environment)?.inspect(context, request).await?,
            )),
            Operation::FilesystemReadBytes(request) => DispatchResult::Unary(Response::ReadBytes(
                filesystem(environment)?
                    .read_bytes(context, request)
                    .await?,
            )),
            Operation::FilesystemWriteBytes(request) => {
                DispatchResult::Unary(Response::WriteBytes(
                    filesystem(environment)?
                        .write_bytes(context, request)
                        .await?,
                ))
            }
            Operation::FilesystemCreateDirectory(request) => {
                filesystem(environment)?
                    .create_directory(context, request)
                    .await?;
                DispatchResult::Unary(Response::Unit)
            }
            Operation::FilesystemRemove(request) => {
                filesystem(environment)?.remove(context, request).await?;
                DispatchResult::Unary(Response::Unit)
            }
            Operation::FilesystemMove(request) => {
                filesystem(environment)?.move_path(context, request).await?;
                DispatchResult::Unary(Response::Unit)
            }
            Operation::FilesystemCopy(request) => {
                filesystem(environment)?.copy_path(context, request).await?;
                DispatchResult::Unary(Response::Unit)
            }
            Operation::FilesystemListRaw(request) => DispatchResult::Unary(Response::List(
                filesystem(environment)?.list_raw(context, request).await?,
            )),
            Operation::FilesystemWalk(request) => DispatchResult::Unary(Response::Walk(
                filesystem(environment)?.walk(context, request).await?,
            )),
        };
        Ok(result)
    }
}

fn mutation(
    environment: &dyn ExecutionEnvironment,
) -> ExecutionResult<&dyn execution_runtime::WorkspaceMutation> {
    environment
        .workspace_mutation()
        .ok_or_else(missing_capability)
}

fn processes(
    environment: &dyn ExecutionEnvironment,
) -> ExecutionResult<&dyn execution_runtime::ProcessRuntime> {
    environment.process_runtime().ok_or_else(missing_capability)
}

fn artifacts(
    environment: &dyn ExecutionEnvironment,
) -> ExecutionResult<&dyn execution_runtime::ArtifactStore> {
    environment.artifact_store().ok_or_else(missing_capability)
}

fn filesystem(
    environment: &dyn ExecutionEnvironment,
) -> ExecutionResult<&dyn execution_runtime::BasicFileSystem> {
    environment.filesystem().ok_or_else(missing_capability)
}

fn missing_capability() -> ExecutionError {
    ExecutionError {
        code: ExecutionErrorCode::Unsupported,
        message: "environment does not implement the requested capability".to_owned(),
        retryable: false,
        details: Default::default(),
    }
}
