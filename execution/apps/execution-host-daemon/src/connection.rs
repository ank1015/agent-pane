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
    let heartbeat_interval = match serde_json::from_str::<GatewayHostMessage>(welcome.as_str())
        .context("decode gateway welcome")?
    {
        GatewayHostMessage::Welcome {
            heartbeat_interval_ms,
        } => {
            anyhow::ensure!(
                heartbeat_interval_ms > 0,
                "gateway returned an invalid zero heartbeat interval"
            );
            Duration::from_millis(heartbeat_interval_ms)
        }
        GatewayHostMessage::Disconnect { code, message } => {
            anyhow::bail!("gateway rejected connection ({code}): {message}");
        }
        GatewayHostMessage::Request { .. } => {
            anyhow::bail!("gateway sent an execution request before welcome");
        }
    };
    tracing::info!(host_id = %credential.host_id, "connected to Execution Gateway");

    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<Outgoing>(128);
    let heartbeat_timeout = heartbeat_interval.saturating_mul(3);
    let heartbeat_deadline = tokio::time::sleep(heartbeat_timeout);
    tokio::pin!(heartbeat_deadline);
    loop {
        tokio::select! {
        _ = &mut heartbeat_deadline => {
            tracing::warn!(
                host_id = %credential.host_id,
                heartbeat_timeout_ms = heartbeat_timeout.as_millis(),
                "gateway heartbeat timed out; reconnecting"
            );
            break;
        }
        outgoing = outgoing_rx.recv() => {
            let Some(outgoing) = outgoing else { break; };
            match outgoing {
                Outgoing::Protocol(message) => {
                    socket.send(Message::Text(serde_json::to_string(&message)?.into())).await?;
                }
            }
        }
        incoming = socket.next() => {
        let Some(incoming) = incoming else { break; };
        heartbeat_deadline
            .as_mut()
            .reset(tokio::time::Instant::now() + heartbeat_timeout);
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
                if socket.send(Message::Pong(data)).await.is_err() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use execution_core::{ExecutionHostId, RootId};
    use execution_supervisor_core::{SupervisorConfig, SupervisorLimits, SupervisorRoot};
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    #[tokio::test]
    async fn stale_connection_times_out_and_returns_for_reconnect() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake gateway");
        let address = listener.local_addr().expect("fake gateway address");
        let gateway = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept daemon");
            let mut socket = accept_async(stream).await.expect("accept WebSocket");
            let hello = socket
                .next()
                .await
                .expect("daemon hello")
                .expect("read daemon hello");
            let Message::Text(hello) = hello else {
                panic!("daemon did not send a JSON hello");
            };
            assert!(matches!(
                serde_json::from_str::<HostDaemonMessage>(hello.as_str())
                    .expect("decode daemon hello"),
                HostDaemonMessage::Hello { .. }
            ));
            socket
                .send(Message::Text(
                    serde_json::to_string(&GatewayHostMessage::Welcome {
                        heartbeat_interval_ms: 50,
                    })
                    .expect("encode welcome")
                    .into(),
                ))
                .await
                .expect("send welcome");

            // Prove that inbound activity extends the deadline before the
            // gateway becomes silent while leaving the TCP connection open.
            tokio::time::sleep(Duration::from_millis(100)).await;
            socket
                .send(Message::Ping(Vec::new().into()))
                .await
                .expect("send heartbeat");
            tokio::time::sleep(Duration::from_secs(1)).await;
        });

        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        tokio::fs::create_dir_all(&root)
            .await
            .expect("create workspace");
        let host_id = Uuid::now_v7();
        let runtime = Arc::new(
            SupervisorRuntime::new(SupervisorConfig {
                host_id: ExecutionHostId::new(host_id.to_string()).expect("host ID"),
                state_directory: temporary.path().join("state"),
                roots: vec![SupervisorRoot {
                    id: RootId::new("workspace").expect("root ID"),
                    name: "Workspace".to_owned(),
                    path: root,
                    read_only: false,
                }],
                limits: SupervisorLimits::default(),
            })
            .await
            .expect("start supervisor"),
        );
        let credential = HostCredential {
            host_id,
            credential: "test-credential".to_owned(),
            gateway_url: format!("http://{address}"),
            websocket_url: format!("ws://{address}/connect"),
        };

        let started = tokio::time::Instant::now();
        let outcome = tokio::time::timeout(
            Duration::from_millis(750),
            connect_once(Arc::clone(&runtime), &credential, Uuid::now_v7()),
        )
        .await
        .expect("stale connection did not time out")
        .expect("daemon connection failed");
        assert!(matches!(outcome, ConnectionEnd::Reconnect));
        assert!(
            started.elapsed() >= Duration::from_millis(225),
            "inbound heartbeat did not extend the liveness deadline"
        );

        gateway.abort();
        runtime.shutdown().await;
    }
}
