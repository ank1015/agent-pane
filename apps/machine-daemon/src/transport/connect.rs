use std::{sync::Arc, time::Duration};

use anyhow::Context;
use execution_local::LocalExecutionRuntime;
use execution_runtime::ExecutionRuntime;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::header::AUTHORIZATION},
};

use crate::{
    dispatch::Dispatcher,
    pairing,
    protocol::{ClientMessage, ServerMessage},
    session::{self, protocol_error},
};

pub async fn run(
    runtime: Arc<LocalExecutionRuntime>,
    gateway: String,
    token: Option<String>,
) -> anyhow::Result<()> {
    let mut delay = Duration::from_secs(1);
    loop {
        match connect_once(Arc::clone(&runtime), &gateway, token.as_deref()).await {
            Ok(()) => tracing::warn!(%gateway, "gateway connection closed"),
            Err(source) => tracing::warn!(%gateway, %source, "gateway connection failed"),
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

async fn connect_once(
    runtime: Arc<LocalExecutionRuntime>,
    gateway: &str,
    token: Option<&str>,
) -> anyhow::Result<()> {
    let mut request = gateway
        .into_client_request()
        .context("invalid gateway WebSocket URL")?;
    if let Some(value) = pairing::authorization_value(token) {
        request.headers_mut().insert(
            AUTHORIZATION,
            value.parse().context("invalid authorization header")?,
        );
    }
    request.headers_mut().insert(
        "x-machine-id",
        runtime
            .descriptor()
            .machine_id
            .as_str()
            .parse()
            .context("invalid machine identifier header")?,
    );
    let (socket, _) = connect_async(request)
        .await
        .context("connect to gateway WebSocket")?;
    tracing::info!(%gateway, "connected to machine gateway");
    let (mut socket_writer, mut socket_reader) = socket.split();
    let (incoming_tx, incoming_rx) = mpsc::channel(64);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(64);
    let dispatcher = Dispatcher::new(Arc::clone(&runtime));
    let session_task = tokio::spawn(session::run(
        runtime.descriptor().clone(),
        dispatcher,
        incoming_rx,
        outgoing_tx.clone(),
    ));
    let parse_errors = outgoing_tx.clone();
    drop(outgoing_tx);

    let reader_task = tokio::spawn(async move {
        while let Some(message) = socket_reader.next().await {
            match message {
                Ok(Message::Text(text)) => match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(message) => {
                        if incoming_tx.send(message).await.is_err() {
                            break;
                        }
                    }
                    Err(source) => {
                        let _ = parse_errors
                            .send(ServerMessage::Error {
                                request_id: None,
                                error: protocol_error(format!("invalid JSON message: {source}")),
                            })
                            .await;
                    }
                },
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(
                    Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_),
                ) => {}
            }
        }
    });
    let writer_task = tokio::spawn(async move {
        while let Some(message) = outgoing_rx.recv().await {
            let json = serde_json::to_string(&message).context("serialize daemon message")?;
            socket_writer
                .send(Message::Text(json.into()))
                .await
                .context("send daemon message")?;
        }
        Ok::<(), anyhow::Error>(())
    });

    tokio::select! {
        result = reader_task => result.context("gateway reader task failed")?,
        result = writer_task => result.context("gateway writer task failed")??,
        result = session_task => result.context("gateway session task failed")?,
    }
    Ok(())
}
