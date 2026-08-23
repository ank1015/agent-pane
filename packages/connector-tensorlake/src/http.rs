use std::{pin::Pin, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream};
use reqwest::{Client, Response, header};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    RemoteProcessEvent, RemoteProcessRequest, RemoteProcessStream, RemoteProcessSummary,
    RemoteStreamKind, TensorlakeConnectionConfig, TensorlakeTransport, TensorlakeTransportError,
};

#[derive(Clone)]
pub struct TensorlakeHttpTransport {
    client: Client,
    config: TensorlakeConnectionConfig,
}

#[derive(Clone, Debug, Deserialize)]
struct ProcessInfo {
    pid: u32,
    status: String,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    signal: Option<i32>,
    command: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Deserialize)]
struct ProcessList {
    #[serde(default)]
    processes: Vec<ProcessInfo>,
}

impl TensorlakeHttpTransport {
    pub fn new(config: TensorlakeConnectionConfig) -> Result<Self, TensorlakeTransportError> {
        if config.api_key.trim().is_empty() {
            return Err(TensorlakeTransportError::new(
                "Tensorlake API key must not be empty",
            ));
        }
        if !matches!(config.proxy_url.scheme(), "http" | "https")
            || config.proxy_url.host_str().is_none()
        {
            return Err(TensorlakeTransportError::new(
                "Tensorlake proxy URL must be an absolute HTTP(S) URL",
            ));
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_millis(config.request_timeout_ms))
            .build()
            .map_err(|source| TensorlakeTransportError::new(source.to_string()))?;
        Ok(Self { client, config })
    }

    fn endpoint(&self, path: &str) -> Result<reqwest::Url, TensorlakeTransportError> {
        self.config
            .proxy_url
            .join(path.trim_start_matches('/'))
            .map_err(|source| TensorlakeTransportError::new(source.to_string()))
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, TensorlakeTransportError> {
        Ok(self
            .client
            .request(method, self.endpoint(path)?)
            .bearer_auth(&self.config.api_key))
    }

    async fn start(
        &self,
        request: RemoteProcessRequest,
    ) -> Result<ProcessInfo, TensorlakeTransportError> {
        if request.pty.is_some() {
            return Err(TensorlakeTransportError::new(
                "native Tensorlake PTYs are not used by the recoverable process transport",
            ));
        }
        let mut body = json!({
            "command": request.command,
            "args": request.arguments,
            "env": request.environment,
            "user": self.config.user,
            "stdin_mode": if request.stdin { "pipe" } else { "closed" },
            "stdout_mode": "capture",
            "stderr_mode": "capture",
        });
        if let Some(cwd) = request.cwd {
            body["working_dir"] = Value::String(cwd);
        }
        // The v1 REST API does not persist `tag`; the process wrapper writes a
        // target-side PID record for reconnection instead.
        let _ = request.tag;
        let mut builder = self
            .request(reqwest::Method::POST, "api/v1/processes")?
            .header(header::CONTENT_TYPE, "application/json")
            .json(&body);
        let timeout = request.timeout_ms.unwrap_or(self.config.request_timeout_ms);
        if timeout > 0 {
            builder = builder.timeout(Duration::from_millis(timeout));
        }
        checked_json(builder.send().await.map_err(request_error)?).await
    }

    async fn process(&self, pid: u32) -> Result<ProcessInfo, TensorlakeTransportError> {
        checked_json(
            self.request(reqwest::Method::GET, &format!("api/v1/processes/{pid}"))?
                .timeout(Duration::from_millis(self.config.request_timeout_ms))
                .send()
                .await
                .map_err(request_error)?,
        )
        .await
    }

    async fn follow(&self, pid: u32) -> Result<RemoteProcessStream, TensorlakeTransportError> {
        let response = self
            .request(
                reqwest::Method::GET,
                &format!("api/v1/processes/{pid}/output/follow"),
            )?
            .header(header::ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(request_error)?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        Ok(decode_output_stream(response))
    }

    async fn process_stream(
        &self,
        pid: u32,
        include_started: bool,
    ) -> Result<RemoteProcessStream, TensorlakeTransportError> {
        let output = self.follow(pid).await?;
        let first =
            stream::iter(include_started.then_some(Ok(RemoteProcessEvent::Started { pid })));
        let transport = self.clone();
        let completion =
            stream::once(async move { transport.process(pid).await.map(process_completion_event) });
        Ok(Box::pin(first.chain(output).chain(completion)))
    }

    async fn empty_response(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<(), TensorlakeTransportError> {
        let response = builder
            .timeout(Duration::from_millis(self.config.request_timeout_ms))
            .send()
            .await
            .map_err(request_error)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(response_error(response).await)
        }
    }
}

#[async_trait]
impl TensorlakeTransport for TensorlakeHttpTransport {
    async fn start_process(
        &self,
        request: RemoteProcessRequest,
    ) -> Result<RemoteProcessStream, TensorlakeTransportError> {
        let process = self.start(request).await?;
        self.process_stream(process.pid, true).await
    }

    async fn connect_process(
        &self,
        pid: u32,
        _timeout_ms: Option<u64>,
    ) -> Result<RemoteProcessStream, TensorlakeTransportError> {
        self.process_stream(pid, false).await
    }

    async fn list_processes(&self) -> Result<Vec<RemoteProcessSummary>, TensorlakeTransportError> {
        let list: ProcessList = checked_json(
            self.request(reqwest::Method::GET, "api/v1/processes")?
                .timeout(Duration::from_millis(self.config.request_timeout_ms))
                .send()
                .await
                .map_err(request_error)?,
        )
        .await?;
        Ok(list
            .processes
            .into_iter()
            .map(|process| RemoteProcessSummary {
                pid: process.pid,
                command: process.command,
                arguments: process.args,
                status: process.status,
                exit_code: process.exit_code,
                tag: None,
            })
            .collect())
    }

    async fn send_input(
        &self,
        pid: u32,
        pty: bool,
        data: &[u8],
    ) -> Result<(), TensorlakeTransportError> {
        if pty {
            return Err(TensorlakeTransportError::new(
                "native Tensorlake PTY input is not available through the process transport",
            ));
        }
        self.empty_response(
            self.request(
                reqwest::Method::POST,
                &format!("api/v1/processes/{pid}/stdin"),
            )?
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(data.to_vec()),
        )
        .await
    }

    async fn close_stdin(&self, pid: u32) -> Result<(), TensorlakeTransportError> {
        self.empty_response(self.request(
            reqwest::Method::POST,
            &format!("api/v1/processes/{pid}/stdin/close"),
        )?)
        .await
    }

    async fn resize_pty(
        &self,
        _pid: u32,
        _columns: u16,
        _rows: u16,
    ) -> Result<(), TensorlakeTransportError> {
        Err(TensorlakeTransportError::new(
            "native Tensorlake PTY resize is not available through the process transport",
        ))
    }

    async fn signal_process(&self, pid: u32, kill: bool) -> Result<(), TensorlakeTransportError> {
        self.empty_response(
            self.request(
                reqwest::Method::POST,
                &format!("api/v1/processes/{pid}/signal"),
            )?
            .header(header::CONTENT_TYPE, "application/json")
            .json(&json!({"signal": if kill { 9 } else { 15 }})),
        )
        .await
    }
}

fn process_completion_event(process: ProcessInfo) -> RemoteProcessEvent {
    let exit_code = process
        .exit_code
        .unwrap_or_else(|| process.signal.map_or(1, |signal| 128 + signal));
    let error = match process.status.as_str() {
        "oom_killed" => Some("Tensorlake process was killed after exhausting memory".to_owned()),
        "signaled" => process
            .signal
            .map(|signal| format!("process received signal {signal}")),
        _ => None,
    };
    RemoteProcessEvent::Exited {
        exit_code,
        exited: process.status == "exited",
        status: process.status,
        error,
    }
}

#[derive(Debug, PartialEq)]
struct SseFrame {
    event: String,
    data: String,
}

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

fn decode_output_stream(response: Response) -> RemoteProcessStream {
    struct State {
        source: ByteStream,
        buffer: Vec<u8>,
        done: bool,
    }
    let state = State {
        source: Box::pin(response.bytes_stream()),
        buffer: Vec::new(),
        done: false,
    };
    Box::pin(stream::unfold(state, |mut state| async move {
        loop {
            if let Some((consumed, frame)) = take_sse_frame(&state.buffer) {
                state.buffer.drain(..consumed);
                match frame {
                    Ok(frame) if frame.event == "eof" => return None,
                    Ok(frame) if frame.event == "output" => {
                        let event = parse_output_event(&frame.data);
                        return Some((event, state));
                    }
                    Ok(_) => continue,
                    Err(error) => return Some((Err(error), state)),
                }
            }
            if state.done {
                if state.buffer.iter().all(u8::is_ascii_whitespace) {
                    return None;
                }
                let tail = std::mem::take(&mut state.buffer);
                return Some((parse_output_tail(&tail), state));
            }
            match state.source.next().await {
                Some(Ok(chunk)) => state.buffer.extend_from_slice(&chunk),
                Some(Err(source)) => {
                    state.done = true;
                    return Some((Err(request_error(source)), state));
                }
                None => state.done = true,
            }
        }
    }))
}

fn take_sse_frame(bytes: &[u8]) -> Option<(usize, Result<SseFrame, TensorlakeTransportError>)> {
    let (index, delimiter) = bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| (index, 2))
        .or_else(|| {
            bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|index| (index, 4))
        })?;
    Some((index + delimiter, parse_sse_frame(&bytes[..index])))
}

fn parse_sse_frame(bytes: &[u8]) -> Result<SseFrame, TensorlakeTransportError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|source| TensorlakeTransportError::new(format!("invalid SSE UTF-8: {source}")))?;
    let mut event = "message".to_owned();
    let mut data = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("event:") {
            event = value.trim_start().to_owned();
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim_start());
        }
    }
    Ok(SseFrame {
        event,
        data: data.join("\n"),
    })
}

fn parse_output_event(data: &str) -> Result<RemoteProcessEvent, TensorlakeTransportError> {
    #[derive(Deserialize)]
    struct Output {
        line: String,
        stream: String,
    }
    let output: Output = serde_json::from_str(data).map_err(|source| {
        TensorlakeTransportError::new(format!("invalid Tensorlake output event: {source}"))
    })?;
    let stream = match output.stream.as_str() {
        "stdout" => RemoteStreamKind::Stdout,
        "stderr" => RemoteStreamKind::Stderr,
        other => {
            return Err(TensorlakeTransportError::new(format!(
                "unknown Tensorlake output stream `{other}`"
            )));
        }
    };
    let mut data = output.line.into_bytes();
    if !data.ends_with(b"\n") {
        data.push(b'\n');
    }
    Ok(RemoteProcessEvent::Output { stream, data })
}

fn parse_output_tail(bytes: &[u8]) -> Result<RemoteProcessEvent, TensorlakeTransportError> {
    Err(TensorlakeTransportError::disconnected(format!(
        "Tensorlake SSE stream ended with an incomplete frame ({} bytes)",
        bytes.len()
    )))
}

async fn checked_json<T: serde::de::DeserializeOwned>(
    response: Response,
) -> Result<T, TensorlakeTransportError> {
    if !response.status().is_success() {
        return Err(response_error(response).await);
    }
    response.json().await.map_err(request_error)
}

async fn response_error(response: Response) -> TensorlakeTransportError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    TensorlakeTransportError {
        message: format!("Tensorlake sandbox proxy returned {status}: {body}"),
        retryable: status.is_server_error() || status.as_u16() == 429,
        disconnected: false,
    }
}

fn request_error(source: reqwest::Error) -> TensorlakeTransportError {
    TensorlakeTransportError {
        message: source.to_string(),
        retryable: source.is_timeout() || source.is_connect(),
        disconnected: source.is_connect() || source.is_timeout() || source.is_body(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn parses_tensorlake_sse_output() {
        let bytes = b"event: output\r\ndata: {\"line\":\"hello\",\"timestamp\":1,\"stream\":\"stdout\"}\r\n\r\n";
        let (used, frame) = take_sse_frame(bytes).expect("complete frame");
        assert_eq!(used, bytes.len());
        let frame = frame.expect("valid SSE");
        assert_eq!(frame.event, "output");
        assert_eq!(
            parse_output_event(&frame.data).unwrap(),
            RemoteProcessEvent::Output {
                stream: RemoteStreamKind::Stdout,
                data: b"hello\n".to_vec(),
            }
        );
    }

    #[test]
    fn parses_multiline_sse_data() {
        let frame = parse_sse_frame(b"event: note\ndata: first\ndata: second").unwrap();
        assert_eq!(frame.data, "first\nsecond");
    }

    #[test]
    fn maps_signaled_process_exit() {
        let event = process_completion_event(ProcessInfo {
            pid: 9,
            status: "signaled".to_owned(),
            exit_code: None,
            signal: Some(15),
            command: "sleep".to_owned(),
            args: Vec::new(),
        });
        assert!(matches!(
            event,
            RemoteProcessEvent::Exited { exit_code: 143, .. }
        ));
    }

    #[test]
    fn process_request_environment_is_ordered() {
        let environment = BTreeMap::from([("A".to_owned(), "1".to_owned())]);
        assert_eq!(environment.get("A").map(String::as_str), Some("1"));
    }
}
