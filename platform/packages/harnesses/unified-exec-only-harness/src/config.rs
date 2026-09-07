use execution_core::{ExecutionHostDescriptor, ExecutionHostId, ExecutionPath, RootId};
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
    pub environment: Environment,
    pub system_prompt_append: Option<String>,
}

/// Immutable environment descriptor supplied by the caller. Platform treats it
/// as harness configuration; resolution and sandbox lifecycle belong here.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Environment {
    Machine {
        machine_id: Uuid,
        workspace_root: String,
        path: String,
    },
    Sandbox {
        snapshot_id: Uuid,
        workspace_root: String,
        path: String,
    },
}
impl Environment {
    pub fn cwd(&self) -> Result<ExecutionPath, String> {
        let (root, path) = match self {
            Self::Machine {
                workspace_root,
                path,
                ..
            }
            | Self::Sandbox {
                workspace_root,
                path,
                ..
            } => (workspace_root, path),
        };
        if !platform_runtime_client::types::is_absolute_workspace_root(root) {
            if root.contains(['/', '\\', ':'])
                || root.chars().any(char::is_control)
                || root.trim() != root
            {
                return Err("Invalid absolute workspace path".into());
            }
            // Compatibility for historical immutable configs; new registration
            // schemas accept absolute paths only.
            RootId::new(root.clone()).map_err(|e| e.to_string())?;
        }
        ExecutionPath::new(RootId::new("workspace").unwrap(), path).map_err(|e| e.to_string())
    }
    pub fn target(&self, host_id: Uuid) -> Result<ExecutionTarget, String> {
        let (workspace_root, path) = match self {
            Self::Machine {
                workspace_root,
                path,
                ..
            }
            | Self::Sandbox {
                workspace_root,
                path,
                ..
            } => (workspace_root.clone(), path.clone()),
        };
        Ok(ExecutionTarget {
            host_id: ExecutionHostId::new(host_id.to_string()).map_err(|e| e.to_string())?,
            workspace_root,
            path,
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionTarget {
    pub host_id: ExecutionHostId,
    pub workspace_root: String,
    pub path: String,
}
impl ExecutionTarget {
    pub fn cwd(&self, host: &ExecutionHostDescriptor) -> Result<ExecutionPath, String> {
        let absolute =
            platform_runtime_client::types::is_absolute_workspace_root(&self.workspace_root);
        let mut roots = host.roots.iter().filter(|root| {
            if absolute {
                root.native_path == self.workspace_root
            } else {
                root.id.as_str() == self.workspace_root
            }
        });
        let root = roots
            .next()
            .ok_or("Workspace path is not a registered root on this host")?;
        if roots.next().is_some() {
            return Err("Workspace path matches multiple registered roots".into());
        }
        ExecutionPath::new(root.id.clone(), &self.path).map_err(|e| e.to_string())
    }
}

pub use crate::model::ReasoningLevel;

pub fn config_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["model","reasoning_level","environment"],"properties":{
        "model":{"type":"object","additionalProperties":false,"required":["provider","id"],"properties":{
            "provider":{"enum":["openai","chatgpt","fireworks"]},"id":{"type":"string","minLength":1},"name":{"type":["string","null"]}}},
        "reasoning_level":{"enum":["low","medium","high","xhigh","max"]},
        "account_id":{"type":["string","null"],"format":"uuid"},
        "environment": environment_schema(),
        "system_prompt_append":{"type":["string","null"],"maxLength":8192}
    }})
}

pub fn environment_schema() -> Value {
    let variant = |kind: &str, id: &str| {
        json!({"type":"object","additionalProperties":false,
        "required":["type",id,"workspace_root","path"],"properties":{
            "type":{"const":kind},id:{"type":"string","format":"uuid"},
            "workspace_root":{"type":"string","minLength":1,"maxLength":4096,"pattern":r"^(/|[A-Za-z]:[/\\]|\\\\)","description":"Absolute native workspace path returned by discovery; not a root ID."},
            "path":{"type":"string","minLength":1,"maxLength":4096}}})
    };
    json!({"oneOf":[variant("machine", "machine_id"), variant("sandbox", "snapshot_id")]})
}

impl Config {
    pub fn parse(value: &JsonObject) -> Result<Self, String> {
        let mut value = Value::Object(value.clone());
        // Read historical run snapshots without rewriting immutable run records.
        if value.get("environment").is_none() {
            if let Some(mut target) = value.as_object_mut().unwrap().remove("execution") {
                if let Some(fields) = target.as_object_mut() {
                    if let Some(id) = fields.remove("host_id") {
                        fields.insert("machine_id".into(), id);
                    }
                    fields.insert("type".into(), json!("machine"));
                }
                value["environment"] = target;
            }
        }
        let mut schema = config_schema();
        // Stored configurations predate native-path addressing. Do not rewrite
        // session history; translate those root IDs privately at execution time.
        for variant in schema["properties"]["environment"]["oneOf"]
            .as_array_mut()
            .unwrap()
        {
            variant["properties"]["workspace_root"]
                .as_object_mut()
                .unwrap()
                .remove("pattern");
        }
        if !jsonschema::validator_for(&schema)
            .map_err(|e| e.to_string())?
            .is_valid(&value)
        {
            return Err("Invalid unified-exec-only-harness configuration".into());
        }
        let config: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        config.environment.cwd()?;
        config.provider_options(Uuid::nil())?;
        Ok(config)
    }

    pub fn provider_options(&self, session: Uuid) -> Result<JsonObject, String> {
        crate::model::provider_options(&self.model, self.reasoning_level, session)
    }
}
