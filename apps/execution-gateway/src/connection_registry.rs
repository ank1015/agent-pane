use std::{collections::HashMap, sync::Arc, time::Duration};

use axum::extract::ws::{Message, WebSocket};
use execution_contracts::{ExecutionError, ExecutionErrorCode};
use execution_protocol::{ClientMessage, PROTOCOL_NAME, ServerMessage};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Mutex, RwLock, mpsc, watch};
use uuid::Uuid;

use crate::db::Database;

#[derive(Clone, Default)]
pub struct ConnectionRegistry {
    connections: Arc<RwLock<HashMap<String, Arc<Connection>>>>,
}

struct Connection {
    id: Uuid,
    sender: mpsc::Sender<ClientMessage>,
    shutdown: watch::Sender<bool>,
    pending: Mutex<HashMap<String, mpsc::Sender<ServerMessage>>>,
}

impl ConnectionRegistry {
    pub async fn online(&self, machine_id: &str) -> bool {
        self.connections.read().await.contains_key(machine_id)
    }

    pub async fn dispatch(
        &self,
        machine_id: &str,
        request_id: String,
        operation: Box<execution_protocol::Operation>,
    ) -> Result<mpsc::Receiver<ServerMessage>, ExecutionError> {
        let connection = self
            .connections
            .read()
            .await
            .get(machine_id)
            .cloned()
            .ok_or_else(|| {
                execution_error(ExecutionErrorCode::Disconnected, "machine is offline", true)
            })?;
        let (sender, receiver) = mpsc::channel(64);
        connection
            .pending
            .lock()
            .await
            .insert(request_id.clone(), sender);
        if connection
            .sender
            .send(ClientMessage::Request {
                request_id: request_id.clone(),
                operation,
            })
            .await
            .is_err()
        {
            connection.pending.lock().await.remove(&request_id);
            return Err(execution_error(
                ExecutionErrorCode::Disconnected,
                "machine connection closed",
                true,
            ));
        }
        Ok(receiver)
    }

    pub async fn cancel(&self, machine_id: &str, request_id: String) {
        let connection = self.connections.read().await.get(machine_id).cloned();
        if let Some(connection) = connection {
            let _ = connection
                .sender
                .send(ClientMessage::Cancel { request_id })
                .await;
        }
    }

    pub async fn disconnect(&self, machine_id: &str) {
        let connection = self.connections.write().await.remove(machine_id);
        if let Some(connection) = connection {
            let _ = connection.shutdown.send(true);
            connection.pending.lock().await.clear();
        }
    }

    pub async fn accept(
        &self,
        machine_id: String,
        socket: WebSocket,
        database: Database,
    ) -> Result<(), ExecutionError> {
        let (mut writer, mut reader) = socket.split();
        let ready = tokio::time::timeout(Duration::from_secs(10), reader.next())
            .await
            .map_err(|_| {
                execution_error(
                    ExecutionErrorCode::DeadlineExceeded,
                    "daemon ready handshake timed out",
                    false,
                )
            })?
            .ok_or_else(|| {
                execution_error(
                    ExecutionErrorCode::Disconnected,
                    "daemon closed before ready",
                    true,
                )
            })?
            .map_err(|source| {
                execution_error(ExecutionErrorCode::Disconnected, source.to_string(), true)
            })?;
        let Message::Text(text) = ready else {
            return Err(execution_error(
                ExecutionErrorCode::InvalidRequest,
                "first daemon message must be ready JSON",
                false,
            ));
        };
        let ServerMessage::Ready {
            protocol,
            descriptor,
        } = serde_json::from_str(&text).map_err(|source| {
            execution_error(
                ExecutionErrorCode::InvalidRequest,
                source.to_string(),
                false,
            )
        })?
        else {
            return Err(execution_error(
                ExecutionErrorCode::InvalidRequest,
                "first daemon message must be ready",
                false,
            ));
        };
        if protocol != PROTOCOL_NAME || descriptor.machine_id.as_str() != machine_id {
            return Err(execution_error(
                ExecutionErrorCode::PermissionDenied,
                "daemon identity or protocol mismatch",
                false,
            ));
        }
        database
            .mark_seen(&machine_id, &descriptor)
            .await
            .map_err(|source| {
                execution_error(ExecutionErrorCode::Internal, source.to_string(), true)
            })?;

        let (command_tx, mut command_rx) = mpsc::channel::<ClientMessage>(64);
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let connection = Arc::new(Connection {
            id: Uuid::now_v7(),
            sender: command_tx,
            shutdown: shutdown_tx,
            pending: Mutex::new(HashMap::new()),
        });
        self.connections
            .write()
            .await
            .insert(machine_id.clone(), Arc::clone(&connection));
        tracing::info!(%machine_id, "machine connected");

        let writer_task = tokio::spawn(async move {
            let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
            loop {
                let message = tokio::select! {
                    _ = heartbeat.tick() => Message::Ping(Vec::new().into()),
                    result = shutdown_rx.changed() => {
                        if result.is_ok() && *shutdown_rx.borrow() {
                            let _ = writer.send(Message::Close(None)).await;
                        }
                        break;
                    }
                    command = command_rx.recv() => {
                        let Some(command) = command else { break; };
                        match serde_json::to_string(&command) { Ok(value) => Message::Text(value.into()), Err(_) => break }
                    }
                };
                if writer.send(message).await.is_err() {
                    break;
                }
            }
        });

        while let Some(message) = reader.next().await {
            let Ok(Message::Text(text)) = message else {
                continue;
            };
            let Ok(message) = serde_json::from_str::<ServerMessage>(&text) else {
                continue;
            };
            let request_id = match &message {
                ServerMessage::Response { request_id, .. }
                | ServerMessage::StreamItem { request_id, .. }
                | ServerMessage::StreamEnd { request_id } => Some(request_id.clone()),
                ServerMessage::Error { request_id, .. } => request_id.clone(),
                ServerMessage::Ready { .. } => None,
            };
            if let Some(request_id) = request_id {
                let terminal = matches!(
                    message,
                    ServerMessage::Response { .. }
                        | ServerMessage::StreamEnd { .. }
                        | ServerMessage::Error { .. }
                );
                let sender = connection.pending.lock().await.get(&request_id).cloned();
                if let Some(sender) = sender {
                    let _ = sender.send(message).await;
                }
                if terminal {
                    connection.pending.lock().await.remove(&request_id);
                }
            }
        }
        writer_task.abort();
        let mut connections = self.connections.write().await;
        if connections
            .get(&machine_id)
            .is_some_and(|current| current.id == connection.id)
        {
            connections.remove(&machine_id);
        }
        drop(connections);
        connection.pending.lock().await.clear();
        tracing::info!(%machine_id, "machine disconnected");
        Ok(())
    }
}

pub fn execution_error(
    code: ExecutionErrorCode,
    message: impl Into<String>,
    retryable: bool,
) -> ExecutionError {
    ExecutionError {
        code,
        message: message.into(),
        retryable,
        details: Default::default(),
    }
}
