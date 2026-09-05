use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use execution_core::ExecutionHostDescriptor;
use execution_wire::{
    NdjsonCodec, Operation, OperationResult, PROTOCOL_VERSION, RequestEnvelope, RequestId,
    ResponseEnvelope,
};
use tokio::{
    io::BufReader,
    net::{UnixStream, unix::OwnedReadHalf},
};

/// Sends one request to a running supervisor and validates response correlation.
pub async fn call(socket_path: &Path, request: &RequestEnvelope) -> Result<ResponseEnvelope> {
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connect to supervisor socket {}", socket_path.display()))?;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let codec = NdjsonCodec::default();

    codec
        .write_request(&mut write_half, request)
        .await
        .context("write supervisor request")?;
    let response = read_response(&codec, &mut reader).await?;
    if response.request_id() != &request.request_id {
        bail!(
            "supervisor response request ID {} did not match {}",
            response.request_id(),
            request.request_id
        );
    }
    if response.version() != PROTOCOL_VERSION {
        bail!(
            "supervisor response used protocol version {}; expected {}",
            response.version(),
            PROTOCOL_VERSION
        );
    }
    Ok(response)
}

/// Returns the descriptor published by a running supervisor.
pub async fn probe(socket_path: &Path) -> Result<ExecutionHostDescriptor> {
    let response = call(
        socket_path,
        &RequestEnvelope::new(RequestId::generate(), Operation::Describe),
    )
    .await?;
    match response {
        ResponseEnvelope::Success {
            result: OperationResult::HostDescriptor(descriptor),
            ..
        } => Ok(descriptor),
        ResponseEnvelope::Success { result, .. } => {
            Err(anyhow!("supervisor describe returned {result:?}"))
        }
        ResponseEnvelope::Error { error, .. } => {
            Err(anyhow!("supervisor describe failed: {error}"))
        }
    }
}

/// Relays exactly one request from stdin to a supervisor and writes its response to stdout.
pub async fn relay_stdio(socket_path: &Path) -> Result<()> {
    let codec = NdjsonCodec::default();
    let mut stdin = BufReader::new(tokio::io::stdin());
    let request = codec
        .read_request(&mut stdin)
        .await
        .context("read supervisor request from stdin")?
        .context("stdin ended before an execution request was received")?;
    let response = call(socket_path, &request).await?;
    let mut stdout = tokio::io::stdout();
    codec
        .write_response(&mut stdout, &response)
        .await
        .context("write supervisor response to stdout")?;
    Ok(())
}

async fn read_response(
    codec: &NdjsonCodec,
    reader: &mut BufReader<OwnedReadHalf>,
) -> Result<ResponseEnvelope> {
    codec
        .read_response(reader)
        .await
        .context("read supervisor response")?
        .context("supervisor closed the connection before sending a response")
}
