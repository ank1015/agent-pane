//! Opinionated model-facing web search backed by Firecrawl.
//!
//! The model controls only the search query. This crate fixes result count,
//! source, safety, timeout, URL validation, and highlighting so harnesses do
//! not need to expose Firecrawl's complete API surface to the model.

use std::{env, time::Duration};

use llm_contracts::{ContentPart, FunctionTool, TextContent, ToolArguments, ToolDefinition};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use url::Url;

/// Name used in the model-facing tool definition.
pub const TOOL_NAME: &str = "search";
/// Environment variable containing the Firecrawl bearer token.
pub const API_KEY_ENV: &str = "FIRECRAWL_API_KEY";
/// Firecrawl v2 search endpoint.
pub const DEFAULT_SEARCH_ENDPOINT: &str = "https://api.firecrawl.dev/v2/search";
/// Fixed number of web results requested from Firecrawl.
pub const SEARCH_RESULT_LIMIT: u8 = 10;
/// Timeout sent to Firecrawl in the search request.
pub const FIRECRAWL_TIMEOUT_MS: u64 = 30_000;

const HTTP_TIMEOUT: Duration = Duration::from_secs(35);
const MAX_RESPONSE_BYTES: usize = 2 * 1_024 * 1_024;

/// Typed arguments accepted by the Firecrawl search tool.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FirecrawlSearchArguments {
    pub query: String,
}

/// HTTP and credential inputs used to execute Firecrawl searches.
#[derive(Clone)]
pub struct FirecrawlSearchToolContext {
    http: Client,
    api_key: String,
    endpoint: Url,
}

impl std::fmt::Debug for FirecrawlSearchToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirecrawlSearchToolContext")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl FirecrawlSearchToolContext {
    /// Creates a context using the public Firecrawl v2 endpoint.
    pub fn new(api_key: impl Into<String>) -> Result<Self, FirecrawlSearchToolError> {
        let http = Client::builder()
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(FirecrawlSearchToolError::configuration)?;
        Self::with_client(api_key, DEFAULT_SEARCH_ENDPOINT, http)
    }

    /// Reads the process environment only. The worker owns any dotenv loading.
    pub fn from_env() -> Result<Self, FirecrawlSearchToolError> {
        let key = env::var(API_KEY_ENV)
            .map_err(|_| FirecrawlSearchToolError::configuration("FIRECRAWL_API_KEY is missing"))?;
        Self::new(key)
    }

    /// Creates a context with an injected endpoint and HTTP client. This is
    /// useful for tests and Firecrawl-compatible deployments.
    pub fn with_client(
        api_key: impl Into<String>,
        endpoint: impl AsRef<str>,
        http: Client,
    ) -> Result<Self, FirecrawlSearchToolError> {
        let api_key = api_key.into();
        if api_key.is_empty() || !api_key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(FirecrawlSearchToolError::configuration(format!(
                "{API_KEY_ENV} must contain printable ASCII without spaces"
            )));
        }
        let endpoint = Url::parse(endpoint.as_ref()).map_err(|error| {
            FirecrawlSearchToolError::configuration(format!("invalid search endpoint: {error}"))
        })?;
        if !(endpoint.scheme() == "https"
            || (endpoint.scheme() == "http"
                && matches!(
                    endpoint.host_str(),
                    Some("localhost" | "127.0.0.1" | "[::1]")
                )))
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(FirecrawlSearchToolError::configuration(
                "search endpoint must use HTTPS (or loopback HTTP) without credentials, query, or fragment",
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

/// Successful model-facing output from a Firecrawl search.
#[derive(Clone, Debug, PartialEq)]
pub struct FirecrawlSearchToolOutput {
    pub content: Vec<ContentPart>,
    pub details: Option<Value>,
}

impl FirecrawlSearchToolOutput {
    fn new(text: String, details: Value) -> Self {
        let text = bounded_text(&text, MAX_OUTPUT_TEXT_BYTES);
        let details = bounded_details(details);
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
pub struct FirecrawlSearchToolError {
    name: &'static str,
    message: String,
    details: Option<Value>,
}

impl FirecrawlSearchToolError {
    fn new(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            message: bounded_text(&message.into(), 4096),
            details: None,
        }
    }

    fn with_details(mut self, details: Value) -> Self {
        self.details = Some(bounded_details(details));
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
        Self::new(
            name,
            format!("Web search request failed: {}", error.without_url()),
        )
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

/// Returns the model-facing Firecrawl search definition.
#[must_use]
pub fn definition() -> ToolDefinition {
    function_tool(
        TOOL_NAME,
        "Search the public web. Use this to find current information, relevant webpages, documentation, articles, and sources. The query supports search operators such as site:example.com, filetype:pdf, quoted phrases, and -excluded terms.",
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "A concise web search query.",
                    "minLength": 1,
                    "maxLength": 500
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    )
}

/// Parses provider-neutral LLM tool arguments into typed search arguments.
pub fn parse_arguments(
    arguments: &ToolArguments,
) -> Result<FirecrawlSearchArguments, FirecrawlSearchToolError> {
    parse(arguments).map_err(FirecrawlSearchToolError::invalid_arguments)
}

/// Parses and executes an LLM search tool call.
pub async fn execute_search_tool(
    arguments: &ToolArguments,
    context: &FirecrawlSearchToolContext,
) -> Result<FirecrawlSearchToolOutput, FirecrawlSearchToolError> {
    execute(parse_arguments(arguments)?, context).await
}

/// Executes a typed Firecrawl search request.
pub async fn execute(
    arguments: FirecrawlSearchArguments,
    context: &FirecrawlSearchToolContext,
) -> Result<FirecrawlSearchToolOutput, FirecrawlSearchToolError> {
    let query = validate_query(&arguments.query)?;
    let mut response = context
        .http
        .post(context.endpoint.clone())
        .timeout(HTTP_TIMEOUT)
        .bearer_auth(&context.api_key)
        .json(&FirecrawlSearchRequest {
            query: &query,
            limit: SEARCH_RESULT_LIMIT,
            sources: ["web"],
            safe: true,
            timeout: FIRECRAWL_TIMEOUT_MS,
            ignore_invalid_urls: true,
            highlights: true,
        })
        .send()
        .await
        .map_err(FirecrawlSearchToolError::request)?;

    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(FirecrawlSearchToolError::invalid_response(format!(
            "Search response exceeds the {MAX_RESPONSE_BYTES}-byte limit"
        )));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(FirecrawlSearchToolError::request)?
    {
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
            return Err(FirecrawlSearchToolError::invalid_response(format!(
                "Search response exceeds the {MAX_RESPONSE_BYTES}-byte limit"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    let response: FirecrawlSearchResponse = serde_json::from_slice(&body).map_err(|error| {
        FirecrawlSearchToolError::invalid_response(format!(
            "Search service returned an invalid JSON response: {error}"
        ))
        .with_details(json!({ "http_status": status.as_u16() }))
    })?;

    if !status.is_success() || !response.success {
        return Err(api_error(status, response));
    }

    let results = response.data.map_or_else(Vec::new, |data| data.web);
    let text = format_results(&query, &results, response.warning.as_deref());
    let details = json!({
        "query": query,
        "results": results,
        "warning": response.warning,
        "search_id": response.id,
        "credits_used": response.credits_used
    });
    Ok(FirecrawlSearchToolOutput::new(text, details))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FirecrawlSearchRequest<'a> {
    query: &'a str,
    limit: u8,
    sources: [&'static str; 1],
    safe: bool,
    timeout: u64,
    #[serde(rename = "ignoreInvalidURLs")]
    ignore_invalid_urls: bool,
    highlights: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FirecrawlSearchResponse {
    success: bool,
    #[serde(default)]
    data: Option<FirecrawlSearchData>,
    #[serde(default)]
    warning: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    credits_used: Option<u64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FirecrawlSearchData {
    #[serde(default)]
    web: Vec<FirecrawlWebResult>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FirecrawlWebResult {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    url: String,
    #[serde(default)]
    category: Option<String>,
}

fn validate_query(query: &str) -> Result<String, FirecrawlSearchToolError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(FirecrawlSearchToolError::invalid_arguments(
            "query must not be empty",
        ));
    }
    if query.chars().count() > 500 {
        return Err(FirecrawlSearchToolError::invalid_arguments(
            "query must not exceed 500 characters",
        ));
    }
    Ok(query.to_owned())
}

fn api_error(status: StatusCode, response: FirecrawlSearchResponse) -> FirecrawlSearchToolError {
    let message = response.error.unwrap_or_else(|| {
        if status.is_success() {
            "The search service reported that the search failed".to_owned()
        } else {
            format!("Web search failed with HTTP status {status}")
        }
    });
    FirecrawlSearchToolError::new("api_error", message).with_details(json!({
        "http_status": status.as_u16(),
        "code": response.code
    }))
}

fn format_results(query: &str, results: &[FirecrawlWebResult], warning: Option<&str>) -> String {
    let mut output = format!("Search results for: {query}\n");
    if results.is_empty() {
        output.push_str("\nNo results found.\n");
    } else {
        for (index, result) in results.iter().enumerate() {
            let title = result
                .title
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .unwrap_or("Untitled result");
            output.push_str(&format!(
                "\n{}. {title}\n   URL: {}\n",
                index + 1,
                result.url
            ));
            if let Some(description) = result
                .description
                .as_deref()
                .map(str::trim)
                .filter(|description| !description.is_empty())
            {
                output.push_str("   ");
                output.push_str(description);
                output.push('\n');
            }
        }
    }
    if let Some(warning) = warning.map(str::trim).filter(|warning| !warning.is_empty()) {
        output.push_str("\nWarning: ");
        output.push_str(warning);
        output.push('\n');
    }
    output
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

/// Upper bound on rendered text; JSON encoding may require additional bytes.
pub const MAX_OUTPUT_TEXT_BYTES: usize = 64 * 1024;
/// Upper bound on serialized structured metadata (not just its string values).
pub const MAX_OUTPUT_DETAILS_BYTES: usize = 64 * 1024;

fn bounded_text(value: &str, limit: usize) -> String {
    const MARKER: &str = "\n[... tool output truncated ...]";
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut end = limit.saturating_sub(MARKER.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{MARKER}", &value[..end])
}

fn bounded_details(value: Value) -> Value {
    if serde_json::to_vec(&value).is_ok_and(|bytes| bytes.len() <= MAX_OUTPUT_DETAILS_BYTES) {
        value
    } else {
        json!({"truncated": true, "reason": "Structured details exceeded the tool output limit; use the bounded text result."})
    }
}
