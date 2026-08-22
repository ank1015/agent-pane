use std::collections::BTreeMap;

use llm_contracts::{LlmRequest, LlmTransport, ModelId, ModelRef, ProviderId};
use provider_chatgpt::{ChatGptConfig, ChatGptProvider};
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

fn request() -> LlmRequest {
    LlmRequest {
        model: ModelRef {
            provider: ProviderId::new("chatgpt").expect("valid provider id"),
            id: ModelId::new("gpt-5.6-luna").expect("valid model id"),
            name: None,
        },
        instructions: Some("Be concise".into()),
        messages: Vec::new(),
        tools: Vec::new(),
        provider_options: Map::new(),
        metadata: BTreeMap::new(),
    }
}

#[tokio::test]
async fn complete_buffers_sse_and_sends_chatgpt_credentials() {
    let sse = sse_body(&[
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "id": "message-1",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "Hello" }]
            }
        }),
        json!({
            "type": "response.completed",
            "response": {
                "id": "response-http",
                "model": "gpt-5.6-luna",
                "status": "completed",
                "usage": { "input_tokens": 5, "output_tokens": 1 }
            }
        }),
    ]);
    let (base_url, captured) = spawn_server("200 OK", &[], &sse, "text/event-stream").await;
    let config = ChatGptConfig::new("oauth-token", "account-123")
        .expect("valid config")
        .with_base_url(format!("{base_url}/backend-api"))
        .expect("valid test URL");
    let provider = ChatGptProvider::new(config).expect("valid provider");

    let message = provider
        .complete(request())
        .await
        .expect("successful response");
    let raw_request = captured.await.expect("mock server completed");
    let lower = raw_request.to_ascii_lowercase();
    let body: Value =
        serde_json::from_str(raw_request.split_once("\r\n\r\n").expect("request body").1)
            .expect("JSON request");

    assert!(raw_request.starts_with("POST /backend-api/codex/responses HTTP/1.1\r\n"));
    assert!(lower.contains("authorization: bearer oauth-token\r\n"));
    assert!(lower.contains("chatgpt-account-id: account-123\r\n"));
    assert!(lower.contains("originator: agent-pane\r\n"));
    assert!(lower.contains("accept: text/event-stream\r\n"));
    assert!(lower.contains("openai-beta: responses=experimental\r\n"));
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(message.id.as_str(), "response-http");
    assert_eq!(
        message.native_message["events"].as_array().map(Vec::len),
        Some(2)
    );
}

#[tokio::test]
async fn complete_normalizes_http_errors() {
    let body = json!({
        "error": {
            "message": "Usage limit reached",
            "type": "usage_limit_reached",
            "code": "limited"
        }
    })
    .to_string();
    let (base_url, captured) = spawn_server(
        "429 Too Many Requests",
        &[("retry-after", "1.5")],
        &body,
        "application/json",
    )
    .await;
    let provider = ChatGptProvider::new(
        ChatGptConfig::new("oauth-token", "account-123")
            .expect("valid config")
            .with_base_url(base_url)
            .expect("valid test URL"),
    )
    .expect("valid provider");

    let error = provider
        .complete(request())
        .await
        .expect_err("rate limit must fail");
    captured.await.expect("mock server completed");

    assert_eq!(error.message, "Usage limit reached");
    assert_eq!(error.http_status, Some(429));
    assert_eq!(error.provider_code.as_deref(), Some("limited"));
    assert_eq!(error.retry_after_ms, Some(1_500));
    assert!(error.can_retry);
}

fn sse_body(events: &[Value]) -> String {
    events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

async fn spawn_server(
    status: &str,
    headers: &[(&str, &str)],
    body: &str,
    content_type: &str,
) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let address = listener.local_addr().expect("mock server address");
    let status = status.to_owned();
    let headers = headers
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect::<Vec<_>>();
    let body = body.to_owned();
    let content_type = content_type.to_owned();
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept HTTP request");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4_096];
        let mut expected_length = None;
        loop {
            let read = socket.read(&mut buffer).await.expect("read HTTP request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if expected_length.is_none()
                && let Some(header_end) = find_bytes(&request, b"\r\n\r\n")
            {
                let header_text = String::from_utf8_lossy(&request[..header_end]);
                let content_length = header_text
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                expected_length = Some(header_end + 4 + content_length);
            }
            if expected_length.is_some_and(|length| request.len() >= length) {
                break;
            }
        }

        let mut response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            response.push_str(&format!("{name}: {value}\r\n"));
        }
        response.push_str("\r\n");
        response.push_str(&body);
        socket
            .write_all(response.as_bytes())
            .await
            .expect("write HTTP response");
        String::from_utf8(request).expect("HTTP request is UTF-8")
    });
    (format!("http://{address}"), handle)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
