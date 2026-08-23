use std::{net::SocketAddr, sync::Arc};

use axum::{
    Json, Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use execution_contracts::EnvironmentDescriptor;
use execution_local::LocalExecutionEnvironment;
use execution_runtime::ExecutionEnvironment;
use serde::Serialize;
use tokio::{net::TcpListener, sync::mpsc};

use crate::{
    dispatch::Dispatcher,
    pairing,
    protocol::{ClientMessage, ServerMessage},
    session::{self, protocol_error},
};

#[derive(Clone)]
struct AppState {
    environment: Arc<LocalExecutionEnvironment>,
    token: Option<String>,
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

pub async fn run(
    environment: Arc<LocalExecutionEnvironment>,
    listen: SocketAddr,
    token: Option<String>,
) -> anyhow::Result<()> {
    let state = AppState { environment, token };
    let router = Router::new()
        .route("/health", get(health))
        .route("/v1/descriptor", get(descriptor))
        .route("/v1/ws", get(websocket))
        .with_state(state);
    let listener = TcpListener::bind(listen).await?;
    tracing::info!(%listen, "machine daemon listening");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

async fn descriptor(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !pairing::authorized(&headers, state.token.as_deref()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(state.environment.descriptor().clone()).into_response()
}

async fn websocket(
    State(state): State<AppState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !pairing::authorized(&headers, state.token.as_deref()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    upgrade
        .on_upgrade(move |socket| run_socket(socket, state.environment))
        .into_response()
}

async fn run_socket(socket: WebSocket, environment: Arc<LocalExecutionEnvironment>) {
    use futures_util::{SinkExt, StreamExt};

    let (mut socket_writer, mut socket_reader) = socket.split();
    let (incoming_tx, incoming_rx) = mpsc::channel(64);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(64);
    let descriptor: EnvironmentDescriptor = environment.descriptor().clone();
    let dispatcher = Dispatcher::new(environment);
    let session_task = tokio::spawn(session::run(
        descriptor,
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
                Ok(Message::Binary(_) | Message::Ping(_) | Message::Pong(_)) => {}
            }
        }
    });

    let writer_task = tokio::spawn(async move {
        while let Some(message) = outgoing_rx.recv().await {
            let Ok(json) = serde_json::to_string(&message) else {
                break;
            };
            if socket_writer
                .send(Message::Text(json.into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    tokio::select! {
        _ = reader_task => {}
        _ = writer_task => {}
        _ = session_task => {}
    }
}

async fn shutdown_signal() {
    if let Err(source) = tokio::signal::ctrl_c().await {
        tracing::error!(%source, "could not listen for shutdown signal");
    }
}
