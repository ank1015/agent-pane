use execution_contracts::ExecutionErrorCode;
use execution_protocol::ServerMessage;
use uuid::Uuid;

use crate::{
    connection_registry::execution_error,
    connectors::ConnectorRouter,
    db::{Database, MachineRecord},
};

#[derive(Clone)]
pub struct OperationRouter {
    database: Database,
    connectors: ConnectorRouter,
}

impl OperationRouter {
    pub fn new(database: Database, connectors: ConnectorRouter) -> Self {
        Self {
            database,
            connectors,
        }
    }
    pub fn spawn(
        &self,
        id: Uuid,
        machine: MachineRecord,
        operation: Box<execution_protocol::Operation>,
    ) {
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(source) = this.run(id, machine, operation).await {
                tracing::error!(operation_id=%id, %source, "operation routing failed");
            }
        });
    }
    async fn run(
        &self,
        id: Uuid,
        machine: MachineRecord,
        operation: Box<execution_protocol::Operation>,
    ) -> Result<(), crate::db::DbError> {
        if !self.database.set_running(id).await? {
            return Ok(());
        }
        let mut receiver = match self
            .connectors
            .execute(&machine, id.to_string(), operation)
            .await
        {
            Ok(receiver) => receiver,
            Err(error) => {
                self.database.fail(id, &error).await?;
                return Ok(());
            }
        };
        let mut sequence = 0_u64;
        while let Some(message) = receiver.recv().await {
            match message {
                ServerMessage::Response { response, .. } => {
                    self.database.complete(id, Some(&response)).await?;
                    return Ok(());
                }
                ServerMessage::StreamItem { item, .. } => {
                    sequence += 1;
                    self.database.add_event(id, sequence, &item).await?;
                }
                ServerMessage::StreamEnd { .. } => {
                    self.database.complete(id, None).await?;
                    return Ok(());
                }
                ServerMessage::Error { error, .. } => {
                    self.database.fail(id, &error).await?;
                    return Ok(());
                }
                ServerMessage::Ready { .. } => {}
            }
        }
        self.database
            .fail(
                id,
                &execution_error(
                    ExecutionErrorCode::Disconnected,
                    "machine disconnected before completing the operation",
                    true,
                ),
            )
            .await?;
        Ok(())
    }

    pub async fn cancel(&self, id: Uuid, machine: &MachineRecord) {
        self.connectors.cancel(machine, id.to_string()).await;
    }
}
