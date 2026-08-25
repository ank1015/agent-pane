use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use execution_contracts::{ExecutionError, ExecutionErrorCode};
use execution_protocol::{ConnectorKind, Operation, ServerMessage};
use futures_util::StreamExt;
use tokio::sync::{Mutex, mpsc};

use crate::{
    connection_registry::{ConnectionRegistry, execution_error},
    db::MachineRecord,
    runtime_dispatch::{DispatchResult, dispatch},
    sandbox_runtime::SandboxRuntimeFactory,
};

#[async_trait]
pub trait MachineConnector: Send + Sync {
    async fn execute(
        &self,
        machine: &MachineRecord,
        request_id: String,
        operation: Box<Operation>,
    ) -> Result<mpsc::Receiver<ServerMessage>, ExecutionError>;

    async fn cancel(&self, machine: &MachineRecord, request_id: String);
}

pub struct DaemonConnector {
    connections: ConnectionRegistry,
}

pub struct SandboxConnector {
    runtimes: SandboxRuntimeFactory,
    operations: Arc<Mutex<HashMap<String, execution_runtime::OperationContext>>>,
}

impl SandboxConnector {
    pub fn new(runtimes: SandboxRuntimeFactory) -> Self {
        Self {
            runtimes,
            operations: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl MachineConnector for SandboxConnector {
    async fn execute(
        &self,
        machine: &MachineRecord,
        request_id: String,
        operation: Box<Operation>,
    ) -> Result<mpsc::Receiver<ServerMessage>, ExecutionError> {
        let runtime = self
            .runtimes
            .connect(&machine.summary.machine_id, &machine.summary.name)
            .await
            .map_err(|error| {
                execution_error(
                    ExecutionErrorCode::Disconnected,
                    format!("could not connect to sandbox runtime: {error}"),
                    true,
                )
            })?;
        let context = execution_runtime::OperationContext::new();
        self.operations
            .lock()
            .await
            .insert(request_id.clone(), context.clone());
        let operations = Arc::clone(&self.operations);
        let (sender, receiver) = mpsc::channel(64);
        tokio::spawn(async move {
            let result = dispatch(runtime.as_ref(), &context, *operation).await;
            match result {
                Ok(DispatchResult::Unary(response)) => {
                    let _ = sender
                        .send(ServerMessage::Response {
                            request_id: request_id.clone(),
                            response,
                        })
                        .await;
                }
                Ok(DispatchResult::Stream(mut stream)) => {
                    while let Some(item) = stream.next().await {
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
                        if sender.send(message).await.is_err() {
                            break;
                        }
                    }
                    let _ = sender
                        .send(ServerMessage::StreamEnd {
                            request_id: request_id.clone(),
                        })
                        .await;
                }
                Err(error) => {
                    let _ = sender
                        .send(ServerMessage::Error {
                            request_id: Some(request_id.clone()),
                            error,
                        })
                        .await;
                }
            }
            operations.lock().await.remove(&request_id);
        });
        Ok(receiver)
    }

    async fn cancel(&self, _machine: &MachineRecord, request_id: String) {
        if let Some(context) = self.operations.lock().await.remove(&request_id) {
            context.cancel();
        }
    }
}

impl DaemonConnector {
    pub fn new(connections: ConnectionRegistry) -> Self {
        Self { connections }
    }
}

#[async_trait]
impl MachineConnector for DaemonConnector {
    async fn execute(
        &self,
        machine: &MachineRecord,
        request_id: String,
        operation: Box<Operation>,
    ) -> Result<mpsc::Receiver<ServerMessage>, ExecutionError> {
        self.connections
            .dispatch(machine.summary.machine_id.as_str(), request_id, operation)
            .await
    }

    async fn cancel(&self, machine: &MachineRecord, request_id: String) {
        self.connections
            .cancel(machine.summary.machine_id.as_str(), request_id)
            .await;
    }
}

#[derive(Clone)]
pub struct ConnectorRouter {
    daemon: Arc<dyn MachineConnector>,
    sandbox: Option<Arc<dyn MachineConnector>>,
}

impl ConnectorRouter {
    pub fn new(connections: ConnectionRegistry) -> Self {
        Self {
            daemon: Arc::new(DaemonConnector::new(connections)),
            sandbox: None,
        }
    }

    pub fn with_sandbox(connections: ConnectionRegistry, runtimes: SandboxRuntimeFactory) -> Self {
        Self {
            daemon: Arc::new(DaemonConnector::new(connections)),
            sandbox: Some(Arc::new(SandboxConnector::new(runtimes))),
        }
    }
    pub async fn execute(
        &self,
        machine: &MachineRecord,
        request_id: String,
        operation: Box<Operation>,
    ) -> Result<mpsc::Receiver<ServerMessage>, ExecutionError> {
        match machine.summary.connector {
            ConnectorKind::MachineDaemon => {
                self.daemon.execute(machine, request_id, operation).await
            }
            ConnectorKind::Sandbox => match &self.sandbox {
                Some(sandbox) => sandbox.execute(machine, request_id, operation).await,
                None => Err(execution_error(
                    ExecutionErrorCode::Unsupported,
                    "sandbox connector is not installed in this gateway",
                    false,
                )),
            },
            ConnectorKind::E2b | ConnectorKind::Ssh => Err(execution_error(
                ExecutionErrorCode::Unsupported,
                "connector is not installed in this gateway",
                false,
            )),
        }
    }

    pub async fn cancel(&self, machine: &MachineRecord, request_id: String) {
        match machine.summary.connector {
            ConnectorKind::MachineDaemon => self.daemon.cancel(machine, request_id).await,
            ConnectorKind::Sandbox => {
                if let Some(sandbox) = &self.sandbox {
                    sandbox.cancel(machine, request_id).await;
                }
            }
            ConnectorKind::E2b | ConnectorKind::Ssh => {}
        }
    }
}
