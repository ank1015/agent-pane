//! Opinionated model-facing webpage scraping backed by Firecrawl.
//!
//! The model controls only the public URL. This crate fixes extraction format,
//! content filtering, caching, proxy, timeout, and output-size policies.

use std::{env, path::PathBuf, time::Duration};

use llm_contracts::{ContentPart, FunctionTool, TextContent, ToolArguments, ToolDefinition};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use url::Url;

/// Name used in the model-facing tool definition.
pub const TOOL_NAME: &str = "scrape";
/// Environment variable containing the Firecrawl bearer token.
pub const API_KEY_ENV: &str = "FIRECRAWL_API_KEY";
/// Firecrawl v2 scrape endpoint.
pub const DEFAULT_SCRAPE_ENDPOINT: &str = "https://api.firecrawl.dev/v2/scrape";
/// Maximum age of cached content accepted by the tool.
pub const MAX_CACHE_AGE_MS: u64 = 60 * 60 * 1_000;
/// Timeout sent to Firecrawl in the scrape request.
pub const FIRECRAWL_TIMEOUT_MS: u64 = 60_000;
/// Maximum Markdown payload returned to the model before head-tail truncation.
pub const MAX_MARKDOWN_BYTES: usize = 50 * 1_024;

const HTTP_TIMEOUT: Duration = Duration::from_secs(65);
const MAX_RESPONSE_BYTES: usize = 16 * 1_024 * 1_024;
const MARKDOWN_HEAD_BYTES: usize = 40 * 1_024;
const MARKDOWN_TAIL_BYTES: usize = 10 * 1_024;
const TRUNCATION_MARKER: &str = "\n\n[... page content truncated ...]\n\n";

/// Typed arguments accepted by the scrape tool.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirecrawlScrapeArguments {
    pub url: String,
}

/// HTTP and credential inputs used to execute webpage scrapes.
#[derive(Clone, Debug)]
pub struct FirecrawlScrapeToolContext {
    http: Client,
    api_key: String,
    endpoint: Url,
}

impl FirecrawlScrapeToolContext {
    /// Creates a context using the public Firecrawl v2 endpoint.
    pub fn new(api_key: impl Into<String>) -> Result<Self, FirecrawlScrapeToolError> {
        let http = Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(FirecrawlScrapeToolError::configuration)?;
        Self::with_client(api_key, DEFAULT_SCRAPE_ENDPOINT, http)
    }

    /// Loads `FIRECRAWL_API_KEY` from the process environment or this crate's
    /// package-local `.env` file, in that order.
    pub fn from_package_env() -> Result<Self, FirecrawlScrapeToolError> {
        if let Ok(api_key) = env::var(API_KEY_ENV)
            && !api_key.trim().is_empty()
        {
            return Self::new(api_key);
        }

        let path = package_env_path();
        dotenvy::from_path(&path).map_err(|error| {
            FirecrawlScrapeToolError::configuration(format!(
                "failed to load {}: {error}",
                path.display()
            ))
        })?;
        let api_key = env::var(API_KEY_ENV).map_err(|_| {
            FirecrawlScrapeToolError::configuration(format!(
                "{API_KEY_ENV} is missing from {}",
                path.display()
            ))
        })?;
        Self::new(api_key)
    }

    /// Creates a context with an injected endpoint and HTTP client. This is
    /// useful for tests and Firecrawl-compatible deployments.
    pub fn with_client(
        api_key: impl Into<String>,
        endpoint: impl AsRef<str>,
        http: Client,
    ) -> Result<Self, FirecrawlScrapeToolError> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(FirecrawlScrapeToolError::configuration(format!(
                "{API_KEY_ENV} must not be empty"
            )));
        }
        let endpoint = Url::parse(endpoint.as_ref()).map_err(|error| {
            FirecrawlScrapeToolError::configuration(format!("invalid scrape endpoint: {error}"))
        })?;
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(FirecrawlScrapeToolError::configuration(
                "scrape endpoint must use http or https",
            ));
        }
        Ok(Self {
            http,
            api_key,
            endpoint,
        })
    }

    #[must_use]
    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }
}

/// Successful model-facing output from a webpage scrape.
#[derive(Clone, Debug, PartialEq)]
pub struct FirecrawlScrapeToolOutput {
    pub content: Vec<ContentPart>,
    pub details: Option<Value>,
}

impl FirecrawlScrapeToolOutput {
    fn new(text: String, details: Value) -> Self {
        Self {
            content: vec![ContentPart::Text(TextContent {
                content: text,
                metadata: None,
            })],
            details: Some(details),
        }
    }
}

/// Stable structured failure that a harness can map into its tool result.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct FirecrawlScrapeToolError {
    name: &'static str,
    message: String,
    details: Option<Value>,
}

impl FirecrawlScrapeToolError {
    fn new(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            message: message.into(),
            details: None,
        }
    }

    fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    fn invalid_arguments(message: impl std::fmt::Display) -> Self {
        Self::new(
            "invalid_arguments",
            format!("Invalid arguments for {TOOL_NAME}: {message}"),
        )
    }

    fn configuration(message: impl std::fmt::Display) -> Self {
        Self::new("configuration_error", message.to_string())
    }

    fn request(error: reqwest::Error) -> Self {
        let name = if error.is_timeout() {
            "timeout"
        } else {
            "request_error"
        };
        Self::new(name, format!("Webpage scrape request failed: {error}"))
    }

    fn invalid_response(message: impl Into<String>) -> Self {
        Self::new("invalid_response", message)
    }

    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub const fn details(&self) -> Option<&Value> {
        self.details.as_ref()
    }

    #[must_use]
    pub fn into_parts(self) -> (&'static str, String, Option<Value>) {
        (self.name, self.message, self.details)
    }
}

/// Returns the model-facing webpage scrape definition.
#[must_use]
pub fn definition() -> ToolDefinition {
    function_tool(
        TOOL_NAME,
        "Fetch a public webpage or PDF and return its main content as Markdown. Use this after search when you need to read the full contents of a specific result. Accepts one HTTP or HTTPS URL.",
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "format": "uri",
                    "description": "The public HTTP or HTTPS URL to scrape.",
                    "minLength": 1,
                    "maxLength": 4096
                }
            },
            "required": ["url"],
            "additionalProperties": false
        }),
    )
}

/// Parses provider-neutral LLM tool arguments into typed scrape arguments.
pub fn parse_arguments(
    arguments: &ToolArguments,
) -> Result<FirecrawlScrapeArguments, FirecrawlScrapeToolError> {
    parse(arguments).map_err(FirecrawlScrapeToolError::invalid_arguments)
}

/// Parses and executes an LLM scrape tool call.
pub async fn execute_scrape_tool(
    arguments: &ToolArguments,
    context: &FirecrawlScrapeToolContext,
) -> Result<FirecrawlScrapeToolOutput, FirecrawlScrapeToolError> {
    execute(parse_arguments(arguments)?, context).await
}

/// Executes a typed webpage scrape request.
pub async fn execute(
    arguments: FirecrawlScrapeArguments,
    context: &FirecrawlScrapeToolContext,
) -> Result<FirecrawlScrapeToolOutput, FirecrawlScrapeToolError> {
    let requested_url = validate_url(&arguments.url)?;
    let response = context
        .http
        .post(context.endpoint.clone())
        .bearer_auth(&context.api_key)
        .json(&FirecrawlScrapeRequest {
            url: requested_url.as_str(),
            formats: ["markdown"],
            only_main_content: true,
            only_clean_content: false,
            max_age: MAX_CACHE_AGE_MS,
            timeout: FIRECRAWL_TIMEOUT_MS,
            parsers: ["pdf"],
            remove_base64_images: true,
            block_ads: true,
            proxy: "auto",
            store_in_cache: true,
        })
        .send()
        .await
        .map_err(FirecrawlScrapeToolError::request)?;

    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(FirecrawlScrapeToolError::invalid_response(format!(
            "Scrape response exceeds the {MAX_RESPONSE_BYTES}-byte limit"
        )));
    }
    let body = response
        .bytes()
        .await
        .map_err(FirecrawlScrapeToolError::request)?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(FirecrawlScrapeToolError::invalid_response(format!(
            "Scrape response exceeds the {MAX_RESPONSE_BYTES}-byte limit"
        )));
    }
    let response: FirecrawlScrapeResponse = serde_json::from_slice(&body).map_err(|error| {
        FirecrawlScrapeToolError::invalid_response(format!(
            "Scrape service returned an invalid JSON response: {error}"
        ))
        .with_details(json!({ "http_status": status.as_u16() }))
    })?;

    if !status.is_success() || response.success == Some(false) {
        return Err(api_error(status, response));
    }
    let data = response.data.ok_or_else(|| {
        FirecrawlScrapeToolError::invalid_response("Scrape service returned no page data")
            .with_details(json!({ "http_status": status.as_u16() }))
    })?;
    build_output(requested_url.as_str(), data)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FirecrawlScrapeRequest<'a> {
    url: &'a str,
    formats: [&'static str; 1],
    only_main_content: bool,
    only_clean_content: bool,
    max_age: u64,
    timeout: u64,
    parsers: [&'static str; 1],
    remove_base64_images: bool,
    block_ads: bool,
    proxy: &'static str,
    store_in_cache: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FirecrawlScrapeResponse {
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    data: Option<FirecrawlScrapeData>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FirecrawlScrapeData {
    #[serde(default)]
    markdown: Option<String>,
    #[serde(default)]
    pages: Vec<FirecrawlScrapePage>,
    #[serde(default)]
    metadata: Map<String, Value>,
    #[serde(default)]
    warning: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FirecrawlScrapePage {
    #[serde(default)]
    page_number: Option<u64>,
    #[serde(default)]
    markdown: Option<String>,
}

fn validate_url(input: &str) -> Result<Url, FirecrawlScrapeToolError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(FirecrawlScrapeToolError::invalid_arguments(
            "url must not be empty",
        ));
    }
    if input.chars().count() > 4_096 {
        return Err(FirecrawlScrapeToolError::invalid_arguments(
            "url must not exceed 4096 characters",
        ));
    }
    let url = Url::parse(input).map_err(|error| {
        FirecrawlScrapeToolError::invalid_arguments(format!("url is invalid: {error}"))
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(FirecrawlScrapeToolError::invalid_arguments(
            "url must use http or https",
        ));
    }
    if url.host_str().is_none() {
        return Err(FirecrawlScrapeToolError::invalid_arguments(
            "url must include a host",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(FirecrawlScrapeToolError::invalid_arguments(
            "url must not include embedded credentials",
        ));
    }
    Ok(url)
}

fn api_error(status: StatusCode, response: FirecrawlScrapeResponse) -> FirecrawlScrapeToolError {
    let message = response.error.unwrap_or_else(|| {
        if status.is_success() {
            "The scrape service reported that the scrape failed".to_owned()
        } else {
            format!("Webpage scrape failed with HTTP status {status}")
        }
    });
    FirecrawlScrapeToolError::new("api_error", message).with_details(json!({
        "http_status": status.as_u16(),
        "code": response.code
    }))
}

fn build_output(
    requested_url: &str,
    data: FirecrawlScrapeData,
) -> Result<FirecrawlScrapeToolOutput, FirecrawlScrapeToolError> {
    let FirecrawlScrapeData {
        markdown,
        pages,
        metadata,
        warning,
    } = data;
    let markdown = markdown
        .filter(|markdown| !markdown.trim().is_empty())
        .or_else(|| markdown_from_pages(&pages))
        .ok_or_else(|| {
            FirecrawlScrapeToolError::invalid_response(
                "Scrape service returned no Markdown content",
            )
        })?;
    let original_markdown_bytes = markdown.len();
    let (markdown, truncated) = truncate_markdown(&markdown);
    let source_url = metadata_string(&metadata, &["sourceURL", "url"])
        .unwrap_or_else(|| requested_url.to_owned());
    let title = metadata_string(&metadata, &["title"]);
    let description = metadata_string(&metadata, &["description"]);
    let language = metadata_string(&metadata, &["language"]);
    let content_type = metadata_string(&metadata, &["contentType"]);
    let status_code = metadata.get("statusCode").cloned();

    let mut text = format!("Source: {source_url}\n");
    if let Some(title) = &title {
        text.push_str("Title: ");
        text.push_str(title);
        text.push('\n');
    }
    text.push('\n');
    text.push_str(&markdown);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    if let Some(warning) = warning
        .as_deref()
        .map(str::trim)
        .filter(|warning| !warning.is_empty())
    {
        text.push_str("\nWarning: ");
        text.push_str(warning);
        text.push('\n');
    }

    Ok(FirecrawlScrapeToolOutput::new(
        text,
        json!({
            "url": requested_url,
            "source_url": source_url,
            "title": title,
            "description": description,
            "language": language,
            "content_type": content_type,
            "status_code": status_code,
            "warning": warning,
            "truncated": truncated,
            "original_markdown_bytes": original_markdown_bytes,
            "returned_markdown_bytes": markdown.len()
        }),
    ))
}

fn markdown_from_pages(pages: &[FirecrawlScrapePage]) -> Option<String> {
    let mut output = String::new();
    for page in pages {
        let Some(markdown) = page
            .markdown
            .as_deref()
            .map(str::trim)
            .filter(|markdown| !markdown.is_empty())
        else {
            continue;
        };
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        if let Some(page_number) = page.page_number {
            output.push_str(&format!("## Page {page_number}\n\n"));
        }
        output.push_str(markdown);
    }
    (!output.is_empty()).then_some(output)
}

fn metadata_string(metadata: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        metadata
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn truncate_markdown(markdown: &str) -> (String, bool) {
    if markdown.len() <= MAX_MARKDOWN_BYTES {
        return (markdown.to_owned(), false);
    }
    let head_end = previous_char_boundary(markdown, MARKDOWN_HEAD_BYTES);
    let tail_start =
        next_char_boundary(markdown, markdown.len().saturating_sub(MARKDOWN_TAIL_BYTES));
    let mut output = String::with_capacity(
        head_end + TRUNCATION_MARKER.len() + markdown.len().saturating_sub(tail_start),
    );
    output.push_str(&markdown[..head_end]);
    output.push_str(TRUNCATION_MARKER);
    output.push_str(&markdown[tail_start..]);
    (output, true)
}

fn previous_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn next_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn package_env_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env")
}

fn function_tool(name: &str, description: &str, parameters: Value) -> ToolDefinition {
    let Value::Object(parameters) = parameters else {
        unreachable!("tool parameters are declared as an object")
    };
    ToolDefinition::Function(FunctionTool {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters,
        output_schema: None,
        strict: Some(false),
    })
}

fn parse<T: DeserializeOwned>(arguments: &ToolArguments) -> Result<T, serde_json::Error> {
    match arguments {
        ToolArguments::Object(arguments) => {
            serde_json::from_value(Value::Object(arguments.clone()))
        }
        ToolArguments::String(arguments) => serde_json::from_str(arguments),
    }
}
