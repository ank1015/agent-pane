use std::sync::Arc;

use async_trait::async_trait;
use execution_contracts::{ExecutionError, ExecutionErrorCode};
use execution_protocol::{ConnectorKind, Operation, ServerMessage};
use tokio::sync::mpsc;

use crate::{
    connection_registry::{ConnectionRegistry, execution_error},
    db::MachineRecord,
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
}

impl ConnectorRouter {
    pub fn new(connections: ConnectionRegistry) -> Self {
        Self {
            daemon: Arc::new(DaemonConnector::new(connections)),
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
            ConnectorKind::E2b | ConnectorKind::Ssh => Err(execution_error(
                ExecutionErrorCode::Unsupported,
                "connector is not installed in this gateway",
                false,
            )),
        }
    }

    pub async fn cancel(&self, machine: &MachineRecord, request_id: String) {
        if machine.summary.connector == ConnectorKind::MachineDaemon {
            self.daemon.cancel(machine, request_id).await;
        }
    }
}
