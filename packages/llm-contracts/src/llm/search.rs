use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::validation::{Validate, ValidationError, ValidationIssue, issue, require_non_empty};

/// Request body accepted by the Codex `alpha/search` endpoint.
///
/// This intentionally mirrors the upstream wire format. Provider routing and
/// account selection belong to the gateway envelope, not this payload.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SearchRequest {
    pub id: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<SearchReasoning>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<SearchInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<SearchCommands>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<SearchSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
}

/// Search context supplied either as plain text or native Responses API items.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SearchInput {
    Text(String),
    Items(Vec<JsonValue>),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SearchReasoning {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<SearchReasoningSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<SearchReasoningContext>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchReasoningSummary {
    Auto,
    Concise,
    Detailed,
    None,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchReasoningContext {
    Auto,
    CurrentTurn,
    AllTurns,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SearchCommands {
    /// Query the internet search engine for a given list of queries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_query: Option<Vec<SearchQuery>>,
    /// Query the image search engine for a given list of queries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_query: Option<Vec<SearchQuery>>,
    /// Open pages by reference id or URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<Vec<OpenOperation>>,
    /// Open links from previously opened pages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click: Option<Vec<ClickOperation>>,
    /// Find text patterns in pages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub find: Option<Vec<FindOperation>>,
    /// Take screenshots of PDF pages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<Vec<ScreenshotOperation>>,
    /// Look up prices for the given stock symbols.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finance: Option<Vec<FinanceOperation>>,
    /// Look up weather forecasts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weather: Option<Vec<WeatherOperation>>,
    /// Look up sports schedules and standings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sports: Option<Vec<SportsOperation>>,
    /// Get time for the given UTC offsets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<Vec<TimeOperation>>,
    /// Set the length of the response to be returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_length: Option<SearchResponseLength>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SearchQuery {
    /// Search query.
    pub q: String,
    /// Whether to filter by recency, as a number of recent days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recency: Option<u64>,
    /// Whether to filter by a specific list of domains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domains: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct OpenOperation {
    /// Reference id or URL to open.
    pub ref_id: String,
    /// Line number to position the page at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineno: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ClickOperation {
    /// Reference id containing the numbered link.
    pub ref_id: String,
    /// Numbered link id to open.
    pub id: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct FindOperation {
    /// Reference id or URL to search within.
    pub ref_id: String,
    /// Text pattern to find.
    pub pattern: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ScreenshotOperation {
    /// Reference id or URL to screenshot.
    pub ref_id: String,
    /// Zero-indexed PDF page number.
    pub pageno: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct FinanceOperation {
    /// Ticker symbol to look up.
    pub ticker: String,
    /// Asset type to look up.
    pub r#type: FinanceAssetType,
    /// ISO 3166-1 alpha-3 country code, "OTC", or "" for cryptocurrency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FinanceAssetType {
    Equity,
    Fund,
    Crypto,
    Index,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct WeatherOperation {
    /// Location in "Country, Area, City" format.
    pub location: String,
    /// Start date in YYYY-MM-DD format. Defaults to today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    /// Number of days to return. Defaults to 7.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SportsOperation {
    /// Tool name for sports requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<SportsToolName>,
    /// Sports function to call.
    pub r#fn: SportsFunction,
    /// League to look up.
    pub league: SportsLeague,
    /// Team to look up, using the common 3 or 4 letter alias used in broadcasts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// Opponent to use with `team` when narrowing the lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opponent: Option<String>,
    /// Start date in YYYY-MM-DD format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_from: Option<String>,
    /// End date in YYYY-MM-DD format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_to: Option<String>,
    /// Number of games to return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_games: Option<u64>,
    /// Locale for the lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SportsToolName {
    Sports,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SportsFunction {
    Schedule,
    Standings,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SportsLeague {
    Nba,
    Wnba,
    Nfl,
    Nhl,
    Mlb,
    Epl,
    Ncaamb,
    Ncaawb,
    Ipl,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct TimeOperation {
    /// UTC offset formatted like "+03:00".
    pub utc_offset: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchResponseLength {
    Short,
    Medium,
    Long,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SearchSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_location: Option<ApproximateLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_context_size: Option<SearchContextSize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filters: Option<SearchFilters>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_settings: Option<SearchImageSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_callers: Option<Vec<AllowedCaller>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_web_access: Option<ExternalWebAccess>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ApproximateLocation {
    pub r#type: LocationType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LocationType {
    Approximate,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchContextSize {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct SearchFilters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_domains: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct SearchImageSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<bool>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowedCaller {
    Direct,
    Shell,
    CodeInterpreter,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ExternalWebAccess {
    Boolean(bool),
    Mode(ExternalWebAccessMode),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalWebAccessMode {
    Cached,
    Indexed,
    Live,
}

/// HTTP metadata forwarded by Codex alongside a search request.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchRequestOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub originator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_turn_metadata: Option<String>,
}

/// Non-streaming response returned by `alpha/search`.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct SearchResponse {
    pub encrypted_output: Option<String>,
    pub output: String,
    #[serde(default)]
    pub results: Option<Vec<JsonValue>>,
}

impl Validate for SearchRequest {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        require_non_empty(&mut issues, "search.id", &self.id);
        require_non_empty(&mut issues, "search.model", &self.model);
        if self.max_output_tokens == Some(0) {
            issue(
                &mut issues,
                "search.max_output_tokens",
                "must be greater than zero",
            );
        }
        if let Some(SearchInput::Text(input)) = &self.input {
            require_non_empty(&mut issues, "search.input", input);
        }
        finish(issues)
    }
}

fn finish(issues: Vec<ValidationIssue>) -> Result<(), ValidationError> {
    ValidationError::from_issues(issues).map_or(Ok(()), Err)
}
