use std::{pin::Pin, sync::Arc};

use execution_contracts::{ExecutionError, ExecutionErrorCode};
use execution_local::LocalExecutionRuntime;
use execution_runtime::{ExecutionResult, ExecutionRuntime, OperationContext};
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
    runtime: Arc<LocalExecutionRuntime>,
}

impl Dispatcher {
    pub fn new(runtime: Arc<LocalExecutionRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn dispatch(
        &self,
        context: &OperationContext,
        operation: Operation,
    ) -> ExecutionResult<DispatchResult> {
        let runtime = self.runtime.as_ref();
        let result = match operation {
            Operation::Describe => {
                DispatchResult::Unary(Response::MachineDescriptor(runtime.descriptor().clone()))
            }
            Operation::WorkspaceInspect(request) => DispatchResult::Unary(Response::FileMetadata(
                runtime.workspace_query().inspect(context, request).await?,
            )),
            Operation::WorkspaceInspectMany(request) => {
                DispatchResult::Unary(Response::InspectMany(
                    runtime
                        .workspace_query()
                        .inspect_many(context, request)
                        .await?,
                ))
            }
            Operation::WorkspaceRead(request) => DispatchResult::Unary(Response::Read(
                runtime.workspace_query().read(context, request).await?,
            )),
            Operation::WorkspaceList(request) => DispatchResult::Unary(Response::List(
                runtime.workspace_query().list(context, request).await?,
            )),
            Operation::WorkspaceSearch(request) => DispatchResult::Unary(Response::Search(
                runtime.workspace_query().search(context, request).await?,
            )),
            Operation::MutationPrepare(request) => DispatchResult::Unary(
                Response::PreparedMutation(mutation(runtime)?.prepare(context, request).await?),
            ),
            Operation::MutationCommit(request) => DispatchResult::Unary(Response::Mutation(
                mutation(runtime)?.commit(context, request).await?,
            )),
            Operation::MutationAbort(request) => {
                mutation(runtime)?.abort(context, request).await?;
                DispatchResult::Unary(Response::Unit)
            }
            Operation::MutationApply(request) => DispatchResult::Unary(Response::Mutation(
                mutation(runtime)?.apply(context, request).await?,
            )),
            Operation::ProcessStart(request) => DispatchResult::Unary(Response::ExecutionHandle(
                processes(runtime)?.start(context, request).await?,
            )),
            Operation::ProcessInspect(request) => DispatchResult::Unary(Response::ExecutionStatus(
                processes(runtime)?.inspect(context, request).await?,
            )),
            Operation::ProcessAttach(request) => {
                let stream = processes(runtime)?.attach(context, request).await?;
                DispatchResult::Stream(Box::pin(
                    stream.map(|item| item.map(StreamItem::ProcessEvent)),
                ))
            }
            Operation::ProcessWriteInput(request) => DispatchResult::Unary(Response::ProcessInput(
                processes(runtime)?.write_input(context, request).await?,
            )),
            Operation::ProcessResizePty(request) => {
                processes(runtime)?.resize_pty(context, request).await?;
                DispatchResult::Unary(Response::Unit)
            }
            Operation::ProcessSignal(request) => {
                processes(runtime)?.signal(context, request).await?;
                DispatchResult::Unary(Response::Unit)
            }
            Operation::ProcessTerminate(request) => DispatchResult::Unary(Response::Terminated(
                processes(runtime)?.terminate(context, request).await?,
            )),
            Operation::ArtifactMetadata(request) => DispatchResult::Unary(
                Response::ArtifactMetadata(artifacts(runtime)?.metadata(context, request).await?),
            ),
            Operation::ArtifactOpen(request) => {
                let stream = artifacts(runtime)?.open(context, request).await?;
                DispatchResult::Stream(Box::pin(
                    stream.map(|item| item.map(StreamItem::ArtifactChunk)),
                ))
            }
            Operation::FilesystemInspect(request) => DispatchResult::Unary(Response::FileMetadata(
                filesystem(runtime)?.inspect(context, request).await?,
            )),
            Operation::FilesystemReadBytes(request) => DispatchResult::Unary(Response::ReadBytes(
                filesystem(runtime)?.read_bytes(context, request).await?,
            )),
            Operation::FilesystemWriteBytes(request) => DispatchResult::Unary(
                Response::WriteBytes(filesystem(runtime)?.write_bytes(context, request).await?),
            ),
            Operation::FilesystemCreateDirectory(request) => {
                filesystem(runtime)?
                    .create_directory(context, request)
                    .await?;
                DispatchResult::Unary(Response::Unit)
            }
            Operation::FilesystemRemove(request) => {
                filesystem(runtime)?.remove(context, request).await?;
                DispatchResult::Unary(Response::Unit)
            }
            Operation::FilesystemMove(request) => {
                filesystem(runtime)?.move_path(context, request).await?;
                DispatchResult::Unary(Response::Unit)
            }
            Operation::FilesystemCopy(request) => {
                filesystem(runtime)?.copy_path(context, request).await?;
                DispatchResult::Unary(Response::Unit)
            }
            Operation::FilesystemListRaw(request) => DispatchResult::Unary(Response::List(
                filesystem(runtime)?.list_raw(context, request).await?,
            )),
            Operation::FilesystemWalk(request) => DispatchResult::Unary(Response::Walk(
                filesystem(runtime)?.walk(context, request).await?,
            )),
        };
        Ok(result)
    }
}

fn mutation(
    runtime: &dyn ExecutionRuntime,
) -> ExecutionResult<&dyn execution_runtime::WorkspaceMutation> {
    runtime.workspace_mutation().ok_or_else(missing_capability)
}

fn processes(
    runtime: &dyn ExecutionRuntime,
) -> ExecutionResult<&dyn execution_runtime::ProcessRuntime> {
    runtime.process_runtime().ok_or_else(missing_capability)
}

fn artifacts(
    runtime: &dyn ExecutionRuntime,
) -> ExecutionResult<&dyn execution_runtime::ArtifactStore> {
    runtime.artifact_store().ok_or_else(missing_capability)
}

fn filesystem(
    runtime: &dyn ExecutionRuntime,
) -> ExecutionResult<&dyn execution_runtime::BasicFileSystem> {
    runtime.filesystem().ok_or_else(missing_capability)
}

fn missing_capability() -> ExecutionError {
    ExecutionError {
        code: ExecutionErrorCode::Unsupported,
        message: "runtime does not implement the requested capability".to_owned(),
        retryable: false,
        details: Default::default(),
    }
}
