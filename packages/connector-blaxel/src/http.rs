use std::{pin::Pin, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream};
use reqwest::{Client, Response, header};
use serde::{Deserialize, Deserializer};
use serde_json::json;
use uuid::Uuid;

use crate::{
    BlaxelConnectionConfig, BlaxelTransport, BlaxelTransportError, RemoteProcessEvent,
    RemoteProcessRequest, RemoteProcessStream, RemoteProcessSummary, RemoteStreamKind,
};

const EXECUTION_PROCESS_PREFIX: &str = "agent-pane-execution-";

#[derive(Clone)]
pub struct BlaxelHttpTransport {
    client: Client,
    config: BlaxelConnectionConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProcessInfo {
    #[serde(deserialize_with = "deserialize_pid")]
    pid: u32,
    status: String,
    #[serde(default)]
    exit_code: i32,
    #[serde(default)]
    command: String,
    #[serde(default)]
    name: String,
}

impl BlaxelHttpTransport {
    pub fn new(config: BlaxelConnectionConfig) -> Result<Self, BlaxelTransportError> {
        if config.api_key.trim().is_empty() {
            return Err(BlaxelTransportError::new(
                "Blaxel API key must not be empty",
            ));
        }
        if !matches!(config.sandbox_url.scheme(), "http" | "https")
            || config.sandbox_url.host_str().is_none()
        {
            return Err(BlaxelTransportError::new(
                "Blaxel sandbox URL must be an absolute HTTP(S) URL",
            ));
        }
        if config
            .workspace
            .as_deref()
            .is_some_and(|workspace| workspace.trim().is_empty())
        {
            return Err(BlaxelTransportError::new(
                "Blaxel workspace must not be empty when provided",
            ));
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_millis(config.request_timeout_ms))
            .build()
            .map_err(|source| BlaxelTransportError::new(source.to_string()))?;
        Ok(Self { client, config })
    }

    fn endpoint(&self, path: &str) -> Result<reqwest::Url, BlaxelTransportError> {
        self.config
            .sandbox_url
            .join(path.trim_start_matches('/'))
            .map_err(|source| BlaxelTransportError::new(source.to_string()))
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, BlaxelTransportError> {
        let mut request = self
            .client
            .request(method, self.endpoint(path)?)
            .bearer_auth(&self.config.api_key);
        if let Some(workspace) = &self.config.workspace {
            request = request.header("X-Blaxel-Workspace", workspace);
        }
        Ok(request)
    }

    async fn start(
        &self,
        request: RemoteProcessRequest,
    ) -> Result<ProcessInfo, BlaxelTransportError> {
        if request.pty.is_some() {
            return Err(BlaxelTransportError::new(
                "native Blaxel terminal sessions are not used by the recoverable process transport",
            ));
        }
        // Blaxel's process REST endpoint has no stdin channel. Interactive
        // child input is handled by the execution wrapper's control mailbox.
        let _ = request.stdin;
        let command = shell_command(&request.command, &request.arguments);
        let durable_execution = request
            .tag
            .as_deref()
            .is_some_and(|tag| tag.starts_with(EXECUTION_PROCESS_PREFIX));
        let name = request.tag.map_or_else(
            || format!("agent-pane-operation-{}", Uuid::now_v7()),
            |tag| {
                if durable_execution {
                    tag
                } else {
                    format!("{}-{}", sanitize_name(&tag), Uuid::now_v7())
                }
            },
        );
        let timeout_seconds = if durable_execution {
            0
        } else {
            request
                .timeout_ms
                .unwrap_or(self.config.request_timeout_ms)
                .div_ceil(1_000)
        };
        let mut body = json!({
            "command": command,
            "env": request.environment,
            "keepAlive": durable_execution,
            "maxRestarts": 0,
            "name": name,
            "restartOnFailure": false,
            "timeout": timeout_seconds,
            "waitForCompletion": false,
        });
        if let Some(cwd) = request.cwd {
            body["workingDir"] = serde_json::Value::String(cwd);
        }
        let response = self
            .request(reqwest::Method::POST, "process")?
            .header(header::CONTENT_TYPE, "application/json")
            .timeout(Duration::from_millis(self.config.request_timeout_ms))
            .json(&body)
            .send()
            .await
            .map_err(request_error)?;
        checked_json(response).await
    }

    async fn process(&self, pid: u32) -> Result<ProcessInfo, BlaxelTransportError> {
        checked_json(
            self.request(reqwest::Method::GET, &format!("process/{pid}"))?
                .timeout(Duration::from_millis(self.config.request_timeout_ms))
                .send()
                .await
                .map_err(request_error)?,
        )
        .await
    }

    async fn follow(&self, pid: u32) -> Result<RemoteProcessStream, BlaxelTransportError> {
        let response = self
            .request(reqwest::Method::GET, &format!("process/{pid}/logs/stream"))?
            .header(header::ACCEPT, "text/plain")
            .send()
            .await
            .map_err(request_error)?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        Ok(decode_log_stream(response))
    }

    async fn process_stream(
        &self,
        pid: u32,
        include_started: bool,
    ) -> Result<RemoteProcessStream, BlaxelTransportError> {
        struct State {
            transport: BlaxelHttpTransport,
            pid: u32,
            emit_started: bool,
            output: Option<RemoteProcessStream>,
            done: bool,
        }

        Ok(Box::pin(stream::unfold(
            State {
                transport: self.clone(),
                pid,
                emit_started: include_started,
                output: None,
                done: false,
            },
            |mut state| async move {
                if state.done {
                    return None;
                }
                if state.emit_started {
                    state.emit_started = false;
                    return Some((Ok(RemoteProcessEvent::Started { pid: state.pid }), state));
                }
                if state.output.is_none() {
                    match state.transport.follow(state.pid).await {
                        Ok(output) => state.output = Some(output),
                        Err(error) => {
                            state.done = true;
                            return Some((Err(error), state));
                        }
                    }
                }
                if let Some(event) = state.output.as_mut().expect("output stream").next().await {
                    return Some((event, state));
                }
                state.done = true;
                let event = state
                    .transport
                    .process(state.pid)
                    .await
                    .map(process_completion_event);
                Some((event, state))
            },
        )))
    }
}

#[async_trait]
impl BlaxelTransport for BlaxelHttpTransport {
    async fn start_process(
        &self,
        request: RemoteProcessRequest,
    ) -> Result<RemoteProcessStream, BlaxelTransportError> {
        let process = self.start(request).await?;
        self.process_stream(process.pid, true).await
    }

    async fn connect_process(
        &self,
        pid: u32,
        _timeout_ms: Option<u64>,
    ) -> Result<RemoteProcessStream, BlaxelTransportError> {
        self.process_stream(pid, false).await
    }

    async fn list_processes(&self) -> Result<Vec<RemoteProcessSummary>, BlaxelTransportError> {
        let processes: Vec<ProcessInfo> = checked_json(
            self.request(reqwest::Method::GET, "process")?
                .timeout(Duration::from_millis(self.config.request_timeout_ms))
                .send()
                .await
                .map_err(request_error)?,
        )
        .await?;
        Ok(processes
            .into_iter()
            .map(|process| RemoteProcessSummary {
                pid: process.pid,
                command: process.command,
                arguments: Vec::new(),
                status: process.status.clone(),
                exit_code: terminal_status(&process.status).then_some(process.exit_code),
                tag: (!process.name.is_empty()).then_some(process.name),
            })
            .collect())
    }

    async fn write_control_file(
        &self,
        path: &str,
        content: &[u8],
    ) -> Result<(), BlaxelTransportError> {
        let content = std::str::from_utf8(content).map_err(|source| {
            BlaxelTransportError::new(format!("process control is not valid UTF-8: {source}"))
        })?;
        if !path.starts_with('/') {
            return Err(BlaxelTransportError::new(
                "Blaxel process-control path must be absolute",
            ));
        }
        // The sandbox API represents an absolute filesystem path with the
        // double slash in `/filesystem//absolute/path`.
        let path = format!("filesystem/{path}");
        let response = self
            .request(reqwest::Method::PUT, &path)?
            .header(header::CONTENT_TYPE, "application/json")
            .timeout(Duration::from_millis(self.config.request_timeout_ms))
            .json(&json!({
                "content": content,
                "isDirectory": false,
                "permissions": "0600",
            }))
            .send()
            .await
            .map_err(request_error)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(response_error(response).await)
        }
    }

    async fn signal_process(&self, pid: u32, kill: bool) -> Result<(), BlaxelTransportError> {
        let path = if kill {
            format!("process/{pid}/kill")
        } else {
            format!("process/{pid}")
        };
        let response = self
            .request(reqwest::Method::DELETE, &path)?
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

fn process_completion_event(process: ProcessInfo) -> RemoteProcessEvent {
    let exited = process.status == "completed";
    let exit_code = match process.status.as_str() {
        "killed" if process.exit_code == 0 => 137,
        "stopped" if process.exit_code == 0 => 143,
        "failed" if process.exit_code == 0 => 1,
        _ => process.exit_code,
    };
    let error = (!exited).then(|| format!("Blaxel process ended with status `{}`", process.status));
    RemoteProcessEvent::Exited {
        exit_code,
        exited,
        status: process.status,
        error,
    }
}

fn terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "killed" | "stopped")
}

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

fn decode_log_stream(response: Response) -> RemoteProcessStream {
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
            if let Some(index) = state.buffer.iter().position(|byte| *byte == b'\n') {
                let mut line: Vec<_> = state.buffer.drain(..=index).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if let Some(event) = parse_log_line(&line) {
                    return Some((event, state));
                }
                continue;
            }
            if state.done {
                if state.buffer.is_empty() {
                    return None;
                }
                let tail = std::mem::take(&mut state.buffer);
                return parse_log_line(&tail).map(|event| (event, state));
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

fn parse_log_line(line: &[u8]) -> Option<Result<RemoteProcessEvent, BlaxelTransportError>> {
    if line.starts_with(b"[keepalive]") || line.is_empty() {
        return None;
    }
    let (stream, content) = if let Some(content) = line.strip_prefix(b"stdout:") {
        (RemoteStreamKind::Stdout, content)
    } else if let Some(content) = line.strip_prefix(b"stderr:") {
        (RemoteStreamKind::Stderr, content)
    } else {
        (RemoteStreamKind::Stdout, line)
    };
    let mut data = content.to_vec();
    data.push(b'\n');
    Some(Ok(RemoteProcessEvent::Output { stream, data }))
}

fn shell_command(command: &str, arguments: &[String]) -> String {
    std::iter::once(command)
        .chain(arguments.iter().map(String::as_str))
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn sanitize_name(value: &str) -> String {
    let value: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    value.trim_matches('-').chars().take(80).collect()
}

fn deserialize_pid<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Pid {
        Number(u32),
        String(String),
    }
    match Pid::deserialize(deserializer)? {
        Pid::Number(pid) => Ok(pid),
        Pid::String(pid) => pid.parse().map_err(serde::de::Error::custom),
    }
}

async fn checked_json<T: serde::de::DeserializeOwned>(
    response: Response,
) -> Result<T, BlaxelTransportError> {
    if !response.status().is_success() {
        return Err(response_error(response).await);
    }
    response.json().await.map_err(request_error)
}

async fn response_error(response: Response) -> BlaxelTransportError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    BlaxelTransportError {
        message: format!("Blaxel sandbox API returned {status}: {body}"),
        retryable: status.is_server_error() || status.as_u16() == 429,
        disconnected: false,
    }
}

fn request_error(source: reqwest::Error) -> BlaxelTransportError {
    BlaxelTransportError {
        message: source.to_string(),
        retryable: source.is_timeout() || source.is_connect(),
        disconnected: source.is_connect() || source.is_timeout() || source.is_body(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_commands_without_shell_injection() {
        assert_eq!(
            shell_command("python3", &["-c".to_owned(), "print('hello')".to_owned()]),
            "python3 -c 'print('\"'\"'hello'\"'\"')'"
        );
    }

    #[test]
    fn parses_blaxel_log_lines() {
        assert!(matches!(
            parse_log_line(b"stdout:hello"),
            Some(Ok(RemoteProcessEvent::Output {
                stream: RemoteStreamKind::Stdout,
                ref data,
            })) if data == b"hello\n"
        ));
        assert!(parse_log_line(b"[keepalive]").is_none());
    }

    #[test]
    fn maps_killed_process_exit() {
        let event = process_completion_event(ProcessInfo {
            pid: 9,
            status: "killed".to_owned(),
            exit_code: 0,
            command: "sleep".to_owned(),
            name: "sleep".to_owned(),
        });
        assert!(matches!(
            event,
            RemoteProcessEvent::Exited { exit_code: 137, .. }
        ));
    }

    #[test]
    fn accepts_string_process_ids() {
        let process: ProcessInfo = serde_json::from_value(json!({
            "pid": "123",
            "status": "running",
            "command": "sleep 1",
            "name": "sleep",
            "workingDir": "/tmp"
        }))
        .unwrap();
        assert_eq!(process.pid, 123);
    }
}
