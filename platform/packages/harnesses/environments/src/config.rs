use llm_contracts::{JsonObject, ModelRef};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub model: ModelRef,
    pub reasoning_level: ReasoningLevel,
    pub account_id: Option<Uuid>,
    #[serde(default = "enabled")]
    pub web_search_enabled: bool,
    pub system_prompt_append: Option<String>,
}

fn enabled() -> bool {
    true
}

pub use cc_harness_support::model::ReasoningLevel;

pub fn config_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["model","reasoning_level"],"properties":{
        "model":{"type":"object","additionalProperties":false,"required":["provider","id"],"properties":{
            "provider":{"enum":["openai","chatgpt","fireworks"]},"id":{"type":"string","minLength":1},"name":{"type":["string","null"]}}},
        "reasoning_level":{"enum":["low","medium","high","xhigh","max"]},
        "account_id":{"type":["string","null"],"format":"uuid"},
        "web_search_enabled":{"type":"boolean","default":true},
        "system_prompt_append":{"type":["string","null"],"maxLength":8192}
    }})
}

impl Config {
    pub fn parse(value: &JsonObject) -> Result<Self, String> {
        let value = Value::Object(value.clone());
        if !jsonschema::validator_for(&config_schema())
            .map_err(|e| e.to_string())?
            .is_valid(&value)
        {
            return Err("Invalid environments harness configuration".into());
        }
        let config: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        config.provider_options(Uuid::nil())?;
        Ok(config)
    }

    pub fn provider_options(&self, session: Uuid) -> Result<JsonObject, String> {
        cc_harness_support::model::provider_options(&self.model, self.reasoning_level, session)
    }
}
