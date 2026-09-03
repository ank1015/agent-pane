use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::{
    Validate, ValidationError,
    validation::{finish, issue, require_non_empty},
};

/// Request accepted by the Codex provider-backed search API.
///
/// This mirrors the upstream wire format. Provider routing and account
/// selection belong outside this payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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

/// Search context supplied as plain text or native Responses API items.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SearchInput {
    Text(String),
    Items(Vec<JsonValue>),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SearchReasoning {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<SearchReasoningSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<SearchReasoningContext>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchReasoningSummary {
    Auto,
    Concise,
    Detailed,
    None,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchReasoningContext {
    Auto,
    CurrentTurn,
    AllTurns,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct SearchCommands {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_query: Option<Vec<SearchQuery>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_query: Option<Vec<SearchQuery>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<Vec<OpenOperation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click: Option<Vec<ClickOperation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub find: Option<Vec<FindOperation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<Vec<ScreenshotOperation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finance: Option<Vec<FinanceOperation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weather: Option<Vec<WeatherOperation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sports: Option<Vec<SportsOperation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<Vec<TimeOperation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_length: Option<SearchResponseLength>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recency: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domains: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OpenOperation {
    pub ref_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineno: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClickOperation {
    pub ref_id: String,
    pub id: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FindOperation {
    pub ref_id: String,
    pub pattern: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScreenshotOperation {
    pub ref_id: String,
    pub pageno: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FinanceOperation {
    pub ticker: String,
    pub r#type: FinanceAssetType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FinanceAssetType {
    Equity,
    Fund,
    Crypto,
    Index,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WeatherOperation {
    pub location: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SportsOperation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<SportsToolName>,
    pub r#fn: SportsFunction,
    pub league: SportsLeague,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opponent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_games: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SportsToolName {
    Sports,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SportsFunction {
    Schedule,
    Standings,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TimeOperation {
    pub utc_offset: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchResponseLength {
    Short,
    Medium,
    Long,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LocationType {
    Approximate,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchContextSize {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SearchFilters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_domains: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SearchImageSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<bool>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowedCaller {
    Direct,
    Shell,
    CodeInterpreter,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ExternalWebAccess {
    Boolean(bool),
    Mode(ExternalWebAccessMode),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalWebAccessMode {
    Cached,
    Indexed,
    Live,
}

/// HTTP metadata forwarded by Codex alongside a search request.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchRequestOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub originator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_turn_metadata: Option<String>,
}

/// Non-streaming response returned by the provider-backed search API.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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
