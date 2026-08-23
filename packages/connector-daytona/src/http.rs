use std::{collections::VecDeque, time::Duration};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt, stream};
use reqwest::{Client, Response, StatusCode, header};
use serde::Deserialize;
use serde_json::json;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderValue, header as ws_header},
    },
};
use uuid::Uuid;

use crate::{
    DaytonaConnectionConfig, DaytonaTransport, DaytonaTransportError, RemoteProcessEvent,
    RemoteProcessId, RemoteProcessRequest, RemoteProcessStream, RemoteProcessSummary,
    RemoteStreamKind,
};

const STDOUT_PREFIX: &[u8] = &[0x01, 0x01, 0x01];
const STDERR_PREFIX: &[u8] = &[0x02, 0x02, 0x02];

#[derive(Clone)]
pub struct DaytonaHttpTransport {
    client: Client,
    config: DaytonaConnectionConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    session_id: String,
    #[serde(default)]
    commands: Vec<Command>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Command {
    id: String,
    command: String,
    #[serde(default)]
    exit_code: Option<i32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionExecuteResponse {
    cmd_id: String,
}

struct SessionCommandLogs {
    output: Vec<u8>,
}

impl DaytonaHttpTransport {
    pub fn new(config: DaytonaConnectionConfig) -> Result<Self, DaytonaTransportError> {
        if config.api_key.trim().is_empty() {
            return Err(DaytonaTransportError::new(
                "Daytona API key must not be empty",
            ));
        }
        if !matches!(config.toolbox_url.scheme(), "http" | "https")
            || config.toolbox_url.host_str().is_none()
        {
            return Err(DaytonaTransportError::new(
                "Daytona Toolbox URL must be an absolute HTTP(S) URL",
            ));
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_millis(config.request_timeout_ms))
            .build()
            .map_err(|source| DaytonaTransportError::new(source.to_string()))?;
        Ok(Self { client, config })
    }

    fn endpoint(&self, path: &str) -> Result<reqwest::Url, DaytonaTransportError> {
        self.config
            .toolbox_url
            .join(path.trim_start_matches('/'))
            .map_err(|source| DaytonaTransportError::new(source.to_string()))
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, DaytonaTransportError> {
        Ok(self
            .client
            .request(method, self.endpoint(path)?)
            .bearer_auth(&self.config.api_key)
            .header("X-Daytona-Source", "agent-pane"))
    }

    async fn create_session(&self, session_id: &str) -> Result<bool, DaytonaTransportError> {
        let response = self
            .request(reqwest::Method::POST, "process/session")?
            .header(header::CONTENT_TYPE, "application/json")
            .json(&json!({"sessionId": session_id}))
            .timeout(Duration::from_millis(self.config.request_timeout_ms))
            .send()
            .await
            .map_err(request_error)?;
        if response.status().is_success() {
            Ok(true)
        } else if response.status() == StatusCode::CONFLICT {
            Ok(false)
        } else {
            Err(response_error(response).await)
        }
    }

    async fn session(&self, session_id: &str) -> Result<Session, DaytonaTransportError> {
        checked_json(
            self.request(
                reqwest::Method::GET,
                &format!("process/session/{session_id}"),
            )?
            .timeout(Duration::from_millis(self.config.request_timeout_ms))
            .send()
            .await
            .map_err(request_error)?,
        )
        .await
    }

    async fn command(
        &self,
        process_id: &RemoteProcessId,
    ) -> Result<Command, DaytonaTransportError> {
        checked_json(
            self.request(
                reqwest::Method::GET,
                &format!(
                    "process/session/{}/command/{}",
                    process_id.session_id, process_id.command_id
                ),
            )?
            .timeout(Duration::from_millis(self.config.request_timeout_ms))
            .send()
            .await
            .map_err(request_error)?,
        )
        .await
    }

    async fn command_logs(
        &self,
        process_id: &RemoteProcessId,
    ) -> Result<SessionCommandLogs, DaytonaTransportError> {
        let response = self
            .request(
                reqwest::Method::GET,
                &format!(
                    "process/session/{}/command/{}/logs",
                    process_id.session_id, process_id.command_id
                ),
            )?
            .timeout(Duration::from_millis(self.config.request_timeout_ms))
            .send()
            .await
            .map_err(request_error)?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        let output = response.bytes().await.map_err(request_error)?.to_vec();
        Ok(SessionCommandLogs { output })
    }

    async fn delete_session(&self, session_id: &str) -> Result<(), DaytonaTransportError> {
        let response = self
            .request(
                reqwest::Method::DELETE,
                &format!("process/session/{session_id}"),
            )?
            .timeout(Duration::from_millis(self.config.request_timeout_ms))
            .send()
            .await
            .map_err(request_error)?;
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(response_error(response).await)
        }
    }

    async fn execute(
        &self,
        session_id: &str,
        request: &RemoteProcessRequest,
    ) -> Result<RemoteProcessId, DaytonaTransportError> {
        let command = shell_command(request)?;
        let response: SessionExecuteResponse = checked_json(
            self.request(
                reqwest::Method::POST,
                &format!("process/session/{session_id}/exec"),
            )?
            .header(header::CONTENT_TYPE, "application/json")
            .json(&json!({
                "command": command,
                "runAsync": true,
                "suppressInputEcho": true,
            }))
            .timeout(Duration::from_millis(self.config.request_timeout_ms))
            .send()
            .await
            .map_err(request_error)?,
        )
        .await?;
        Ok(RemoteProcessId {
            session_id: session_id.to_owned(),
            command_id: response.cmd_id,
        })
    }

    async fn start(
        &self,
        request: &RemoteProcessRequest,
    ) -> Result<RemoteProcessId, DaytonaTransportError> {
        if request.pty.is_some() {
            return Err(DaytonaTransportError::new(
                "native Daytona PTYs are not used by the recoverable process transport",
            ));
        }
        let session_id = request
            .session_id
            .clone()
            .unwrap_or_else(|| format!("ap-op-{}", Uuid::now_v7().simple()));
        if self.create_session(&session_id).await? {
            return self.execute(&session_id, request).await;
        }

        // A stable execution session may already exist after a retried start.
        // Reuse its command rather than launching the user command twice.
        let session = self.session(&session_id).await?;
        if let Some(command) = session.commands.last() {
            Ok(RemoteProcessId {
                session_id,
                command_id: command.id.clone(),
            })
        } else {
            self.execute(&session_id, request).await
        }
    }

    async fn follow(
        &self,
        process_id: &RemoteProcessId,
    ) -> Result<RemoteProcessStream, DaytonaTransportError> {
        let mut url = self.endpoint(&format!(
            "process/session/{}/command/{}/logs?follow=true",
            process_id.session_id, process_id.command_id
        ))?;
        let scheme = match url.scheme() {
            "https" => "wss",
            "http" => "ws",
            other => {
                return Err(DaytonaTransportError::new(format!(
                    "unsupported Daytona WebSocket scheme `{other}`"
                )));
            }
        };
        url.set_scheme(scheme).map_err(|()| {
            DaytonaTransportError::new("failed to construct Daytona WebSocket URL")
        })?;
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|source| DaytonaTransportError::new(source.to_string()))?;
        let authorization = HeaderValue::from_str(&format!("Bearer {}", self.config.api_key))
            .map_err(|source| DaytonaTransportError::new(source.to_string()))?;
        request
            .headers_mut()
            .insert(ws_header::AUTHORIZATION, authorization);
        request
            .headers_mut()
            .insert("X-Daytona-Source", HeaderValue::from_static("agent-pane"));
        let (socket, _) = connect_async(request).await.map_err(websocket_error)?;
        Ok(decode_log_stream(socket))
    }

    async fn process_stream(
        &self,
        process_id: RemoteProcessId,
        include_started: bool,
    ) -> Result<RemoteProcessStream, DaytonaTransportError> {
        let (output, output_closed) = match self.follow(&process_id).await {
            Ok(output) => (output, false),
            // Fast commands can finish while the WebSocket handshake is in
            // flight, and the hosted proxy then resets that handshake. Polling
            // plus the final HTTP log snapshot still provides exact output.
            Err(_) => (Box::pin(stream::empty()) as RemoteProcessStream, true),
        };
        let mut pending = VecDeque::new();
        if include_started {
            pending.push_back(RemoteProcessEvent::Started {
                process_id: process_id.clone(),
            });
        }
        let mut completion_poll = tokio::time::interval(Duration::from_millis(100));
        completion_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let state = ProcessStreamState {
            transport: self.clone(),
            process_id,
            output,
            output_closed,
            reconnect_after: tokio::time::Instant::now(),
            completion_poll,
            observed_output: Vec::new(),
            pending,
            done: false,
        };
        Ok(Box::pin(stream::unfold(state, |mut state| async move {
            loop {
                if let Some(event) = state.pending.pop_front() {
                    record_output(&mut state, &event);
                    return Some((Ok(event), state));
                }
                if state.done {
                    return None;
                }

                tokio::select! {
                    biased;
                    event = state.output.next(), if !state.output_closed => {
                        match event {
                            Some(Ok(event)) => {
                                record_output(&mut state, &event);
                                return Some((Ok(event), state));
                            }
                            Some(Err(_)) | None => {
                                // The provider can close or retain the WebSocket independently
                                // of command completion. The final HTTP log snapshot below is
                                // authoritative and fills any unobserved suffix.
                                state.output_closed = true;
                                state.reconnect_after = tokio::time::Instant::now();
                            }
                        }
                    }
                    _ = state.completion_poll.tick() => {
                        match state.transport.command(&state.process_id).await {
                            Ok(command) if command.exit_code.is_some() => {
                                match state.transport.command_logs(&state.process_id).await {
                                    Ok(logs) => {
                                        if let Err(error) = queue_final_logs(&mut state, logs) {
                                            state.done = true;
                                            return Some((Err(error), state));
                                        }
                                    }
                                    Err(error) => {
                                        state.done = true;
                                        return Some((Err(error), state));
                                    }
                                }
                                let _ = state
                                    .transport
                                    .delete_session(&state.process_id.session_id)
                                    .await;
                                state.pending.push_back(command_completion_event(command));
                                state.done = true;
                            }
                            Ok(_) => {
                                if state.output_closed
                                    && tokio::time::Instant::now() >= state.reconnect_after
                                {
                                    match state.transport.follow(&state.process_id).await {
                                        Ok(output) => {
                                            state.output = output;
                                            state.output_closed = false;
                                        }
                                        Err(_) => {
                                            state.reconnect_after = tokio::time::Instant::now()
                                                + Duration::from_secs(1);
                                        }
                                    }
                                }
                            }
                            Err(error) => {
                                return Some((Err(error), state));
                            }
                        }
                    }
                }
            }
        })))
    }

    async fn empty_response(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<(), DaytonaTransportError> {
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
impl DaytonaTransport for DaytonaHttpTransport {
    async fn start_process(
        &self,
        request: RemoteProcessRequest,
    ) -> Result<RemoteProcessStream, DaytonaTransportError> {
        let process_id = self.start(&request).await?;
        self.process_stream(process_id, true).await
    }

    async fn connect_process(
        &self,
        process_id: &RemoteProcessId,
        _timeout_ms: Option<u64>,
    ) -> Result<RemoteProcessStream, DaytonaTransportError> {
        self.process_stream(process_id.clone(), false).await
    }

    async fn list_processes(&self) -> Result<Vec<RemoteProcessSummary>, DaytonaTransportError> {
        let sessions: Vec<Session> = checked_json(
            self.request(reqwest::Method::GET, "process/session")?
                .timeout(Duration::from_millis(self.config.request_timeout_ms))
                .send()
                .await
                .map_err(request_error)?,
        )
        .await?;
        Ok(sessions
            .into_iter()
            .flat_map(|session| {
                session.commands.into_iter().map(move |command| {
                    let status = if command.exit_code.is_some() {
                        "exited"
                    } else {
                        "running"
                    };
                    RemoteProcessSummary {
                        process_id: RemoteProcessId {
                            session_id: session.session_id.clone(),
                            command_id: command.id,
                        },
                        command: command.command,
                        arguments: Vec::new(),
                        status: status.to_owned(),
                        exit_code: command.exit_code,
                        tag: None,
                    }
                })
            })
            .collect())
    }

    async fn send_input(
        &self,
        process_id: &RemoteProcessId,
        pty: bool,
        data: &[u8],
    ) -> Result<(), DaytonaTransportError> {
        if pty {
            return Err(DaytonaTransportError::new(
                "native Daytona PTY input is not used by the recoverable process transport",
            ));
        }
        let data = std::str::from_utf8(data).map_err(|source| {
            DaytonaTransportError::new(format!(
                "Daytona session input must be UTF-8 framed control data: {source}"
            ))
        })?;
        self.empty_response(
            self.request(
                reqwest::Method::POST,
                &format!(
                    "process/session/{}/command/{}/input",
                    process_id.session_id, process_id.command_id
                ),
            )?
            .header(header::CONTENT_TYPE, "application/json")
            .json(&json!({"data": data})),
        )
        .await
    }

    async fn close_stdin(&self, process_id: &RemoteProcessId) -> Result<(), DaytonaTransportError> {
        self.send_input(process_id, false, b"{\"type\":\"close_stdin\"}\n")
            .await
    }

    async fn resize_pty(
        &self,
        _process_id: &RemoteProcessId,
        _columns: u16,
        _rows: u16,
    ) -> Result<(), DaytonaTransportError> {
        Err(DaytonaTransportError::new(
            "native Daytona PTY resize is not used by the recoverable process transport",
        ))
    }

    async fn signal_process(
        &self,
        process_id: &RemoteProcessId,
        _kill: bool,
    ) -> Result<(), DaytonaTransportError> {
        // Toolbox has no command-level signal endpoint. Deleting the dedicated
        // session terminates its shell and is safe because every agent-pane
        // execution owns one session.
        self.delete_session(&process_id.session_id).await
    }
}

fn shell_command(request: &RemoteProcessRequest) -> Result<String, DaytonaTransportError> {
    if request.command.trim().is_empty() {
        return Err(DaytonaTransportError::new(
            "remote process command must not be empty",
        ));
    }
    let mut pieces = Vec::with_capacity(request.arguments.len() + 1);
    pieces.push(shell_quote(&request.command));
    pieces.extend(
        request
            .arguments
            .iter()
            .map(|argument| shell_quote(argument)),
    );
    // A Daytona session owns the shell that tracks command completion. Using
    // the shell `exec` builtin here would replace that shell and can leave the
    // command permanently reported as running even after the child exits.
    let mut command = pieces.join(" ");
    if !request.environment.is_empty() {
        let assignments = request
            .environment
            .iter()
            .map(|(key, value)| shell_quote(&format!("{key}={value}")))
            .collect::<Vec<_>>()
            .join(" ");
        command = format!("env {assignments} {}", pieces.join(" "));
    }
    if let Some(cwd) = &request.cwd {
        command = format!("cd -- {} && {command}", shell_quote(cwd));
    }
    Ok(command)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn command_completion_event(command: Command) -> RemoteProcessEvent {
    let exit_code = command.exit_code.unwrap_or(1);
    RemoteProcessEvent::Exited {
        exit_code,
        exited: command.exit_code.is_some(),
        status: if command.exit_code.is_some() {
            "exited".to_owned()
        } else {
            "unknown".to_owned()
        },
        error: command.exit_code.is_none().then(|| {
            "Daytona command log stream closed before an exit code was available".to_owned()
        }),
    }
}

struct ProcessStreamState {
    transport: DaytonaHttpTransport,
    process_id: RemoteProcessId,
    output: RemoteProcessStream,
    output_closed: bool,
    reconnect_after: tokio::time::Instant,
    completion_poll: tokio::time::Interval,
    observed_output: Vec<u8>,
    pending: VecDeque<RemoteProcessEvent>,
    done: bool,
}

fn record_output(state: &mut ProcessStreamState, event: &RemoteProcessEvent) {
    if let RemoteProcessEvent::Output { data, .. } = event {
        state.observed_output.extend_from_slice(data);
    }
}

fn queue_final_logs(
    state: &mut ProcessStreamState,
    logs: SessionCommandLogs,
) -> Result<(), DaytonaTransportError> {
    // The raw Toolbox endpoint and current hosted WebSocket combine stdout and
    // stderr. This transport runs only agent-pane's wrappers, whose protocol is
    // emitted on stdout and represents child stdout/stderr explicitly.
    queue_unseen_log(
        &mut state.pending,
        RemoteStreamKind::Stdout,
        &state.observed_output,
        &logs.output,
    )
}

fn queue_unseen_log(
    pending: &mut VecDeque<RemoteProcessEvent>,
    stream: RemoteStreamKind,
    observed: &[u8],
    complete: &[u8],
) -> Result<(), DaytonaTransportError> {
    let Some(unseen) = complete.strip_prefix(observed) else {
        return Err(DaytonaTransportError::new(format!(
            "Daytona final {stream:?} log did not match its streamed prefix"
        )));
    };
    if !unseen.is_empty() {
        pending.push_back(RemoteProcessEvent::Output {
            stream,
            data: unseen.to_vec(),
        });
    }
    Ok(())
}

type DaytonaSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct WebSocketState {
    socket: DaytonaSocket,
    decoder: MultiplexDecoder,
    pending: VecDeque<RemoteProcessEvent>,
    done: bool,
}

fn decode_log_stream(socket: DaytonaSocket) -> RemoteProcessStream {
    let state = WebSocketState {
        socket,
        decoder: MultiplexDecoder::default(),
        pending: VecDeque::new(),
        done: false,
    };
    Box::pin(stream::unfold(state, |mut state| async move {
        loop {
            if let Some(event) = state.pending.pop_front() {
                return Some((Ok(event), state));
            }
            if state.done {
                return None;
            }
            match state.socket.next().await {
                Some(Ok(Message::Binary(bytes))) => {
                    state.pending.extend(state.decoder.push(&bytes));
                }
                Some(Ok(Message::Text(text))) => {
                    state.pending.extend(state.decoder.push(text.as_bytes()));
                }
                Some(Ok(Message::Ping(payload))) => {
                    if let Err(source) = state.socket.send(Message::Pong(payload)).await {
                        state.done = true;
                        return Some((Err(websocket_error(source)), state));
                    }
                }
                Some(Ok(Message::Close(_))) | None => {
                    state.pending.extend(state.decoder.finish());
                    state.done = true;
                }
                Some(Ok(Message::Pong(_) | Message::Frame(_))) => {}
                Some(Err(source)) => {
                    state.done = true;
                    return Some((Err(websocket_error(source)), state));
                }
            }
        }
    }))
}

struct MultiplexDecoder {
    buffer: Vec<u8>,
    current: Option<RemoteStreamKind>,
}

impl Default for MultiplexDecoder {
    fn default() -> Self {
        Self {
            buffer: Vec::new(),
            // Hosted Daytona currently returns an unlabeled combined byte
            // stream, while other Toolbox versions prefix chunks with the
            // documented stdout/stderr markers. Treat unlabeled bytes as
            // stdout and still switch streams whenever markers are present.
            current: Some(RemoteStreamKind::Stdout),
        }
    }
}

impl MultiplexDecoder {
    fn push(&mut self, bytes: &[u8]) -> Vec<RemoteProcessEvent> {
        self.buffer.extend_from_slice(bytes);
        let mut output = Vec::new();
        loop {
            if let Some((index, kind)) = next_marker(&self.buffer) {
                if index > 0 {
                    let payload: Vec<_> = self.buffer.drain(..index).collect();
                    self.emit(payload, &mut output);
                }
                self.buffer.drain(..3);
                self.current = Some(kind);
                continue;
            }
            let keep = partial_marker_suffix(&self.buffer);
            let safe = self.buffer.len().saturating_sub(keep);
            if safe > 0 {
                let payload: Vec<_> = self.buffer.drain(..safe).collect();
                self.emit(payload, &mut output);
            }
            break;
        }
        output
    }

    fn finish(&mut self) -> Vec<RemoteProcessEvent> {
        let mut output = Vec::new();
        let payload = std::mem::take(&mut self.buffer);
        self.emit(payload, &mut output);
        output
    }

    fn emit(&self, payload: Vec<u8>, output: &mut Vec<RemoteProcessEvent>) {
        if let Some(stream) = self.current
            && !payload.is_empty()
        {
            output.push(RemoteProcessEvent::Output {
                stream,
                data: payload,
            });
        }
    }
}

fn next_marker(bytes: &[u8]) -> Option<(usize, RemoteStreamKind)> {
    let stdout = bytes.windows(3).position(|window| window == STDOUT_PREFIX);
    let stderr = bytes.windows(3).position(|window| window == STDERR_PREFIX);
    match (stdout, stderr) {
        (Some(left), Some(right)) if left <= right => Some((left, RemoteStreamKind::Stdout)),
        (Some(_), Some(right)) => Some((right, RemoteStreamKind::Stderr)),
        (Some(index), None) => Some((index, RemoteStreamKind::Stdout)),
        (None, Some(index)) => Some((index, RemoteStreamKind::Stderr)),
        (None, None) => None,
    }
}

fn partial_marker_suffix(bytes: &[u8]) -> usize {
    for length in (1..=2).rev() {
        if bytes.len() >= length {
            let suffix = &bytes[bytes.len() - length..];
            if STDOUT_PREFIX.starts_with(suffix) || STDERR_PREFIX.starts_with(suffix) {
                return length;
            }
        }
    }
    0
}

async fn checked_json<T: serde::de::DeserializeOwned>(
    response: Response,
) -> Result<T, DaytonaTransportError> {
    if !response.status().is_success() {
        return Err(response_error(response).await);
    }
    response.json().await.map_err(request_error)
}

async fn response_error(response: Response) -> DaytonaTransportError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    DaytonaTransportError {
        message: format!("Daytona Toolbox returned {status}: {body}"),
        retryable: status.is_server_error() || status.as_u16() == 429,
        disconnected: false,
    }
}

fn request_error(source: reqwest::Error) -> DaytonaTransportError {
    DaytonaTransportError {
        message: source.to_string(),
        retryable: source.is_timeout() || source.is_connect(),
        disconnected: source.is_connect() || source.is_timeout() || source.is_body(),
    }
}

fn websocket_error(source: tokio_tungstenite::tungstenite::Error) -> DaytonaTransportError {
    DaytonaTransportError::disconnected(format!("Daytona log WebSocket failed: {source}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn shell_command_quotes_arguments_environment_and_cwd() {
        let request = RemoteProcessRequest {
            session_id: None,
            command: "python3".to_owned(),
            arguments: vec!["-c".to_owned(), "print('hello')".to_owned()],
            environment: BTreeMap::from([("A".to_owned(), "one two".to_owned())]),
            cwd: Some("/work/a b".to_owned()),
            tag: None,
            stdin: false,
            pty: None,
            timeout_ms: None,
        };
        let command = shell_command(&request).unwrap();
        assert!(command.starts_with("cd -- '/work/a b' && env 'A=one two'"));
        assert!(command.contains("'print('\"'\"'hello'\"'\"')'"));
    }

    #[test]
    fn demultiplexes_split_daytona_markers() {
        let mut decoder = MultiplexDecoder::default();
        assert!(decoder.push(&[1, 1]).is_empty());
        let first = decoder.push(&[1, b'a', b'b', 2, 2]);
        assert_eq!(
            first,
            vec![RemoteProcessEvent::Output {
                stream: RemoteStreamKind::Stdout,
                data: b"ab".to_vec(),
            }]
        );
        let second = decoder.push(&[2, b'e', b'r', b'r']);
        assert_eq!(
            second,
            vec![RemoteProcessEvent::Output {
                stream: RemoteStreamKind::Stderr,
                data: b"err".to_vec(),
            }]
        );
    }

    #[test]
    fn treats_unlabelled_hosted_daytona_logs_as_stdout() {
        let mut decoder = MultiplexDecoder::default();
        assert_eq!(
            decoder.push(b"wrapper event\n"),
            vec![RemoteProcessEvent::Output {
                stream: RemoteStreamKind::Stdout,
                data: b"wrapper event\n".to_vec(),
            }]
        );
    }

    #[test]
    fn completion_maps_exit_code() {
        let event = command_completion_event(Command {
            id: "cmd".to_owned(),
            command: "true".to_owned(),
            exit_code: Some(0),
        });
        assert!(matches!(
            event,
            RemoteProcessEvent::Exited {
                exit_code: 0,
                exited: true,
                ..
            }
        ));
    }

    #[test]
    fn final_log_snapshot_only_queues_the_unseen_suffix() {
        let mut pending = VecDeque::new();
        queue_unseen_log(
            &mut pending,
            RemoteStreamKind::Stdout,
            b"first",
            b"firstsecond",
        )
        .unwrap();
        assert_eq!(
            pending,
            VecDeque::from([RemoteProcessEvent::Output {
                stream: RemoteStreamKind::Stdout,
                data: b"second".to_vec(),
            }])
        );
    }
}
