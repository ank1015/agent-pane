use std::collections::BTreeMap;

use llm_contracts::{
    LlmRequest, LlmTransport, ModelId, ModelRef, ProviderId, SearchCommands, SearchQuery,
    SearchRequest, SearchRequestOptions, SearchTransport,
};
use provider_openai::{CODEX_RESPONSES_LITE_OPTION, OpenAiConfig, OpenAiProvider};
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

fn request(model_id: &str) -> LlmRequest {
    LlmRequest {
        model: ModelRef {
            provider: ProviderId::new("openai").expect("valid provider id"),
            id: ModelId::new(model_id).expect("valid model id"),
            name: None,
        },
        instructions: None,
        messages: Vec::new(),
        tools: Vec::new(),
        provider_options: Map::new(),
        metadata: BTreeMap::new(),
    }
}

#[tokio::test]
async fn complete_performs_a_non_streaming_http_round_trip() {
    let native = json!({
        "id": "response-http",
        "object": "response",
        "model": "resolved-snapshot",
        "status": "completed",
        "output": [{
            "type": "message",
            "content": [{ "type": "output_text", "text": "Hello" }]
        }],
        "usage": {
            "input_tokens": 10,
            "output_tokens": 2,
            "input_tokens_details": { "cached_tokens": 3 }
        }
    });
    let (base_url, captured) = spawn_server("200 OK", &[], native.clone()).await;
    let config = OpenAiConfig::new("secret-key")
        .expect("valid config")
        .with_base_url(format!("{base_url}/v1"))
        .expect("valid test URL")
        .with_organization("org-1")
        .with_project("project-1");
    let provider = OpenAiProvider::new(config).expect("valid provider");

    let message = provider
        .complete(request("gpt-5.6-luna"))
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

    assert!(raw_request.starts_with("POST /v1/responses HTTP/1.1\r\n"));
    assert!(lower_headers.contains("authorization: bearer secret-key\r\n"));
    assert!(lower_headers.contains("openai-organization: org-1\r\n"));
    assert!(lower_headers.contains("openai-project: project-1\r\n"));
    assert!(body.get("stream").is_none());
    assert_eq!(body["model"], "gpt-5.6-luna");
    assert_eq!(message.native_message, native);
    assert!(message.usage.expect("usage").cost.is_some());
}

#[tokio::test]
async fn complete_normalizes_retryable_http_errors() {
    let (base_url, captured) = spawn_server(
        "429 Too Many Requests",
        &[("retry-after", "1.5")],
        json!({
            "error": {
                "message": "Slow down",
                "type": "rate_limit",
                "code": "limited"
            }
        }),
    )
    .await;
    let config = OpenAiConfig::new("secret-key")
        .expect("valid config")
        .with_base_url(format!("{base_url}/v1"))
        .expect("valid test URL");
    let provider = OpenAiProvider::new(config).expect("valid provider");

    let error = provider
        .complete(request("gpt-5.6-luna"))
        .await
        .expect_err("rate limit must fail");
    captured.await.expect("mock server completed");

    assert_eq!(error.message, "Slow down");
    assert_eq!(error.http_status, Some(429));
    assert_eq!(error.provider_type.as_deref(), Some("rate_limit"));
    assert_eq!(error.provider_code.as_deref(), Some("limited"));
    assert_eq!(error.retry_after_ms, Some(1_500));
    assert!(error.can_retry);
}

#[tokio::test]
async fn responses_lite_sends_the_native_codex_header() {
    let native = json!({
        "id": "response-lite",
        "object": "response",
        "model": "gpt-5.6-sol",
        "status": "completed",
        "output": [],
        "usage": {"input_tokens": 1, "output_tokens": 0}
    });
    let (base_url, captured) = spawn_server("200 OK", &[], native).await;
    let provider = OpenAiProvider::new(
        OpenAiConfig::new("secret-key")
            .expect("valid config")
            .with_base_url(format!("{base_url}/v1"))
            .expect("valid test URL"),
    )
    .expect("valid provider");
    let mut request = request("gpt-5.6-sol");
    request
        .provider_options
        .insert(CODEX_RESPONSES_LITE_OPTION.to_owned(), json!(true));

    provider
        .complete(request)
        .await
        .expect("successful response");
    let raw_request = captured.await.expect("mock server completed");
    assert!(
        raw_request
            .to_ascii_lowercase()
            .contains("x-openai-internal-codex-responses-lite: true\r\n")
    );
}

#[tokio::test]
async fn search_posts_the_codex_wire_contract_and_forwards_metadata() {
    let native = json!({
        "encrypted_output": "ciphertext",
        "output": "search result",
        "results": [{"type": "computer_initialize_state", "id": "1"}]
    });
    let (base_url, captured) = spawn_server("200 OK", &[], native.clone()).await;
    let provider = OpenAiProvider::new(
        OpenAiConfig::new("secret-key")
            .expect("valid config")
            .with_base_url(format!("{base_url}/v1"))
            .expect("valid test URL")
            .with_organization("org-1")
            .with_project("project-1"),
    )
    .expect("valid provider");
    let search = SearchRequest {
        id: "session-1".into(),
        model: "gpt-5.6-luna".into(),
        reasoning: None,
        input: None,
        commands: Some(SearchCommands {
            search_query: Some(vec![SearchQuery {
                q: "OpenAI Codex".into(),
                recency: Some(7),
                domains: Some(vec!["openai.com".into()]),
            }]),
            ..Default::default()
        }),
        settings: None,
        max_output_tokens: Some(2048),
    };

    let response = provider
        .search(
            search,
            SearchRequestOptions {
                originator: Some("codex_chatgpt_desktop".into()),
                codex_turn_metadata: Some(r#"{"turn_id":"turn-1"}"#.into()),
            },
        )
        .await
        .expect("successful search");
    let raw_request = captured.await.expect("mock server completed");
    let lower = raw_request.to_ascii_lowercase();
    let body: Value = serde_json::from_str(
        raw_request
            .split_once("\r\n\r\n")
            .expect("HTTP request contains a body")
            .1,
    )
    .expect("request body is JSON");

    assert!(raw_request.starts_with("POST /v1/alpha/search HTTP/1.1\r\n"));
    assert!(lower.contains("authorization: bearer secret-key\r\n"));
    assert!(lower.contains("originator: codex_chatgpt_desktop\r\n"));
    assert!(lower.contains("x-codex-turn-metadata: {\"turn_id\":\"turn-1\"}\r\n"));
    assert!(lower.contains("openai-organization: org-1\r\n"));
    assert!(lower.contains("openai-project: project-1\r\n"));
    assert_eq!(body["id"], "session-1");
    assert_eq!(body["model"], "gpt-5.6-luna");
    assert_eq!(body["commands"]["search_query"][0]["q"], "OpenAI Codex");
    assert!(body.get("provider").is_none());
    assert_eq!(response.output, "search result");
    assert_eq!(response.results, native["results"].as_array().cloned());
}

async fn spawn_server(
    status: &str,
    headers: &[(&str, &str)],
    body: Value,
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
    let body = serde_json::to_string(&body).expect("serializable mock body");
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
            if expected_length.is_none() {
                if let Some(header_end) = find_bytes(&request, b"\r\n\r\n") {
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
