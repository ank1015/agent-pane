use std::{sync::Arc, time::Duration};

use anyhow::Context as _;
use execution_api::{GatewayHostMessage, HostDaemonMessage};
use execution_core::{ExecutionRuntime as _, OperationContext};
use execution_supervisor_core::SupervisorRuntime;
use execution_wire::{PROTOCOL_NAME, PROTOCOL_VERSION, dispatch_request};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::header::AUTHORIZATION},
};
use uuid::Uuid;

use crate::credential::HostCredential;

enum Outgoing {
    Protocol(HostDaemonMessage),
    Pong(Vec<u8>),
}

enum ConnectionEnd {
    Reconnect,
    Stop,
}

pub async fn run(
    runtime: Arc<SupervisorRuntime>,
    credential: HostCredential,
) -> anyhow::Result<()> {
    let daemon_instance_id = Uuid::now_v7();
    let mut delay = Duration::from_secs(1);
    loop {
        match connect_once(Arc::clone(&runtime), &credential, daemon_instance_id).await {
            Ok(ConnectionEnd::Reconnect) => {
                tracing::warn!(host_id = %credential.host_id, "gateway connection closed");
                delay = Duration::from_secs(1);
            }
            Ok(ConnectionEnd::Stop) => return Ok(()),
            Err(error) => {
                tracing::warn!(host_id = %credential.host_id, %error, "gateway connection failed");
            }
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

async fn connect_once(
    runtime: Arc<SupervisorRuntime>,
    credential: &HostCredential,
    daemon_instance_id: Uuid,
) -> anyhow::Result<ConnectionEnd> {
    let mut request = credential
        .websocket_url
        .as_str()
        .into_client_request()
        .context("invalid Host Daemon WebSocket URL")?;
    request.headers_mut().insert(
        AUTHORIZATION,
        format!("Bearer {}", credential.credential)
            .parse()
            .context("invalid Host Daemon credential")?,
    );
    let (mut socket, _) = connect_async(request)
        .await
        .context("connect to Execution Gateway")?;
    socket
        .send(Message::Text(
            serde_json::to_string(&HostDaemonMessage::Hello {
                protocol_name: PROTOCOL_NAME.to_owned(),
                protocol_version: PROTOCOL_VERSION,
                daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
                daemon_instance_id,
                descriptor: Box::new(runtime.descriptor().clone()),
            })?
            .into(),
        ))
        .await
        .context("send Host Daemon hello")?;

    let welcome = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .context("gateway handshake timed out")?
        .context("gateway disconnected during handshake")??;
    let Message::Text(welcome) = welcome else {
        anyhow::bail!("gateway did not return a JSON welcome message");
    };
    match serde_json::from_str::<GatewayHostMessage>(welcome.as_str())
        .context("decode gateway welcome")?
    {
        GatewayHostMessage::Welcome { .. } => {}
        GatewayHostMessage::Disconnect { code, message } => {
            anyhow::bail!("gateway rejected connection ({code}): {message}");
        }
        GatewayHostMessage::Request { .. } => {
            anyhow::bail!("gateway sent an execution request before welcome");
        }
    }
    tracing::info!(host_id = %credential.host_id, "connected to Execution Gateway");

    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<Outgoing>(128);
    loop {
        tokio::select! {
        outgoing = outgoing_rx.recv() => {
            let Some(outgoing) = outgoing else { break; };
            match outgoing {
                Outgoing::Protocol(message) => {
                    socket.send(Message::Text(serde_json::to_string(&message)?.into())).await?;
                }
                Outgoing::Pong(data) => socket.send(Message::Pong(data.into())).await?,
            }
        }
        incoming = socket.next() => {
        let Some(incoming) = incoming else { break; };
        match incoming.context("read gateway WebSocket message")? {
            Message::Text(text) => match serde_json::from_str::<GatewayHostMessage>(text.as_str())
                .context("decode gateway message")?
            {
                GatewayHostMessage::Request { request } => {
                    let runtime = Arc::clone(&runtime);
                    let outgoing = outgoing_tx.clone();
                    tokio::spawn(async move {
                        let context = OperationContext::with_timeout(Duration::from_secs(60));
                        let response = dispatch_request(runtime.as_ref(), &context, *request).await;
                        let _ = outgoing
                            .send(Outgoing::Protocol(HostDaemonMessage::Response {
                                response: Box::new(response),
                            }))
                            .await;
                    });
                }
                GatewayHostMessage::Disconnect { code, message } => {
                    tracing::warn!(%code, %message, "gateway closed Host Daemon session");
                    if matches!(
                        code.as_str(),
                        "HOST_REGISTRATION_DELETED"
                            | "HOST_CREDENTIAL_ROTATED"
                            | "CONNECTION_REPLACED"
                    ) {
                        return Ok(ConnectionEnd::Stop);
                    }
                    break;
                }
                GatewayHostMessage::Welcome { .. } => {
                    anyhow::bail!("gateway sent a duplicate welcome message");
                }
            },
            Message::Ping(data) => {
                if outgoing_tx
                    .send(Outgoing::Pong(data.to_vec()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Message::Close(_) => break,
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
        }
        }
    }
    Ok(ConnectionEnd::Reconnect)
}
