use std::pin::Pin;

use async_trait::async_trait;
use execution_contracts::{
    AttachExecutionRequest, ExecutionHandle, ExecutionStatus, InspectExecutionRequest,
    ProcessEvent, ResizePtyRequest, SignalExecutionRequest, StartExecutionRequest,
    TerminateExecutionRequest, TerminateExecutionResult, WriteProcessInputRequest,
    WriteProcessInputResult,
};
use futures_core::Stream;

use crate::{ExecutionResult, OperationContext};

/// Ordered process events pushed by an execution backend.
pub type ProcessEventStream =
    Pin<Box<dyn Stream<Item = ExecutionResult<ProcessEvent>> + Send + 'static>>;

/// Durable process and PTY sessions owned by the target runtime.
#[async_trait]
pub trait ProcessRuntime: Send + Sync {
    async fn start(
        &self,
        context: &OperationContext,
        request: StartExecutionRequest,
    ) -> ExecutionResult<ExecutionHandle>;

    async fn inspect(
        &self,
        context: &OperationContext,
        request: InspectExecutionRequest,
    ) -> ExecutionResult<ExecutionStatus>;

    /// Attaches to pushed events after the requested sequence number.
    async fn attach(
        &self,
        context: &OperationContext,
        request: AttachExecutionRequest,
    ) -> ExecutionResult<ProcessEventStream>;

    async fn write_input(
        &self,
        context: &OperationContext,
        request: WriteProcessInputRequest,
    ) -> ExecutionResult<WriteProcessInputResult>;

    async fn resize_pty(
        &self,
        context: &OperationContext,
        request: ResizePtyRequest,
    ) -> ExecutionResult<()>;

    async fn signal(
        &self,
        context: &OperationContext,
        request: SignalExecutionRequest,
    ) -> ExecutionResult<()>;

    async fn terminate(
        &self,
        context: &OperationContext,
        request: TerminateExecutionRequest,
    ) -> ExecutionResult<TerminateExecutionResult>;
}
