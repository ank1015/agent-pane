use async_trait::async_trait;
use execution_contracts::{
    AttachExecutionRequest, ExecutionHandle, ExecutionStatus, InspectExecutionRequest,
    ResizePtyRequest, SignalExecutionRequest, StartExecutionRequest, TerminateExecutionRequest,
    TerminateExecutionResult, WriteProcessInputRequest, WriteProcessInputResult,
};
use execution_protocol::Operation;
use execution_runtime::{ExecutionResult, OperationContext, ProcessEventStream, ProcessRuntime};

use crate::GatewayExecutionEnvironment;

#[async_trait]
impl ProcessRuntime for GatewayExecutionEnvironment {
    async fn start(
        &self,
        context: &OperationContext,
        request: StartExecutionRequest,
    ) -> ExecutionResult<ExecutionHandle> {
        self.execute(context, Operation::ProcessStart(request))
            .await
    }

    async fn inspect(
        &self,
        context: &OperationContext,
        request: InspectExecutionRequest,
    ) -> ExecutionResult<ExecutionStatus> {
        self.execute(context, Operation::ProcessInspect(request))
            .await
    }

    async fn attach(
        &self,
        context: &OperationContext,
        request: AttachExecutionRequest,
    ) -> ExecutionResult<ProcessEventStream> {
        self.stream(context, Operation::ProcessAttach(request))
            .await
    }

    async fn write_input(
        &self,
        context: &OperationContext,
        request: WriteProcessInputRequest,
    ) -> ExecutionResult<WriteProcessInputResult> {
        self.execute(context, Operation::ProcessWriteInput(request))
            .await
    }

    async fn resize_pty(
        &self,
        context: &OperationContext,
        request: ResizePtyRequest,
    ) -> ExecutionResult<()> {
        self.execute(context, Operation::ProcessResizePty(request))
            .await
    }

    async fn signal(
        &self,
        context: &OperationContext,
        request: SignalExecutionRequest,
    ) -> ExecutionResult<()> {
        self.execute(context, Operation::ProcessSignal(request))
            .await
    }

    async fn terminate(
        &self,
        context: &OperationContext,
        request: TerminateExecutionRequest,
    ) -> ExecutionResult<TerminateExecutionResult> {
        self.execute(context, Operation::ProcessTerminate(request))
            .await
    }
}
