use std::collections::BTreeMap;

use llm_contracts::{
    ContentPart, LlmRequest, LlmTransport, Message, MessageId, ModelId, ModelRef, ProviderId,
    TextContent, Timestamp, UserMessage,
};
use provider_deepseek::{DeepSeekConfig, DeepSeekProvider};
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

const PRO: &str = "deepseek-v4-pro";

fn request() -> LlmRequest {
    LlmRequest {
        model: ModelRef {
            provider: ProviderId::new("deepseek").expect("valid provider id"),
            id: ModelId::new(PRO).expect("valid model id"),
            name: None,
        },
        instructions: None,
        messages: vec![Message::User(UserMessage {
            id: MessageId::new("user-1").expect("valid id"),
            timestamp: Timestamp(1),
            content: vec![ContentPart::Text(TextContent {
                content: "Hello".into(),
                metadata: None,
            })],
        })],
        tools: Vec::new(),
        provider_options: Map::new(),
        metadata: BTreeMap::new(),
    }
}

#[tokio::test]
async fn complete_handles_keep_alive_whitespace_and_non_streaming_round_trip() {
    let native = json!({
        "id": "completion-http",
        "object": "chat.completion",
        "model": PRO,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hello" },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 2,
            "prompt_cache_hit_tokens": 3,
            "prompt_cache_miss_tokens": 7
        }
    });
    let response_body = format!("\n\n{}", serde_json::to_string(&native).expect("JSON"));
    let (base_url, captured) = spawn_server("200 OK", &[], response_body).await;
    let config = DeepSeekConfig::new("secret-key")
        .expect("valid config")
        .with_base_url(format!("{base_url}/v1"))
        .expect("valid test URL");
    let provider = DeepSeekProvider::new(config).expect("valid provider");

    let message = provider
        .complete(request())
        .await
        .expect("successful completion");
    let raw_request = captured.await.expect("mock server completed");
    let lower_headers = raw_request.to_ascii_lowercase();
    let body: Value = serde_json::from_str(
        raw_request
            .split_once("\r\n\r\n")
            .expect("HTTP request contains a body")
            .1,
    )
    .expect("request body is JSON");

    assert!(raw_request.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
    assert!(lower_headers.contains("authorization: bearer secret-key\r\n"));
    assert_eq!(body["model"], PRO);
    assert_eq!(body["stream"], false);
    assert_eq!(message.native_message, native);
    assert!(message.usage.expect("usage").cost.is_some());
}

#[tokio::test]
async fn complete_normalizes_retryable_http_errors() {
    let (base_url, captured) = spawn_server(
        "503 Service Unavailable",
        &[("retry-after", "2")],
        json!({
            "error": {
                "message": "Server overloaded",
                "type": "server_error",
                "code": "overloaded"
            }
        })
        .to_string(),
    )
    .await;
    let config = DeepSeekConfig::new("secret-key")
        .expect("valid config")
        .with_base_url(base_url)
        .expect("valid test URL");
    let provider = DeepSeekProvider::new(config).expect("valid provider");

    let error = provider
        .complete(request())
        .await
        .expect_err("server error must fail");
    captured.await.expect("mock server completed");

    assert_eq!(error.message, "Server overloaded");
    assert_eq!(error.http_status, Some(503));
    assert_eq!(error.provider_type.as_deref(), Some("server_error"));
    assert_eq!(error.provider_code.as_deref(), Some("overloaded"));
    assert_eq!(error.retry_after_ms, Some(2_000));
    assert!(error.can_retry);
}

async fn spawn_server(
    status: &str,
    headers: &[(&str, &str)],
    body: String,
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
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
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
