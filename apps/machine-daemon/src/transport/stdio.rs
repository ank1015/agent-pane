use std::sync::Arc;

use anyhow::Context;
use execution_local::LocalExecutionRuntime;
use execution_runtime::ExecutionRuntime;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};

use crate::{
    dispatch::Dispatcher,
    protocol::{ClientMessage, ServerMessage},
    session::{self, protocol_error},
};

pub async fn run(runtime: Arc<LocalExecutionRuntime>) -> anyhow::Result<()> {
    let (incoming_tx, incoming_rx) = mpsc::channel(64);
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(64);
    let descriptor = runtime.descriptor().clone();
    let dispatcher = Dispatcher::new(runtime);
    let session_output = outgoing_tx.clone();
    let session_task = tokio::spawn(session::run(
        descriptor,
        dispatcher,
        incoming_rx,
        session_output,
    ));
    let reader_output = outgoing_tx.clone();
    drop(outgoing_tx);

    let reader_task = tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ClientMessage>(&line) {
                Ok(message) => {
                    if incoming_tx.send(message).await.is_err() {
                        break;
                    }
                }
                Err(source) => {
                    if reader_output
                        .send(ServerMessage::Error {
                            request_id: None,
                            error: protocol_error(format!("invalid JSON message: {source}")),
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
        Ok::<(), std::io::Error>(())
    });

    let mut stdout = tokio::io::stdout();
    while let Some(message) = outgoing_rx.recv().await {
        let mut bytes = serde_json::to_vec(&message).context("serialize daemon response")?;
        bytes.push(b'\n');
        stdout.write_all(&bytes).await?;
        stdout.flush().await?;
    }
    reader_task.await.context("stdio reader task failed")??;
    session_task.await.context("stdio session task failed")?;
    Ok(())
}
