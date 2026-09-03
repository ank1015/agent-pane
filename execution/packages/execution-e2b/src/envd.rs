use std::{collections::BTreeMap, pin::Pin, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream};
use reqwest::{Client, RequestBuilder, Response, header};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use url::Url;

use crate::{ConnectedSandbox, E2bConfig, E2bError, E2bResult};

const MAX_PROCESS_OUTPUT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct EnvdProcessRequest {
    pub command: String,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub cwd: Option<String>,
    pub tag: Option<String>,
    pub stdin: Vec<u8>,
    pub timeout: Option<Duration>,
}

impl EnvdProcessRequest {
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            cwd: None,
            tag: None,
            stdin: Vec::new(),
            timeout: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvdProcessOutput {
    pub pid: u32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct EnvdProcessInfo {
    pub config: EnvdProcessConfig,
    pub pid: u32,
    pub tag: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct EnvdProcessConfig {
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct EnvdProcessList {
    #[serde(default)]
    processes: Vec<EnvdProcessInfo>,
}

#[derive(Clone)]
pub struct E2bEnvdClient {
    client: Client,
    base_url: Url,
    sandbox_id: String,
    access_token: String,
    envd_port: u16,
    username: String,
    request_timeout: Duration,
}

impl std::fmt::Debug for E2bEnvdClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("E2bEnvdClient")
            .field("base_url", &self.base_url)
            .field("sandbox_id", &self.sandbox_id)
            .field("access_token", &"[REDACTED]")
            .field("envd_port", &self.envd_port)
            .field("username", &self.username)
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

impl E2bEnvdClient {
    pub fn new(config: &E2bConfig, connection: &ConnectedSandbox) -> E2bResult<Self> {
        let client = Client::builder()
            .build()
            .map_err(|error| E2bError::configuration(error.to_string()))?;
        Ok(Self {
            client,
            base_url: config.envd_base_url(connection.domain.as_deref())?,
            sandbox_id: connection.sandbox_id.clone(),
            access_token: connection.access_token().to_owned(),
            envd_port: config.envd_port,
            username: config.username.clone(),
            request_timeout: config.request_timeout,
        })
    }

    /// Verifies that envd is reachable after connect/resume completed.
    pub async fn health(&self) -> E2bResult<()> {
        let response = self
            .route(self.client.get(self.endpoint("health")?))
            .timeout(self.request_timeout)
            .send()
            .await
            .map_err(|error| E2bError::from_request(error, true))?;
        checked_status(response, true).await
    }

    /// Uploads a file, creating parent directories as defined by envd.
    pub async fn upload_file(&self, path: &str, bytes: Vec<u8>) -> E2bResult<()> {
        if !path.starts_with('/') || path.contains('\0') {
            return Err(E2bError::configuration(
                "envd upload path must be absolute and contain no null byte",
            ));
        }
        let file_name = path.rsplit('/').next().unwrap_or("file").to_owned();
        let part = reqwest::multipart::Part::bytes(bytes).file_name(file_name);
        let form = reqwest::multipart::Form::new().part("file", part);
        let response = self
            .route(self.client.post(self.endpoint("files")?))
            .query(&[("path", path), ("username", self.username.as_str())])
            .multipart(form)
            .timeout(self.request_timeout)
            .send()
            .await
            .map_err(|error| E2bError::from_request(error, true))?;
        checked_status(response, true).await
    }

    /// Runs a bounded command through envd and returns its complete output.
    pub async fn run_process(&self, request: EnvdProcessRequest) -> E2bResult<EnvdProcessOutput> {
        validate_process_request(&request)?;
        let has_stdin = !request.stdin.is_empty();
        let mut stream = self.start_stream(&request, has_stdin).await?;
        let mut pid = None;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit = None;

        while let Some(event) = stream.next().await {
            match event? {
                ProcessEvent::Started(value) => {
                    pid = Some(value);
                    if has_stdin {
                        self.send_input(value, &request.stdin).await?;
                        self.close_stdin(value).await?;
                    }
                }
                ProcessEvent::Output {
                    stderr: false,
                    data,
                } => {
                    append_bounded(&mut stdout, &data)?;
                }
                ProcessEvent::Output { stderr: true, data } => {
                    append_bounded(&mut stderr, &data)?;
                }
                ProcessEvent::Exited { exit_code, error } => {
                    if let Some(error) = error.filter(|value| !value.is_empty()) {
                        append_bounded(&mut stderr, error.as_bytes())?;
                    }
                    exit = Some(exit_code);
                    // EndEvent is the process protocol's terminal event. Do
                    // not wait for the HTTP/Connect transport to close after
                    // the command has semantically completed; envd may retain
                    // that stream well beyond process exit.
                    break;
                }
                ProcessEvent::KeepAlive => {}
            }
        }

        let pid = pid.ok_or_else(|| {
            E2bError::unavailable("E2B process stream ended before reporting a process ID")
        })?;
        let exit_code = exit.ok_or_else(|| {
            E2bError::unavailable(format!(
                "E2B process {pid} stream ended before the command completed"
            ))
        })?;
        Ok(EnvdProcessOutput {
            pid,
            stdout,
            stderr,
            exit_code,
        })
    }

    /// Starts a long-lived process and returns as soon as envd reports its PID.
    pub async fn start_detached(&self, request: EnvdProcessRequest) -> E2bResult<u32> {
        validate_process_request(&request)?;
        if !request.stdin.is_empty() {
            return Err(E2bError::configuration(
                "detached envd processes cannot receive initial stdin",
            ));
        }
        let mut stream = self.start_stream(&request, false).await?;
        while let Some(event) = stream.next().await {
            match event? {
                ProcessEvent::Started(pid) => return Ok(pid),
                ProcessEvent::Exited { exit_code, error } => {
                    return Err(E2bError::protocol(format!(
                        "detached process exited before start acknowledgement (exit {exit_code}): {}",
                        error.unwrap_or_default()
                    )));
                }
                ProcessEvent::Output { .. } | ProcessEvent::KeepAlive => {}
            }
        }
        Err(E2bError::unavailable(
            "E2B process stream ended before start acknowledgement",
        ))
    }

    pub(crate) async fn list_processes(&self) -> E2bResult<Vec<EnvdProcessInfo>> {
        let response: EnvdProcessList = self
            .unary_response("process.Process/List", json!({}))
            .await?;
        Ok(response.processes)
    }

    pub(crate) async fn kill_process(&self, pid: u32) -> E2bResult<()> {
        self.unary(
            "process.Process/SendSignal",
            json!({
                "process": {"pid": pid},
                "signal": "SIGNAL_SIGKILL",
            }),
        )
        .await
    }

    async fn start_stream(
        &self,
        request: &EnvdProcessRequest,
        stdin: bool,
    ) -> E2bResult<ProcessStream> {
        let mut body = json!({
            "process": {
                "cmd": request.command,
                "args": request.arguments,
                "envs": request.environment,
            },
            "stdin": stdin,
        });
        if let Some(cwd) = &request.cwd {
            body["process"]["cwd"] = Value::String(cwd.clone());
        }
        if let Some(tag) = &request.tag {
            body["tag"] = Value::String(tag.clone());
        }

        let payload = serde_json::to_vec(&body)?;
        let length = u32::try_from(payload.len())
            .map_err(|_| E2bError::configuration("envd process request is too large"))?;
        let mut envelope = Vec::with_capacity(payload.len() + 5);
        envelope.push(0);
        envelope.extend_from_slice(&length.to_be_bytes());
        envelope.extend_from_slice(&payload);

        let mut builder = self
            .route(self.client.post(self.endpoint("process.Process/Start")?))
            .header("Connect-Protocol-Version", "1")
            .header(header::CONTENT_TYPE, "application/connect+json")
            .header("Keepalive-Ping-Interval", "50")
            .header(
                header::AUTHORIZATION,
                format!("Basic {}", STANDARD.encode(format!("{}:", self.username))),
            )
            .body(envelope);
        if let Some(timeout) = request.timeout {
            builder = builder.header("Connect-Timeout-Ms", timeout.as_millis().to_string());
        }
        builder = builder.timeout(request.timeout.unwrap_or(self.request_timeout));
        let response = builder
            .send()
            .await
            .map_err(|error| E2bError::from_request(error, true))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(E2bError::from_response(status, &body, true));
        }
        Ok(decode_connect_stream(response))
    }

    async fn send_input(&self, pid: u32, bytes: &[u8]) -> E2bResult<()> {
        self.unary(
            "process.Process/SendInput",
            json!({
                "process": {"pid": pid},
                "input": {"stdin": STANDARD.encode(bytes)},
            }),
        )
        .await
    }

    async fn close_stdin(&self, pid: u32) -> E2bResult<()> {
        self.unary_allow_missing(
            "process.Process/CloseStdin",
            json!({"process": {"pid": pid}}),
        )
        .await
    }

    async fn unary(&self, endpoint: &str, body: Value) -> E2bResult<()> {
        self.unary_with_missing_policy(endpoint, body, false).await
    }

    async fn unary_allow_missing(&self, endpoint: &str, body: Value) -> E2bResult<()> {
        self.unary_with_missing_policy(endpoint, body, true).await
    }

    async fn unary_response<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        body: Value,
    ) -> E2bResult<T> {
        let response = self
            .unary_request(endpoint)?
            .json(&body)
            .send()
            .await
            .map_err(|error| E2bError::from_request(error, true))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(E2bError::from_response(status, &body, true));
        }
        response.json::<T>().await.map_err(|error| {
            E2bError::protocol(format!("invalid envd unary response JSON: {error}"))
        })
    }

    async fn unary_with_missing_policy(
        &self,
        endpoint: &str,
        body: Value,
        allow_missing: bool,
    ) -> E2bResult<()> {
        let response = self
            .unary_request(endpoint)?
            .json(&body)
            .send()
            .await
            .map_err(|error| E2bError::from_request(error, true))?;
        if allow_missing && response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        checked_status(response, true).await
    }

    fn unary_request(&self, endpoint: &str) -> E2bResult<RequestBuilder> {
        Ok(self
            .route(self.client.post(self.endpoint(endpoint)?))
            .header("Connect-Protocol-Version", "1")
            .header(header::CONTENT_TYPE, "application/json")
            .header(
                header::AUTHORIZATION,
                format!("Basic {}", STANDARD.encode(format!("{}:", self.username))),
            )
            .timeout(self.request_timeout))
    }

    fn route(&self, request: RequestBuilder) -> RequestBuilder {
        request
            .header("X-Access-Token", &self.access_token)
            .header("E2b-Sandbox-Id", &self.sandbox_id)
            .header("E2b-Sandbox-Port", self.envd_port)
    }

    fn endpoint(&self, path: &str) -> E2bResult<Url> {
        self.base_url
            .join(path)
            .map_err(|error| E2bError::configuration(error.to_string()))
    }
}

fn validate_process_request(request: &EnvdProcessRequest) -> E2bResult<()> {
    if request.command.trim().is_empty() || request.command.contains('\0') {
        return Err(E2bError::configuration(
            "envd process command must not be blank or contain a null byte",
        ));
    }
    if request
        .arguments
        .iter()
        .any(|argument| argument.contains('\0'))
    {
        return Err(E2bError::configuration(
            "envd process arguments must not contain null bytes",
        ));
    }
    Ok(())
}

fn append_bounded(target: &mut Vec<u8>, bytes: &[u8]) -> E2bResult<()> {
    if target.len().saturating_add(bytes.len()) > MAX_PROCESS_OUTPUT_BYTES {
        return Err(E2bError::protocol(format!(
            "envd process output exceeded {MAX_PROCESS_OUTPUT_BYTES} bytes"
        )));
    }
    target.extend_from_slice(bytes);
    Ok(())
}

async fn checked_status(response: Response, replayable: bool) -> E2bResult<()> {
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    Err(E2bError::from_response(status, &body, replayable))
}

#[derive(Debug, Eq, PartialEq)]
enum ProcessEvent {
    Started(u32),
    Output {
        stderr: bool,
        data: Vec<u8>,
    },
    Exited {
        exit_code: i32,
        error: Option<String>,
    },
    KeepAlive,
}

type ProcessStream = Pin<Box<dyn Stream<Item = E2bResult<ProcessEvent>> + Send + 'static>>;

fn decode_connect_stream(response: Response) -> ProcessStream {
    type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;
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
            if state.buffer.len() >= 5 {
                let flags = state.buffer[0];
                let length = u32::from_be_bytes([
                    state.buffer[1],
                    state.buffer[2],
                    state.buffer[3],
                    state.buffer[4],
                ]) as usize;
                if state.buffer.len() >= length + 5 {
                    let payload = state.buffer[5..length + 5].to_vec();
                    state.buffer.drain(..length + 5);
                    if flags & 0x01 != 0 {
                        return Some((
                            Err(E2bError::protocol(
                                "compressed Connect envelopes are not supported",
                            )),
                            state,
                        ));
                    }
                    if flags & 0x02 != 0 {
                        state.done = true;
                        if let Ok(value) = serde_json::from_slice::<Value>(&payload) {
                            if let Some(error) = connect_end_error(&value) {
                                return Some((Err(error), state));
                            }
                        }
                        continue;
                    }
                    let item = serde_json::from_slice::<Value>(&payload)
                        .map_err(|error| {
                            E2bError::protocol(format!("invalid envd Connect JSON: {error}"))
                        })
                        .and_then(parse_process_event);
                    return Some((item, state));
                }
            }
            if state.done {
                return None;
            }
            match state.source.next().await {
                Some(Ok(chunk)) => state.buffer.extend_from_slice(&chunk),
                Some(Err(error)) => {
                    state.done = true;
                    return Some((
                        Err(E2bError::unavailable(format!(
                            "E2B process stream failed: {error}"
                        ))),
                        state,
                    ));
                }
                None if state.buffer.is_empty() => return None,
                None => {
                    state.done = true;
                    return Some((
                        Err(E2bError::unavailable(
                            "E2B process stream ended with an incomplete frame",
                        )),
                        state,
                    ));
                }
            }
        }
    }))
}

fn connect_end_error(value: &Value) -> Option<E2bError> {
    let error = value.get("error").unwrap_or(value);
    let message = error.get("message")?.as_str()?;
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if code == "ok" || message.is_empty() {
        None
    } else if matches!(
        code,
        "unavailable" | "deadline_exceeded" | "resource_exhausted"
    ) || message.contains("ended before the stream completed")
    {
        Some(E2bError::unavailable(message))
    } else {
        Some(E2bError::protocol(message))
    }
}

fn parse_process_event(value: Value) -> E2bResult<ProcessEvent> {
    let event = value
        .get("event")
        .ok_or_else(|| E2bError::protocol("envd process response has no event"))?;
    if let Some(start) = event.get("start") {
        let pid = start
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| E2bError::protocol("envd start event has no valid PID"))?;
        return Ok(ProcessEvent::Started(pid));
    }
    if let Some(data) = event.get("data") {
        for (name, stderr) in [("stdout", false), ("pty", false), ("stderr", true)] {
            if let Some(encoded) = data.get(name).and_then(Value::as_str) {
                let data = STANDARD.decode(encoded).map_err(|error| {
                    E2bError::protocol(format!("invalid envd output base64: {error}"))
                })?;
                return Ok(ProcessEvent::Output { stderr, data });
            }
        }
    }
    if let Some(end) = event.get("end") {
        let exit_code = end
            .get("exitCode")
            .or_else(|| end.get("exit_code"))
            .and_then(Value::as_i64)
            .unwrap_or_default() as i32;
        return Ok(ProcessEvent::Exited {
            exit_code,
            error: end.get("error").and_then(Value::as_str).map(str::to_owned),
        });
    }
    if event.get("keepalive").is_some() {
        return Ok(ProcessEvent::KeepAlive);
    }
    Err(E2bError::protocol("unknown envd process event"))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{
        Json, Router,
        body::Body,
        extract::State,
        http::{HeaderMap, Response as HttpResponse, StatusCode},
        routing::post,
    };
    use tokio::net::TcpListener;

    use crate::{E2bApiKey, RetryPolicy};

    use super::*;

    #[test]
    fn parses_envd_events() {
        assert_eq!(
            parse_process_event(json!({"event":{"start":{"pid":42}}})).unwrap(),
            ProcessEvent::Started(42)
        );
        assert_eq!(
            parse_process_event(json!({"event":{"data":{"stderr":"aGk="}}})).unwrap(),
            ProcessEvent::Output {
                stderr: true,
                data: b"hi".to_vec()
            }
        );
    }

    #[test]
    fn stream_disconnect_phrase_is_retryable() {
        let error = connect_end_error(&json!({
            "error": {"code":"unknown", "message":"the connection to sandbox x ended before the stream completed"}
        }))
        .unwrap();
        assert!(error.retryable);
    }

    #[tokio::test]
    async fn runs_a_process_over_envd_connect_framing() {
        #[derive(Clone)]
        struct Calls(Arc<AtomicUsize>);

        async fn start(
            State(calls): State<Calls>,
            headers: HeaderMap,
            body: Bytes,
        ) -> HttpResponse<Body> {
            assert_eq!(headers["e2b-sandbox-id"], "sandbox-1");
            assert_eq!(headers["x-access-token"], "access-1");
            assert_eq!(headers["connect-protocol-version"], "1");
            let payload = decode_request_envelope(&body);
            assert_eq!(payload["process"]["cmd"], "/bin/echo");
            assert_eq!(payload["process"]["args"], json!(["hello"]));
            assert_eq!(payload["stdin"], true);
            calls.0.fetch_add(1, Ordering::SeqCst);

            let mut bytes = Vec::new();
            bytes.extend(connect_response_frame(
                json!({"event":{"start":{"pid":7}}}),
                0,
            ));
            bytes.extend(connect_response_frame(
                json!({"event":{"data":{"stdout":"aGVsbG8K"}}}),
                0,
            ));
            bytes.extend(connect_response_frame(
                json!({"event":{"end":{"exitCode":0,"exited":true,"status":"exited"}}}),
                0,
            ));
            // A terminal process event must be sufficient even if the
            // transport has not produced its own terminal frame yet.
            bytes.extend([0, 0]);
            HttpResponse::builder()
                .status(200)
                .header("content-type", "application/connect+json")
                .body(Body::from(bytes))
                .unwrap()
        }

        async fn unary(State(calls): State<Calls>, headers: HeaderMap) -> &'static str {
            assert_eq!(headers["e2b-sandbox-id"], "sandbox-1");
            calls.0.fetch_add(1, Ordering::SeqCst);
            "{}"
        }

        async fn already_exited(State(calls): State<Calls>) -> StatusCode {
            calls.0.fetch_add(1, Ordering::SeqCst);
            StatusCode::NOT_FOUND
        }

        async fn list(State(calls): State<Calls>) -> Json<Value> {
            calls.0.fetch_add(1, Ordering::SeqCst);
            Json(json!({
                "processes": [{
                    "config": {"cmd":"/usr/local/bin/execution-supervisor", "args":["serve"]},
                    "pid": 91,
                    "tag": "execution-supervisor-host-1"
                }]
            }))
        }

        async fn signal(State(calls): State<Calls>, Json(body): Json<Value>) -> &'static str {
            assert_eq!(
                body,
                json!({"process":{"pid":91}, "signal":"SIGNAL_SIGKILL"})
            );
            calls.0.fetch_add(1, Ordering::SeqCst);
            "{}"
        }

        let calls = Calls(Arc::new(AtomicUsize::new(0)));
        let app = Router::new()
            .route("/process.Process/Start", post(start))
            .route("/process.Process/SendInput", post(unary))
            .route("/process.Process/CloseStdin", post(already_exited))
            .route("/process.Process/List", post(list))
            .route("/process.Process/SendSignal", post(signal))
            .with_state(calls.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut config = E2bConfig::new(E2bApiKey::new("control-key").unwrap()).unwrap();
        config.envd_base_url_override = Some(Url::parse(&format!("http://{address}/")).unwrap());
        config.retry = RetryPolicy::default();
        let connection = ConnectedSandbox {
            sandbox_id: "sandbox-1".to_owned(),
            domain: None,
            envd_access_token: "access-1".to_owned(),
        };
        let envd = E2bEnvdClient::new(&config, &connection).unwrap();
        let mut request = EnvdProcessRequest::new("/bin/echo");
        request.arguments.push("hello".to_owned());
        request.stdin = b"input".to_vec();

        let output = envd.run_process(request).await.unwrap();
        assert_eq!(output.pid, 7);
        assert_eq!(output.stdout, b"hello\n");
        assert_eq!(output.exit_code, 0);
        let processes = envd.list_processes().await.unwrap();
        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].pid, 91);
        assert_eq!(processes[0].config.args, ["serve"]);
        envd.kill_process(91).await.unwrap();
        assert_eq!(calls.0.load(Ordering::SeqCst), 5);
    }

    fn decode_request_envelope(bytes: &[u8]) -> Value {
        assert!(bytes.len() >= 5);
        assert_eq!(bytes[0], 0);
        let length = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
        assert_eq!(bytes.len(), length + 5);
        serde_json::from_slice(&bytes[5..]).unwrap()
    }

    fn connect_response_frame(value: Value, flags: u8) -> Vec<u8> {
        let payload = serde_json::to_vec(&value).unwrap();
        let mut bytes = Vec::with_capacity(payload.len() + 5);
        bytes.push(flags);
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&payload);
        bytes
    }
}
