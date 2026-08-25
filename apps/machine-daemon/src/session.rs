use std::{collections::HashMap, sync::Arc};

use execution_contracts::{ExecutionError, ExecutionErrorCode, MachineDescriptor};
use execution_runtime::OperationContext;
use tokio::sync::{Mutex, mpsc};

use crate::{
    dispatch::{DispatchResult, Dispatcher},
    protocol::{ClientMessage, PROTOCOL_NAME, ServerMessage},
};

pub async fn run(
    descriptor: MachineDescriptor,
    dispatcher: Dispatcher,
    mut incoming: mpsc::Receiver<ClientMessage>,
    outgoing: mpsc::Sender<ServerMessage>,
) {
    let contexts = Arc::new(Mutex::new(HashMap::<String, OperationContext>::new()));
    if outgoing
        .send(ServerMessage::Ready {
            protocol: PROTOCOL_NAME.to_owned(),
            descriptor,
        })
        .await
        .is_err()
    {
        return;
    }

    while let Some(message) = incoming.recv().await {
        match message {
            ClientMessage::Cancel { request_id } => {
                if let Some(context) = contexts.lock().await.get(&request_id) {
                    context.cancel();
                }
            }
            ClientMessage::Request {
                request_id,
                operation,
            } => {
                if request_id.trim().is_empty() {
                    let _ = outgoing
                        .send(ServerMessage::Error {
                            request_id: None,
                            error: protocol_error("request_id must not be empty"),
                        })
                        .await;
                    continue;
                }
                let context = OperationContext::new();
                {
                    let mut active = contexts.lock().await;
                    if active.contains_key(&request_id) {
                        drop(active);
                        let _ = outgoing
                            .send(ServerMessage::Error {
                                request_id: Some(request_id),
                                error: protocol_error("request_id is already active"),
                            })
                            .await;
                        continue;
                    }
                    active.insert(request_id.clone(), context.clone());
                }
                let dispatcher = dispatcher.clone();
                let outgoing = outgoing.clone();
                let contexts = Arc::clone(&contexts);
                tokio::spawn(async move {
                    match dispatcher.dispatch(&context, *operation).await {
                        Ok(DispatchResult::Unary(response)) => {
                            let _ = outgoing
                                .send(ServerMessage::Response {
                                    request_id: request_id.clone(),
                                    response,
                                })
                                .await;
                        }
                        Ok(DispatchResult::Stream(mut stream)) => {
                            use futures_util::StreamExt;
                            loop {
                                let item = tokio::select! {
                                    () = context.cancelled() => None,
                                    item = stream.next() => item,
                                };
                                let Some(item) = item else {
                                    break;
                                };
                                let message = match item {
                                    Ok(item) => ServerMessage::StreamItem {
                                        request_id: request_id.clone(),
                                        item,
                                    },
                                    Err(error) => ServerMessage::Error {
                                        request_id: Some(request_id.clone()),
                                        error,
                                    },
                                };
                                if outgoing.send(message).await.is_err() {
                                    context.cancel();
                                    break;
                                }
                            }
                            let _ = outgoing
                                .send(ServerMessage::StreamEnd {
                                    request_id: request_id.clone(),
                                })
                                .await;
                        }
                        Err(error) => {
                            let _ = outgoing
                                .send(ServerMessage::Error {
                                    request_id: Some(request_id.clone()),
                                    error,
                                })
                                .await;
                        }
                    }
                    contexts.lock().await.remove(&request_id);
                });
            }
        }
    }

    for context in contexts.lock().await.values() {
        context.cancel();
    }
}

pub fn protocol_error(message: impl Into<String>) -> ExecutionError {
    ExecutionError {
        code: ExecutionErrorCode::InvalidRequest,
        message: message.into(),
        retryable: false,
        details: Default::default(),
    }
}
