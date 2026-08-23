use std::{collections::BTreeMap, pin::Pin};

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream};
use reqwest::{Client, Response, header};
use serde_json::{Value, json};

use crate::{
    E2bConnectionConfig, E2bTransport, E2bTransportError, RemoteProcessEvent, RemoteProcessRequest,
    RemoteProcessStream, RemoteProcessSummary, RemoteStreamKind,
};

#[derive(Clone)]
pub struct E2bHttpTransport {
    client: Client,
    config: E2bConnectionConfig,
}

impl E2bHttpTransport {
    pub fn new(config: E2bConnectionConfig) -> Result<Self, E2bTransportError> {
        let client = Client::builder()
            .build()
            .map_err(|source| E2bTransportError::new(source.to_string()))?;
        Ok(Self { client, config })
    }

    fn endpoint(&self, service: &str, method: &str) -> Result<reqwest::Url, E2bTransportError> {
        self.config
            .sandbox_url
            .join(&format!("{service}/{method}"))
            .map_err(|source| E2bTransportError::new(source.to_string()))
    }

    fn request(
        &self,
        service: &str,
        method: &str,
    ) -> Result<reqwest::RequestBuilder, E2bTransportError> {
        let authorization = STANDARD.encode(format!("{}:", self.config.username));
        Ok(self
            .client
            .post(self.endpoint(service, method)?)
            .header("X-Access-Token", &self.config.envd_access_token)
            .header("E2b-Sandbox-Id", &self.config.sandbox_id)
            .header("E2b-Sandbox-Port", self.config.envd_port)
            .header("Connect-Protocol-Version", "1")
            .header(header::AUTHORIZATION, format!("Basic {authorization}")))
    }

    async fn unary(
        &self,
        service: &str,
        method: &str,
        body: Value,
    ) -> Result<Value, E2bTransportError> {
        let response = self
            .request(service, method)?
            .header(header::CONTENT_TYPE, "application/json")
            .timeout(std::time::Duration::from_millis(
                self.config.request_timeout_ms,
            ))
            .json(&body)
            .send()
            .await
            .map_err(request_error)?;
        checked_json(response).await
    }

    async fn server_stream(
        &self,
        method: &str,
        body: Value,
        timeout_ms: Option<u64>,
    ) -> Result<RemoteProcessStream, E2bTransportError> {
        let mut request = self
            .request("process.Process", method)?
            .header(header::CONTENT_TYPE, "application/connect+json")
            .header("Keepalive-Ping-Interval", "50")
            .body(connect_json_envelope(&body)?);
        if let Some(timeout) = timeout_ms.filter(|value| *value > 0) {
            request = request.header("Connect-Timeout-Ms", timeout);
        }
        let response = request.send().await.map_err(request_error)?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        Ok(decode_connect_stream(response))
    }
}

fn connect_json_envelope(value: &Value) -> Result<Vec<u8>, E2bTransportError> {
    let payload = serde_json::to_vec(value).map_err(|source| {
        E2bTransportError::new(format!("cannot encode Connect JSON: {source}"))
    })?;
    let length = u32::try_from(payload.len())
        .map_err(|_| E2bTransportError::new("Connect JSON request is too large"))?;
    let mut envelope = Vec::with_capacity(5 + payload.len());
    envelope.push(0);
    envelope.extend_from_slice(&length.to_be_bytes());
    envelope.extend_from_slice(&payload);
    Ok(envelope)
}

#[async_trait]
impl E2bTransport for E2bHttpTransport {
    async fn start_process(
        &self,
        request: RemoteProcessRequest,
    ) -> Result<RemoteProcessStream, E2bTransportError> {
        let mut body = json!({
            "process": {
                "cmd": request.command,
                "args": request.arguments,
                "envs": request.environment,
            },
            "stdin": request.stdin,
        });
        if let Some(cwd) = request.cwd {
            body["process"]["cwd"] = Value::String(cwd);
        }
        if let Some(tag) = request.tag {
            body["tag"] = Value::String(tag);
        }
        if let Some((columns, rows)) = request.pty {
            body["pty"] = json!({"size": {"cols": columns, "rows": rows}});
        }
        self.server_stream("Start", body, request.timeout_ms).await
    }

    async fn connect_process(
        &self,
        pid: u32,
        timeout_ms: Option<u64>,
    ) -> Result<RemoteProcessStream, E2bTransportError> {
        self.server_stream("Connect", json!({"process": {"pid": pid}}), timeout_ms)
            .await
    }

    async fn list_processes(&self) -> Result<Vec<RemoteProcessSummary>, E2bTransportError> {
        let value = self.unary("process.Process", "List", json!({})).await?;
        let processes = value
            .get("processes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        processes
            .into_iter()
            .map(|process| {
                let config = process.get("config").cloned().unwrap_or_else(|| json!({}));
                Ok(RemoteProcessSummary {
                    pid: u32_field(&process, "pid")?,
                    command: string_field(&config, "cmd")?.to_owned(),
                    arguments: config
                        .get("args")
                        .and_then(Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default(),
                    environment: config
                        .get("envs")
                        .and_then(Value::as_object)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(|(key, value)| {
                                    value.as_str().map(|value| (key.clone(), value.to_owned()))
                                })
                                .collect::<BTreeMap<_, _>>()
                        })
                        .unwrap_or_default(),
                    cwd: config.get("cwd").and_then(Value::as_str).map(str::to_owned),
                    tag: process
                        .get("tag")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            })
            .collect()
    }

    async fn send_input(&self, pid: u32, pty: bool, data: &[u8]) -> Result<(), E2bTransportError> {
        let encoded = STANDARD.encode(data);
        let input = if pty {
            json!({"pty": encoded})
        } else {
            json!({"stdin": encoded})
        };
        self.unary(
            "process.Process",
            "SendInput",
            json!({"process": {"pid": pid}, "input": input}),
        )
        .await?;
        Ok(())
    }

    async fn close_stdin(&self, pid: u32) -> Result<(), E2bTransportError> {
        self.unary(
            "process.Process",
            "CloseStdin",
            json!({"process": {"pid": pid}}),
        )
        .await?;
        Ok(())
    }

    async fn resize_pty(&self, pid: u32, columns: u16, rows: u16) -> Result<(), E2bTransportError> {
        self.unary(
            "process.Process",
            "Update",
            json!({"process": {"pid": pid}, "pty": {"size": {"cols": columns, "rows": rows}}}),
        )
        .await?;
        Ok(())
    }

    async fn signal_process(&self, pid: u32, kill: bool) -> Result<(), E2bTransportError> {
        self.unary(
            "process.Process",
            "SendSignal",
            json!({
                "process": {"pid": pid},
                "signal": if kill { "SIGNAL_SIGKILL" } else { "SIGNAL_SIGTERM" },
            }),
        )
        .await?;
        Ok(())
    }
}

fn decode_connect_stream(response: Response) -> RemoteProcessStream {
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
                if state.buffer.len() >= 5 + length {
                    let payload = state.buffer[5..5 + length].to_vec();
                    state.buffer.drain(..5 + length);
                    if flags & 0x02 != 0 {
                        // EndStreamResponse. Non-empty error trailers are surfaced below.
                        if let Ok(value) = serde_json::from_slice::<Value>(&payload)
                            && let Some(error) = value.get("error")
                        {
                            let message = error
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("E2B Connect stream failed");
                            return Some((Err(E2bTransportError::new(message)), state));
                        }
                        state.done = true;
                        continue;
                    }
                    let item = serde_json::from_slice::<Value>(&payload)
                        .map_err(|source| {
                            E2bTransportError::new(format!("invalid Connect JSON: {source}"))
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
                Some(Err(source)) => {
                    state.done = true;
                    return Some((Err(request_error(source)), state));
                }
                None if state.buffer.is_empty() => return None,
                None => {
                    state.done = true;
                    return Some((
                        Err(E2bTransportError::disconnected(
                            "E2B Connect stream ended with an incomplete frame",
                        )),
                        state,
                    ));
                }
            }
        }
    }))
}

fn parse_process_event(value: Value) -> Result<RemoteProcessEvent, E2bTransportError> {
    let event = value
        .get("event")
        .ok_or_else(|| E2bTransportError::new("E2B process response has no event"))?;
    if let Some(start) = event.get("start") {
        return Ok(RemoteProcessEvent::Started {
            pid: u32_field(start, "pid")?,
        });
    }
    if let Some(data) = event.get("data") {
        for (name, stream) in [
            ("stdout", RemoteStreamKind::Stdout),
            ("stderr", RemoteStreamKind::Stderr),
            ("pty", RemoteStreamKind::Pty),
        ] {
            if let Some(encoded) = data.get(name).and_then(Value::as_str) {
                let bytes = STANDARD.decode(encoded).map_err(|source| {
                    E2bTransportError::new(format!("invalid process output base64: {source}"))
                })?;
                return Ok(RemoteProcessEvent::Output {
                    stream,
                    data: bytes,
                });
            }
        }
    }
    if let Some(end) = event.get("end") {
        return Ok(RemoteProcessEvent::Exited {
            exit_code: end
                .get("exitCode")
                .or_else(|| end.get("exit_code"))
                .and_then(Value::as_i64)
                .unwrap_or_default() as i32,
            exited: end.get("exited").and_then(Value::as_bool).unwrap_or(false),
            status: end
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            error: end.get("error").and_then(Value::as_str).map(str::to_owned),
        });
    }
    if event.get("keepalive").is_some() {
        return Ok(RemoteProcessEvent::KeepAlive);
    }
    Err(E2bTransportError::new("unknown E2B process event"))
}

async fn checked_json(response: Response) -> Result<Value, E2bTransportError> {
    if !response.status().is_success() {
        return Err(response_error(response).await);
    }
    response.json().await.map_err(request_error)
}

async fn response_error(response: Response) -> E2bTransportError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    E2bTransportError {
        message: format!("E2B envd returned {status}: {body}"),
        retryable: status.is_server_error() || status.as_u16() == 429,
        disconnected: false,
    }
}

fn request_error(source: reqwest::Error) -> E2bTransportError {
    E2bTransportError {
        message: source.to_string(),
        retryable: source.is_timeout() || source.is_connect(),
        disconnected: source.is_connect() || source.is_timeout() || source.is_body(),
    }
}

fn string_field<'a>(value: &'a Value, field: &str) -> Result<&'a str, E2bTransportError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| E2bTransportError::new(format!("missing string field `{field}`")))
}

fn u32_field(value: &Value, field: &str) -> Result<u32, E2bTransportError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| E2bTransportError::new(format!("missing integer field `{field}`")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_process_json_events() {
        assert_eq!(
            parse_process_event(json!({"event": {"start": {"pid": 42}}})).unwrap(),
            RemoteProcessEvent::Started { pid: 42 }
        );
        assert_eq!(
            parse_process_event(json!({"event": {"data": {"stderr": "aGk="}}})).unwrap(),
            RemoteProcessEvent::Output {
                stream: RemoteStreamKind::Stderr,
                data: b"hi".to_vec(),
            }
        );
    }

    #[test]
    fn frames_streaming_connect_json_requests() {
        let envelope = connect_json_envelope(&json!({"process": {"cmd": "echo"}})).unwrap();
        assert_eq!(envelope[0], 0);
        let length = u32::from_be_bytes(envelope[1..5].try_into().unwrap()) as usize;
        assert_eq!(length, envelope.len() - 5);
        assert_eq!(
            serde_json::from_slice::<Value>(&envelope[5..]).unwrap(),
            json!({"process": {"cmd": "echo"}})
        );
    }
}
