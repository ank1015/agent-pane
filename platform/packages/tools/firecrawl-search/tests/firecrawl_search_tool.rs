use llm_contracts::{ContentPart, ToolArguments, ToolDefinition, Validate as _};
use reqwest::Client;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
use tool_firecrawl_search::{
    FirecrawlSearchArguments, FirecrawlSearchToolContext, definition, execute, execute_search_tool,
    parse_arguments,
};

#[test]
fn exports_the_opinionated_firecrawl_definition() {
    let definition = definition();
    assert_eq!(definition.name(), "search");
    definition.validate().expect("valid tool definition");
    let ToolDefinition::Function(tool) = definition else {
        panic!("search must be a function tool")
    };
    assert!(!tool.description.to_ascii_lowercase().contains("firecrawl"));
    assert_eq!(tool.parameters["required"], json!(["query"]));
    assert_eq!(tool.parameters["additionalProperties"], false);
    assert_eq!(tool.parameters["properties"].as_object().unwrap().len(), 1);
    assert_eq!(tool.parameters["properties"]["query"]["maxLength"], 500);
}

#[test]
fn parses_only_the_query_argument() {
    let Value::Object(arguments) = json!({ "query": "Rust async traits" }) else {
        unreachable!()
    };
    assert_eq!(
        parse_arguments(&ToolArguments::Object(arguments)).expect("valid arguments"),
        FirecrawlSearchArguments {
            query: "Rust async traits".to_owned()
        }
    );

    let Value::Object(arguments) = json!({ "query": "Rust", "limit": 100 }) else {
        unreachable!()
    };
    let error = parse_arguments(&ToolArguments::Object(arguments))
        .expect_err("unknown options must be rejected");
    assert_eq!(error.name(), "invalid_arguments");
}

#[tokio::test]
async fn sends_fixed_search_policy_and_formats_results() {
    let (endpoint, captured) = spawn_server(
        "200 OK",
        json!({
            "success": true,
            "data": {
                "web": [
                    {
                        "title": "Rust documentation",
                        "description": "The Rust standard library documentation.",
                        "url": "https://doc.rust-lang.org/std/"
                    },
                    {
                        "title": "Async traits",
                        "description": "Patterns for async functions in traits.",
                        "url": "https://docs.rs/async-trait"
                    }
                ]
            },
            "warning": "Search results may be incomplete",
            "id": "search-123",
            "creditsUsed": 1
        }),
    )
    .await;
    let context = FirecrawlSearchToolContext::with_client("secret-key", endpoint, Client::new())
        .expect("test context");
    let Value::Object(arguments) = json!({
        "query": "  Rust async traits site:docs.rs  "
    }) else {
        unreachable!()
    };

    let output = execute_search_tool(&ToolArguments::Object(arguments), &context)
        .await
        .expect("successful search");
    let raw_request = captured.await.expect("mock server completed");
    let lower = raw_request.to_ascii_lowercase();
    let body: Value = serde_json::from_str(
        raw_request
            .split_once("\r\n\r\n")
            .expect("HTTP request has a body")
            .1,
    )
    .expect("request body is JSON");

    assert!(raw_request.starts_with("POST /v2/search HTTP/1.1\r\n"));
    assert!(lower.contains("authorization: bearer secret-key\r\n"));
    assert_eq!(body["query"], "Rust async traits site:docs.rs");
    assert_eq!(body["limit"], 10);
    assert_eq!(body["sources"], json!(["web"]));
    assert_eq!(body["safe"], true);
    assert_eq!(body["timeout"], 30_000);
    assert_eq!(body["ignoreInvalidURLs"], true);
    assert_eq!(body["highlights"], true);
    assert!(body.get("scrapeOptions").is_none());

    let text = text(&output.content);
    assert!(text.contains("1. Rust documentation"));
    assert!(text.contains("URL: https://doc.rust-lang.org/std/"));
    assert!(text.contains("Warning: Search results may be incomplete"));
    let details = output.details.expect("structured details");
    assert_eq!(details["search_id"], "search-123");
    assert_eq!(details["credits_used"], 1);
    assert_eq!(details["results"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn validates_queries_before_sending_a_request() {
    let context = FirecrawlSearchToolContext::new("secret-key").expect("context");
    let error = execute(
        FirecrawlSearchArguments {
            query: "   ".to_owned(),
        },
        &context,
    )
    .await
    .expect_err("empty query must fail");
    assert_eq!(error.name(), "invalid_arguments");

    let error = execute(
        FirecrawlSearchArguments {
            query: "x".repeat(501),
        },
        &context,
    )
    .await
    .expect_err("long query must fail");
    assert_eq!(error.name(), "invalid_arguments");
}

#[tokio::test]
async fn exposes_structured_firecrawl_api_errors() {
    let (endpoint, captured) = spawn_server(
        "429 Too Many Requests",
        json!({
            "success": false,
            "code": "RATE_LIMITED",
            "error": "Too many requests"
        }),
    )
    .await;
    let context = FirecrawlSearchToolContext::with_client("secret-key", endpoint, Client::new())
        .expect("test context");

    let error = execute(
        FirecrawlSearchArguments {
            query: "Rust".to_owned(),
        },
        &context,
    )
    .await
    .expect_err("API error must fail");
    captured.await.expect("mock server completed");

    assert_eq!(error.name(), "api_error");
    assert_eq!(error.message(), "Too many requests");
    assert_eq!(error.details().unwrap()["http_status"], 429);
    assert_eq!(error.details().unwrap()["code"], "RATE_LIMITED");
}

#[tokio::test]
#[ignore = "requires FIRECRAWL_API_KEY and consumes Firecrawl credits"]
async fn live_search_reaches_firecrawl() {
    let context = FirecrawlSearchToolContext::from_env().expect("process Firecrawl credentials");
    let output = execute(
        FirecrawlSearchArguments {
            query: "Firecrawl search API site:docs.firecrawl.dev".to_owned(),
        },
        &context,
    )
    .await
    .expect("live Firecrawl search");

    let details = output.details.expect("structured details");
    assert_eq!(
        details["query"],
        "Firecrawl search API site:docs.firecrawl.dev"
    );
    assert!(
        details["results"]
            .as_array()
            .is_some_and(|results| !results.is_empty()),
        "live search should return at least one result"
    );
    assert!(text(&output.content).contains("URL: https://"));
}

fn text(content: &[ContentPart]) -> &str {
    let ContentPart::Text(text) = &content[0] else {
        panic!("first content part must be text")
    };
    &text.content
}

async fn spawn_server(status: &str, body: Value) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let address = listener.local_addr().expect("mock server address");
    let status = status.to_owned();
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

        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("write HTTP response");
        String::from_utf8(request).expect("HTTP request is UTF-8")
    });
    (format!("http://{address}/v2/search"), handle)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[test]
fn credentials_are_redacted_and_endpoint_configuration_is_validated() {
    let context = FirecrawlSearchToolContext::new("private-secret-marker").unwrap();
    assert!(!format!("{context:?}").contains("private-secret-marker"));
    for key in ["", " bad", "bad\nkey"] {
        assert!(FirecrawlSearchToolContext::new(key).is_err());
    }
    for endpoint in [
        "http://example.com/v2/search",
        "https://user:pass@example.com/v2/search",
        "https://example.com/v2/search?key=secret",
        "file:///tmp/test",
    ] {
        assert!(FirecrawlSearchToolContext::with_client("key", endpoint, Client::new()).is_err());
    }
}

#[tokio::test]
async fn oversized_model_text_and_metadata_are_bounded() {
    let (endpoint, captured) = spawn_server("200 OK", json!({"success":true,"data":{"web":[{"url":"https://example.com/","title":"é".repeat(90_000),"description":"large"}]}})).await;
    let ctx =
        FirecrawlSearchToolContext::with_client("secret-key", endpoint, Client::new()).unwrap();
    let output = execute(
        FirecrawlSearchArguments {
            query: "Rust".into(),
        },
        &ctx,
    )
    .await
    .unwrap();
    captured.await.unwrap();
    assert!(text(&output.content).len() <= tool_firecrawl_search::MAX_OUTPUT_TEXT_BYTES);
    assert!(text(&output.content).contains("tool output truncated"));
    let details = output.details.unwrap();
    assert!(
        serde_json::to_vec(&details).unwrap().len()
            <= tool_firecrawl_search::MAX_OUTPUT_DETAILS_BYTES
    );
    assert_eq!(details["truncated"], true);
}

#[tokio::test]
async fn chunked_responses_are_limited_without_content_length() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v2/search", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            assert!(headers.len() < 4096);
            headers.push(socket.read_u8().await.unwrap());
        }
        let length = String::from_utf8(headers)
            .unwrap()
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        assert!(length < 4096);
        socket.read_exact(&mut vec![0u8; length]).await.unwrap();
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let chunk = format!("{:x}\r\n{}\r\n", 65536, "x".repeat(65536));
        for _ in 0..34 {
            if socket.write_all(chunk.as_bytes()).await.is_err() {
                break;
            }
        }
        let _ = socket.write_all(b"0\r\n\r\n").await;
    });
    let ctx =
        FirecrawlSearchToolContext::with_client("secret-key", endpoint, Client::new()).unwrap();
    let error = execute(
        FirecrawlSearchArguments {
            query: "Rust".into(),
        },
        &ctx,
    )
    .await
    .unwrap_err();
    assert_eq!(error.name(), "invalid_response");
    assert!(error.message().contains("byte limit"));
    task.await.unwrap();
}
