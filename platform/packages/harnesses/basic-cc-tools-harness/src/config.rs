use execution_core::{ExecutionHostId, ExecutionPath, RootId};
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
    pub execution: ExecutionTarget,
    pub system_prompt_append: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionTarget {
    pub host_id: ExecutionHostId,
    pub workspace_root: RootId,
    pub path: String,
}
impl ExecutionTarget {
    pub fn cwd(&self) -> Result<ExecutionPath, String> {
        ExecutionPath::new(self.workspace_root.clone(), &self.path).map_err(|e| e.to_string())
    }
}

pub use cc_harness_support::model::ReasoningLevel;

pub fn config_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["model","reasoning_level","execution"],"properties":{
        "model":{"type":"object","additionalProperties":false,"required":["provider","id"],"properties":{
            "provider":{"enum":["openai","chatgpt","fireworks"]},"id":{"type":"string","minLength":1},"name":{"type":["string","null"]}}},
        "reasoning_level":{"enum":["low","medium","high","xhigh","max"]},
        "account_id":{"type":["string","null"],"format":"uuid"},
        "execution":{"type":"object","additionalProperties":false,"required":["host_id","workspace_root","path"],"properties":{
            "host_id":{"type":"string","format":"uuid"},"workspace_root":{"type":"string","minLength":1},"path":{"type":"string","minLength":1,"maxLength":4096}}},
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
            return Err("Invalid basic-cc-tools-harness configuration".into());
        }
        let config: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        config.execution.cwd()?;
        config.provider_options(Uuid::nil())?;
        Ok(config)
    }

    pub fn provider_options(&self, session: Uuid) -> Result<JsonObject, String> {
        cc_harness_support::model::provider_options(&self.model, self.reasoning_level, session)
    }
}
