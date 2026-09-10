use crate::ReasoningLevel;
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
    #[serde(rename = "siteId")]
    pub site_id: Option<Uuid>,
    pub system_prompt_append: Option<String>,
}
pub fn config_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["model","reasoning_level"],"properties":{
        "model":{"type":"object","additionalProperties":false,"required":["provider","id"],"properties":{
            "provider":{"enum":["openai","chatgpt"]},"id":{"type":"string","minLength":1},"name":{"type":["string","null"]}}},
        "reasoning_level":{"enum":["low","medium","high","xhigh","max"]},
        "account_id":{"type":["string","null"],"format":"uuid"},
        "siteId":{"type":["string","null"],"format":"uuid","description":"Existing project site, or null to create a site on first authoring access. Multiple sessions can edit the same live site."},
        "system_prompt_append":{"type":["string","null"],"maxLength":8192}
    }})
}
impl Config {
    pub fn parse(value: &JsonObject) -> Result<Self, String> {
        let value = Value::Object(value.clone());
        if value["model"]["provider"] == "fireworks" {
            return Err("Sites exec requires raw custom-tool support; choose an OpenAI or ChatGPT model because the Fireworks adapter does not support it".into());
        }
        if !jsonschema::validator_for(&config_schema())
            .map_err(|e| e.to_string())?
            .is_valid(&value)
        {
            return Err("Invalid Sites harness configuration".into());
        }
        let config: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if config.site_id.is_some_and(|id| id.is_nil()) {
            return Err("siteId must be non-nil".into());
        }
        config.provider_options(Uuid::nil())?;
        Ok(config)
    }
    pub fn provider_options(&self, session: Uuid) -> Result<JsonObject, String> {
        crate::model::provider_options(&self.model, self.reasoning_level, session)
    }
}
