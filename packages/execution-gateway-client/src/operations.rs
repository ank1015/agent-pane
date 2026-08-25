use std::{future::Future, pin::Pin, task::Poll};

use execution_contracts::{
    ArtifactChunk, ArtifactMetadata, ExecutionError, ExecutionErrorCode, ExecutionHandle,
    ExecutionStatus, FileMetadata, InspectManyResult, ListResult, MutationResult, PreparedMutation,
    ProcessEvent, ReadBytesResult, ReadResult, SearchResult, TerminateExecutionResult, WalkResult,
    WriteBytesResult, WriteProcessInputResult,
};
use execution_protocol::{Operation, OperationRecord, OperationStatus, Response, StreamItem};
use execution_runtime::{ExecutionResult, OperationContext};
use futures_core::Stream;
use tokio::sync::mpsc;

use crate::{
    GatewayExecutionEnvironment,
    error::{
        cancelled_error, deadline_error, execution_error, unexpected_response,
        unexpected_stream_item,
    },
};

pub(crate) trait FromResponse: Sized {
    const EXPECTED: &'static str;
    fn from_response(response: Response) -> Option<Self>;
}

macro_rules! response_type {
    ($type:ty, $variant:ident, $expected:literal) => {
        impl FromResponse for $type {
            const EXPECTED: &'static str = $expected;

            fn from_response(response: Response) -> Option<Self> {
                match response {
                    Response::$variant(value) => Some(value),
                    _ => None,
                }
            }
        }
    };
}

response_type!(FileMetadata, FileMetadata, "file_metadata");
response_type!(InspectManyResult, InspectMany, "inspect_many");
response_type!(ReadResult, Read, "read");
response_type!(ListResult, List, "list");
response_type!(SearchResult, Search, "search");
response_type!(PreparedMutation, PreparedMutation, "prepared_mutation");
response_type!(MutationResult, Mutation, "mutation");
response_type!(ExecutionHandle, ExecutionHandle, "execution_handle");
response_type!(ExecutionStatus, ExecutionStatus, "execution_status");
response_type!(WriteProcessInputResult, ProcessInput, "process_input");
response_type!(TerminateExecutionResult, Terminated, "terminated");
response_type!(ArtifactMetadata, ArtifactMetadata, "artifact_metadata");
response_type!(ReadBytesResult, ReadBytes, "read_bytes");
response_type!(WriteBytesResult, WriteBytes, "write_bytes");
response_type!(WalkResult, Walk, "walk");

impl FromResponse for () {
    const EXPECTED: &'static str = "unit";

    fn from_response(response: Response) -> Option<Self> {
        matches!(response, Response::Unit).then_some(())
    }
}

pub(crate) trait FromStreamItem: Send + 'static + Sized {
    const EXPECTED: &'static str;
    fn from_stream_item(item: StreamItem) -> Option<Self>;
}

impl FromStreamItem for ProcessEvent {
    const EXPECTED: &'static str = "process_event";

    fn from_stream_item(item: StreamItem) -> Option<Self> {
        match item {
            StreamItem::ProcessEvent(event) => Some(event),
            StreamItem::ArtifactChunk(_) => None,
        }
    }
}

impl FromStreamItem for ArtifactChunk {
    const EXPECTED: &'static str = "artifact_chunk";

    fn from_stream_item(item: StreamItem) -> Option<Self> {
        match item {
            StreamItem::ArtifactChunk(chunk) => Some(chunk),
            StreamItem::ProcessEvent(_) => None,
        }
    }
}

impl GatewayExecutionEnvironment {
    pub(crate) async fn execute<T>(
        &self,
        context: &OperationContext,
        operation: Operation,
    ) -> ExecutionResult<T>
    where
        T: FromResponse,
    {
        let record = await_request(
            context,
            self.client
                .create_operation(&self.descriptor.machine_id, operation),
        )
        .await?;
        let operation_id = record.operation_id.clone();
        let result = self.wait_for_response(context, record).await;
        if result.as_ref().is_err_and(is_context_stop) {
            let _ = self.client.cancel_operation(&operation_id).await;
        }
        let response = result?;
        T::from_response(response).ok_or_else(|| unexpected_response(T::EXPECTED))
    }

    pub(crate) async fn stream<T>(
        &self,
        context: &OperationContext,
        operation: Operation,
    ) -> ExecutionResult<Pin<Box<dyn Stream<Item = ExecutionResult<T>> + Send + 'static>>>
    where
        T: FromStreamItem,
    {
        let record = await_request(
            context,
            self.client
                .create_operation(&self.descriptor.machine_id, operation),
        )
        .await?;
        let (sender, receiver) = mpsc::channel(64);
        let client = self.client.clone();
        let stream_context = context.child();
        tokio::spawn(async move {
            let operation_id = record.operation_id.clone();
            let result = drive_stream::<T>(&client, &stream_context, record, &sender).await;
            if let Err(error) = result {
                let _ = client.cancel_operation(&operation_id).await;
                let _ = sender.send(Err(error)).await;
            }
        });
        Ok(Box::pin(GatewayStream { receiver }))
    }

    async fn wait_for_response(
        &self,
        context: &OperationContext,
        mut record: OperationRecord,
    ) -> ExecutionResult<Response> {
        loop {
            match record.status {
                OperationStatus::Completed => {
                    return record.response.ok_or_else(|| {
                        execution_error(
                            ExecutionErrorCode::Internal,
                            "execution gateway completed a unary operation without a response",
                            false,
                        )
                    });
                }
                OperationStatus::Failed => return Err(operation_failure(record)),
                OperationStatus::Cancelled => return Err(cancelled_error()),
                OperationStatus::Queued | OperationStatus::Running => {}
            }
            wait_for_poll(context, self.client.poll_interval).await?;
            record = await_request(context, self.client.operation(&record.operation_id)).await?;
        }
    }
}

struct GatewayStream<T> {
    receiver: mpsc::Receiver<ExecutionResult<T>>,
}

impl<T> Stream for GatewayStream<T> {
    type Item = ExecutionResult<T>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(context)
    }
}

async fn drive_stream<T>(
    client: &crate::ExecutionGatewayClient,
    context: &OperationContext,
    mut record: OperationRecord,
    sender: &mpsc::Sender<ExecutionResult<T>>,
) -> ExecutionResult<()>
where
    T: FromStreamItem,
{
    let mut after = 0;
    loop {
        loop {
            let events = await_request(
                context,
                client.operation_events(&record.operation_id, after, client.event_page_size),
            )
            .await?;
            let full_page = events.len() == client.event_page_size as usize;
            for event in events {
                after = after.max(event.sequence);
                let item = T::from_stream_item(event.item)
                    .ok_or_else(|| unexpected_stream_item(T::EXPECTED))?;
                if !send_stream_item(context, sender, Ok(item)).await? {
                    let _ = client.cancel_operation(&record.operation_id).await;
                    return Ok(());
                }
            }
            if !full_page {
                break;
            }
        }

        match record.status {
            OperationStatus::Completed => return Ok(()),
            OperationStatus::Failed => return Err(operation_failure(record)),
            OperationStatus::Cancelled => return Err(cancelled_error()),
            OperationStatus::Queued | OperationStatus::Running => {}
        }
        wait_for_poll(context, client.poll_interval).await?;
        record = await_request(context, client.operation(&record.operation_id)).await?;
    }
}

async fn send_stream_item<T>(
    context: &OperationContext,
    sender: &mpsc::Sender<ExecutionResult<T>>,
    item: ExecutionResult<T>,
) -> ExecutionResult<bool> {
    if context.is_cancelled() {
        return Err(cancelled_error());
    }
    match context.remaining() {
        Some(remaining) => {
            tokio::select! {
                _ = context.cancelled() => Err(cancelled_error()),
                _ = tokio::time::sleep(remaining) => Err(deadline_error()),
                result = sender.send(item) => Ok(result.is_ok()),
            }
        }
        None => {
            tokio::select! {
                _ = context.cancelled() => Err(cancelled_error()),
                result = sender.send(item) => Ok(result.is_ok()),
            }
        }
    }
}

async fn await_request<T, F>(context: &OperationContext, request: F) -> ExecutionResult<T>
where
    F: Future<Output = Result<T, crate::ExecutionGatewayClientError>>,
{
    if context.is_cancelled() {
        return Err(cancelled_error());
    }
    match context.remaining() {
        Some(remaining) => {
            tokio::select! {
                _ = context.cancelled() => Err(cancelled_error()),
                _ = tokio::time::sleep(remaining) => Err(deadline_error()),
                result = request => result.map_err(crate::ExecutionGatewayClientError::into_execution_error),
            }
        }
        None => {
            tokio::select! {
                _ = context.cancelled() => Err(cancelled_error()),
                result = request => result.map_err(crate::ExecutionGatewayClientError::into_execution_error),
            }
        }
    }
}

async fn wait_for_poll(
    context: &OperationContext,
    interval: std::time::Duration,
) -> ExecutionResult<()> {
    if context.is_cancelled() {
        return Err(cancelled_error());
    }
    let delay = context
        .remaining()
        .map_or(interval, |remaining| std::cmp::min(interval, remaining));
    tokio::select! {
        _ = context.cancelled() => Err(cancelled_error()),
        _ = tokio::time::sleep(delay) => {
            if context.remaining().is_some_and(|remaining| remaining.is_zero()) {
                Err(deadline_error())
            } else {
                Ok(())
            }
        },
    }
}

fn operation_failure(record: OperationRecord) -> ExecutionError {
    record.error.unwrap_or_else(|| {
        execution_error(
            ExecutionErrorCode::Internal,
            "execution gateway failed an operation without an error",
            false,
        )
    })
}

fn is_context_stop(error: &ExecutionError) -> bool {
    matches!(
        error.code,
        ExecutionErrorCode::Cancelled | ExecutionErrorCode::DeadlineExceeded
    )
}
