use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use execution_api::GatewayHostMessage;
use execution_wire::{RequestEnvelope, ResponseEnvelope};
use thiserror::Error;
use tokio::sync::{Semaphore, mpsc, oneshot};
use uuid::Uuid;

#[derive(Clone)]
pub struct HostConnections {
    inner: Arc<Mutex<HashMap<Uuid, ActiveConnection>>>,
    operation_timeout: Duration,
    max_in_flight: usize,
}

#[derive(Clone)]
struct ActiveConnection {
    connection_id: Uuid,
    outgoing: mpsc::Sender<ConnectionOutput>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<ResponseEnvelope>>>>,
    permits: Arc<Semaphore>,
}

#[derive(Debug)]
pub enum ConnectionOutput {
    Message(GatewayHostMessage),
}

#[derive(Debug, Error)]
pub enum HostConnectionError {
    #[error("the Host Daemon is not connected")]
    Offline,
    #[error("the request ID is already in flight for this host")]
    DuplicateRequest,
    #[error("the Host Daemon has too many operations in flight")]
    Busy,
    #[error("the Host Daemon connection ended before the operation completed")]
    Disconnected,
    #[error("the Host Daemon operation timed out")]
    Timeout,
}

impl HostConnections {
    #[must_use]
    pub fn new(operation_timeout: Duration, max_in_flight: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            operation_timeout,
            max_in_flight,
        }
    }

    pub fn register(
        &self,
        host_id: Uuid,
        connection_id: Uuid,
        outgoing: mpsc::Sender<ConnectionOutput>,
    ) {
        let active = ActiveConnection {
            connection_id,
            outgoing,
            pending: Arc::new(Mutex::new(HashMap::new())),
            permits: Arc::new(Semaphore::new(self.max_in_flight)),
        };
        let previous = self
            .inner
            .lock()
            .expect("host connection registry poisoned")
            .insert(host_id, active);
        if let Some(previous) = previous {
            previous.fail_pending();
            let _ = previous.outgoing.try_send(ConnectionOutput::Message(
                GatewayHostMessage::Disconnect {
                    code: "CONNECTION_REPLACED".to_owned(),
                    message: "a newer authenticated connection replaced this one".to_owned(),
                },
            ));
        }
    }

    pub fn unregister(&self, host_id: Uuid, connection_id: Uuid) -> bool {
        let removed = {
            let mut connections = self
                .inner
                .lock()
                .expect("host connection registry poisoned");
            if connections
                .get(&host_id)
                .is_some_and(|active| active.connection_id == connection_id)
            {
                connections.remove(&host_id)
            } else {
                None
            }
        };
        if let Some(active) = removed {
            active.fail_pending();
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn is_connected(&self, host_id: Uuid) -> bool {
        self.inner
            .lock()
            .expect("host connection registry poisoned")
            .contains_key(&host_id)
    }

    pub fn disconnect(&self, host_id: Uuid, code: &str, message: &str) {
        let removed = self
            .inner
            .lock()
            .expect("host connection registry poisoned")
            .remove(&host_id);
        if let Some(active) = removed {
            active.fail_pending();
            let _ = active.outgoing.try_send(ConnectionOutput::Message(
                GatewayHostMessage::Disconnect {
                    code: code.to_owned(),
                    message: message.to_owned(),
                },
            ));
        }
    }

    pub async fn execute(
        &self,
        host_id: Uuid,
        request: RequestEnvelope,
    ) -> Result<ResponseEnvelope, HostConnectionError> {
        let active = self
            .inner
            .lock()
            .expect("host connection registry poisoned")
            .get(&host_id)
            .cloned()
            .ok_or(HostConnectionError::Offline)?;
        let _permit = active
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| HostConnectionError::Busy)?;
        let request_id = request.request_id.as_str().to_owned();
        let (response_tx, response_rx) = oneshot::channel();
        {
            let mut pending = active
                .pending
                .lock()
                .expect("pending response map poisoned");
            if pending.contains_key(&request_id) {
                return Err(HostConnectionError::DuplicateRequest);
            }
            pending.insert(request_id.clone(), response_tx);
        }
        if let Err(error) =
            active
                .outgoing
                .try_send(ConnectionOutput::Message(GatewayHostMessage::Request {
                    request: Box::new(request),
                }))
        {
            active.remove_pending(&request_id);
            return Err(match error {
                mpsc::error::TrySendError::Full(_) => HostConnectionError::Busy,
                mpsc::error::TrySendError::Closed(_) => HostConnectionError::Disconnected,
            });
        }
        match tokio::time::timeout(self.operation_timeout, response_rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(HostConnectionError::Disconnected),
            Err(_) => {
                active.remove_pending(&request_id);
                Err(HostConnectionError::Timeout)
            }
        }
    }

    pub fn complete(&self, host_id: Uuid, connection_id: Uuid, response: ResponseEnvelope) {
        let active = self
            .inner
            .lock()
            .expect("host connection registry poisoned")
            .get(&host_id)
            .filter(|active| active.connection_id == connection_id)
            .cloned();
        let Some(active) = active else {
            return;
        };
        let request_id = response.request_id().as_str().to_owned();
        let sender = active
            .pending
            .lock()
            .expect("pending response map poisoned")
            .remove(&request_id);
        if let Some(sender) = sender {
            let _ = sender.send(response);
        }
    }
}

impl ActiveConnection {
    fn remove_pending(&self, request_id: &str) {
        self.pending
            .lock()
            .expect("pending response map poisoned")
            .remove(request_id);
    }

    fn fail_pending(&self) {
        self.pending
            .lock()
            .expect("pending response map poisoned")
            .clear();
    }
}

#[cfg(test)]
mod tests {
    use execution_wire::{Operation, OperationResult, RequestId};

    use super::*;

    #[tokio::test]
    async fn correlates_responses_and_fails_pending_work_on_disconnect() {
        let registry = HostConnections::new(Duration::from_secs(1), 4);
        let host_id = Uuid::now_v7();
        let connection_id = Uuid::now_v7();
        let (tx, mut rx) = mpsc::channel(4);
        registry.register(host_id, connection_id, tx);

        let request = RequestEnvelope::new(RequestId::generate(), Operation::Describe);
        let request_id = request.request_id.clone();
        let executing = {
            let registry = registry.clone();
            tokio::spawn(async move { registry.execute(host_id, request).await })
        };
        let sent = rx.recv().await.expect("outgoing request");
        assert!(matches!(
            sent,
            ConnectionOutput::Message(GatewayHostMessage::Request { .. })
        ));
        registry.complete(
            host_id,
            connection_id,
            ResponseEnvelope::success(request_id, OperationResult::Unit),
        );
        assert!(executing.await.unwrap().is_ok());

        let request = RequestEnvelope::new(RequestId::generate(), Operation::Describe);
        let executing = {
            let registry = registry.clone();
            tokio::spawn(async move { registry.execute(host_id, request).await })
        };
        let _ = rx.recv().await;
        assert!(registry.unregister(host_id, connection_id));
        assert!(matches!(
            executing.await.unwrap(),
            Err(HostConnectionError::Disconnected)
        ));
    }
}
