use llm_contracts::{ContentPart, ToolArguments, ToolDefinition, Validate as _};
use reqwest::Client;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
use tool_firecrawl_scrape::{
    FirecrawlScrapeArguments, FirecrawlScrapeToolContext, definition, execute, execute_scrape_tool,
    parse_arguments,
};

#[test]
fn exports_the_opinionated_scrape_definition() {
    let definition = definition();
    assert_eq!(definition.name(), "scrape");
    definition.validate().expect("valid tool definition");
    let ToolDefinition::Function(tool) = definition else {
        panic!("scrape must be a function tool")
    };
    assert!(!tool.description.to_ascii_lowercase().contains("firecrawl"));
    assert_eq!(tool.parameters["required"], json!(["url"]));
    assert_eq!(tool.parameters["additionalProperties"], false);
    assert_eq!(tool.parameters["properties"].as_object().unwrap().len(), 1);
    assert_eq!(tool.parameters["properties"]["url"]["format"], "uri");
    assert_eq!(tool.parameters["properties"]["url"]["maxLength"], 4096);
}

#[test]
fn parses_only_the_url_argument() {
    let Value::Object(arguments) = json!({ "url": "https://example.com/page" }) else {
        unreachable!()
    };
    assert_eq!(
        parse_arguments(&ToolArguments::Object(arguments)).expect("valid arguments"),
        FirecrawlScrapeArguments {
            url: "https://example.com/page".to_owned()
        }
    );

    let Value::Object(arguments) = json!({
        "url": "https://example.com/page",
        "formats": ["html"]
    }) else {
        unreachable!()
    };
    let error = parse_arguments(&ToolArguments::Object(arguments))
        .expect_err("unknown options must be rejected");
    assert_eq!(error.name(), "invalid_arguments");
}

#[tokio::test]
async fn sends_fixed_scrape_policy_and_formats_markdown() {
    let (endpoint, captured) = spawn_server(
        "200 OK",
        json!({
            "success": true,
            "data": {
                "markdown": "# Example page\n\nUseful content with a [link](https://example.com/docs).",
                "metadata": {
                    "title": "Example page",
                    "description": "An example description",
                    "language": "en",
                    "sourceURL": "https://example.com/final",
                    "statusCode": 200,
                    "contentType": "text/html"
                },
                "warning": "Some content was omitted"
            }
        }),
    )
    .await;
    let context = FirecrawlScrapeToolContext::with_client("secret-key", endpoint, Client::new())
        .expect("test context");
    let Value::Object(arguments) = json!({
        "url": "  https://example.com/article  "
    }) else {
        unreachable!()
    };

    let output = execute_scrape_tool(&ToolArguments::Object(arguments), &context)
        .await
        .expect("successful scrape");
    let raw_request = captured.await.expect("mock server completed");
    let lower = raw_request.to_ascii_lowercase();
    let body: Value = serde_json::from_str(
        raw_request
            .split_once("\r\n\r\n")
            .expect("HTTP request has a body")
            .1,
    )
    .expect("request body is JSON");

    assert!(raw_request.starts_with("POST /v2/scrape HTTP/1.1\r\n"));
    assert!(lower.contains("authorization: bearer secret-key\r\n"));
    assert_eq!(body["url"], "https://example.com/article");
    assert_eq!(body["formats"], json!(["markdown"]));
    assert_eq!(body["onlyMainContent"], true);
    assert_eq!(body["onlyCleanContent"], false);
    assert_eq!(body["maxAge"], 3_600_000);
    assert_eq!(body["timeout"], 60_000);
    assert_eq!(body["parsers"], json!(["pdf"]));
    assert_eq!(body["removeBase64Images"], true);
    assert_eq!(body["blockAds"], true);
    assert_eq!(body["proxy"], "auto");
    assert_eq!(body["storeInCache"], true);
    assert!(body.get("headers").is_none());
    assert!(body.get("actions").is_none());

    let text = text(&output.content);
    assert!(text.starts_with("Source: https://example.com/final\nTitle: Example page\n"));
    assert!(text.contains("# Example page"));
    assert!(text.contains("Warning: Some content was omitted"));
    let details = output.details.expect("structured details");
    assert_eq!(details["url"], "https://example.com/article");
    assert_eq!(details["source_url"], "https://example.com/final");
    assert_eq!(details["content_type"], "text/html");
    assert_eq!(details["status_code"], 200);
    assert_eq!(details["truncated"], false);
}

#[tokio::test]
async fn validates_public_http_urls_before_sending_a_request() {
    let context = FirecrawlScrapeToolContext::new("secret-key").expect("context");
    for url in [
        "",
        "not a URL",
        "file:///etc/passwd",
        "https://user:password@example.com/private",
    ] {
        let error = execute(
            FirecrawlScrapeArguments {
                url: url.to_owned(),
            },
            &context,
        )
        .await
        .expect_err("invalid URL must fail");
        assert_eq!(error.name(), "invalid_arguments");
    }
}

#[tokio::test]
async fn falls_back_to_pdf_pages_and_truncates_utf8_safely() {
    let long_markdown = "é".repeat(30_000);
    let (endpoint, captured) = spawn_server(
        "200 OK",
        json!({
            "success": true,
            "data": {
                "pages": [{"pageNumber": 1, "markdown": long_markdown}],
                "metadata": {"sourceURL": "https://example.com/document.pdf"}
            }
        }),
    )
    .await;
    let context = FirecrawlScrapeToolContext::with_client("secret-key", endpoint, Client::new())
        .expect("test context");

    let output = execute(
        FirecrawlScrapeArguments {
            url: "https://example.com/document.pdf".to_owned(),
        },
        &context,
    )
    .await
    .expect("successful PDF scrape");
    captured.await.expect("mock server completed");

    assert!(text(&output.content).contains("[... page content truncated ...]"));
    let details = output.details.expect("structured details");
    assert_eq!(details["truncated"], true);
    assert!(details["original_markdown_bytes"].as_u64().unwrap() > 50 * 1_024);
    assert!(details["returned_markdown_bytes"].as_u64().unwrap() <= 51 * 1_024);
}

#[tokio::test]
async fn exposes_structured_scrape_api_errors_without_a_success_field() {
    let (endpoint, captured) = spawn_server(
        "402 Payment Required",
        json!({"error": "Payment required to access this resource."}),
    )
    .await;
    let context = FirecrawlScrapeToolContext::with_client("secret-key", endpoint, Client::new())
        .expect("test context");

    let error = execute(
        FirecrawlScrapeArguments {
            url: "https://example.com".to_owned(),
        },
        &context,
    )
    .await
    .expect_err("API error must fail");
    captured.await.expect("mock server completed");

    assert_eq!(error.name(), "api_error");
    assert_eq!(error.message(), "Payment required to access this resource.");
    assert_eq!(error.details().unwrap()["http_status"], 402);
}

#[tokio::test]
#[ignore = "requires packages/tool-firecrawl-scrape/.env and consumes Firecrawl credits"]
async fn live_scrape_reaches_firecrawl() {
    let context =
        FirecrawlScrapeToolContext::from_package_env().expect("package-local credentials");
    let output = execute(
        FirecrawlScrapeArguments {
            url: "https://example.com/".to_owned(),
        },
        &context,
    )
    .await
    .expect("live webpage scrape");

    let details = output.details.expect("structured details");
    assert_eq!(details["url"], "https://example.com/");
    assert!(
        details["original_markdown_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0)
    );
    assert!(text(&output.content).contains("Example Domain"));
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
    (format!("http://{address}/v2/scrape"), handle)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
