use std::{collections::BTreeMap, sync::Arc};

use base64::{Engine, engine::general_purpose::STANDARD};
use execution_contracts::{ExecutionError, ExecutionErrorCode};
use futures_util::StreamExt;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::{
    RemoteProcessEvent, RemoteProcessRequest, RemoteStreamKind, TensorlakeRuntimeConfig,
    TensorlakeTransport,
    error::{execution_error, transport_execution_error},
};

const INLINE_RUNNER: &str = include_str!("inline_runner.py");
const MAX_INLINE_ARGUMENT_BYTES: usize = 1024 * 1024;

pub(crate) struct InlineRunner {
    transport: Arc<dyn TensorlakeTransport>,
    python_command: String,
    state_directory: String,
    roots: Value,
    grants: Value,
}

impl InlineRunner {
    pub fn new(
        transport: Arc<dyn TensorlakeTransport>,
        python_command: String,
        config: &TensorlakeRuntimeConfig,
    ) -> Self {
        Self {
            transport,
            python_command,
            state_directory: config.state_directory.clone(),
            roots: json!(
                config
                    .workspace_roots
                    .iter()
                    .map(|root| json!({
                        "id": root.id.as_str(), "path": root.path, "read_only": root.read_only,
                    }))
                    .collect::<Vec<_>>()
            ),
            grants: json!(
                config
                    .native_grants
                    .iter()
                    .map(|grant| json!({
                        "id": grant.id.as_str(), "path": grant.path, "read_only": grant.read_only,
                    }))
                    .collect::<Vec<_>>()
            ),
        }
    }

    pub async fn call<Request, Response>(
        &self,
        operation: &str,
        request: &Request,
    ) -> Result<Response, ExecutionError>
    where
        Request: Serialize + Sync,
        Response: DeserializeOwned,
    {
        let envelope = json!({
            "operation": operation,
            "request": request,
            "state_directory": self.state_directory,
            "roots": self.roots,
            "grants": self.grants,
        });
        let argument = encode_inline_argument(&envelope)?;
        if argument.len() > MAX_INLINE_ARGUMENT_BYTES {
            return Err(execution_error(
                ExecutionErrorCode::ResourceExhausted,
                "inline Tensorlake operation exceeds the safe process argument limit; use artifact-backed content",
            ));
        }
        let mut stream = self
            .transport
            .start_process(RemoteProcessRequest {
                command: self.python_command.clone(),
                arguments: vec![
                    "-u".to_owned(),
                    "-c".to_owned(),
                    INLINE_RUNNER.to_owned(),
                    argument,
                ],
                environment: BTreeMap::new(),
                cwd: None,
                tag: Some(format!("agent-pane-operation:{operation}")),
                stdin: false,
                pty: None,
                timeout_ms: None,
            })
            .await
            .map_err(transport_execution_error)?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_code = None;
        while let Some(event) = stream.next().await {
            match event.map_err(transport_execution_error)? {
                RemoteProcessEvent::Output {
                    stream: RemoteStreamKind::Stdout,
                    data,
                } => stdout.extend(data),
                RemoteProcessEvent::Output {
                    stream: RemoteStreamKind::Stderr,
                    data,
                } => stderr.extend(data),
                RemoteProcessEvent::Exited {
                    exit_code: code, ..
                } => exit_code = Some(code),
                RemoteProcessEvent::Started { .. }
                | RemoteProcessEvent::KeepAlive
                | RemoteProcessEvent::Output {
                    stream: RemoteStreamKind::Pty,
                    ..
                } => {}
            }
        }
        if exit_code != Some(0) {
            return Err(execution_error(
                ExecutionErrorCode::Internal,
                format!(
                    "inline Tensorlake Python operation exited with {:?}: {}",
                    exit_code,
                    String::from_utf8_lossy(&stderr)
                ),
            ));
        }
        let envelope: RunnerResponse = serde_json::from_slice(&stdout).map_err(|source| {
            execution_error(
                ExecutionErrorCode::Internal,
                format!(
                    "invalid inline Tensorlake response: {source}; stderr={}",
                    String::from_utf8_lossy(&stderr)
                ),
            )
        })?;
        if envelope.ok {
            serde_json::from_value(envelope.value).map_err(|source| {
                execution_error(
                    ExecutionErrorCode::Internal,
                    format!("inline Tensorlake result does not match its contract: {source}"),
                )
            })
        } else {
            Err(envelope.error.unwrap_or_else(|| {
                execution_error(
                    ExecutionErrorCode::Internal,
                    "failed inline Tensorlake response omitted its error",
                )
            }))
        }
    }
}

#[derive(serde::Deserialize)]
struct RunnerResponse {
    ok: bool,
    #[serde(default)]
    value: Value,
    error: Option<ExecutionError>,
}

pub(crate) fn encode_inline_argument(value: &Value) -> Result<String, ExecutionError> {
    let serialized = serde_json::to_vec(value)
        .map_err(|source| execution_error(ExecutionErrorCode::Internal, source.to_string()))?;
    Ok(STANDARD.encode(compress_zlib(&serialized)))
}

fn compress_zlib(bytes: &[u8]) -> Vec<u8> {
    // Keep the connector dependency-light: Python's zlib stream format for a
    // stored DEFLATE block is straightforward to produce. Payloads larger than
    // one block fall back to several stored blocks.
    let mut output = vec![0x78, 0x01];
    let mut remaining = bytes;
    while !remaining.is_empty() {
        let take = remaining.len().min(u16::MAX as usize);
        let final_block = take == remaining.len();
        output.push(u8::from(final_block));
        let length = take as u16;
        output.extend_from_slice(&length.to_le_bytes());
        output.extend_from_slice(&(!length).to_le_bytes());
        output.extend_from_slice(&remaining[..take]);
        remaining = &remaining[take..];
    }
    if bytes.is_empty() {
        output.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    output.extend_from_slice(&adler32(bytes).to_be_bytes());
    output
}

fn adler32(bytes: &[u8]) -> u32 {
    const MODULUS: u32 = 65_521;
    let mut a = 1_u32;
    let mut b = 0_u32;
    for byte in bytes {
        a = (a + u32::from(*byte)) % MODULUS;
        b = (b + a) % MODULUS;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_zlib_has_expected_header_and_checksum() {
        let bytes = b"execution-tensorlake";
        let compressed = compress_zlib(bytes);
        assert_eq!(&compressed[..2], &[0x78, 0x01]);
        assert_eq!(
            &compressed[compressed.len() - 4..],
            &adler32(bytes).to_be_bytes()
        );
    }
}
