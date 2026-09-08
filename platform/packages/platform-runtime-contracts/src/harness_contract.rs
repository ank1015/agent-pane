//! Optional harness I/O declarations. Provisioning remains harness-owned.
use crate::{JsonObject, OutputKind, RUN_OUTPUT_MAX_COUNT, valid_output_name};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum EnvironmentCardinality {
    Single,
    Multiple,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EnvironmentInput {
    /// RFC 6901 pointer into resolved config. Single selects a UUID; multiple
    /// selects an array of UUIDs. No wildcards or hidden provisioning semantics.
    pub config_pointer: String,
    pub cardinality: EnvironmentCardinality,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OutputDeclaration {
    pub kind: OutputKind,
    pub description: Option<String>,
    /// Additional locally resolvable JSON Schema applied to the output's value.
    pub value_schema: Option<JsonObject>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HarnessContract {
    #[serde(default)]
    pub environment_inputs: Vec<EnvironmentInput>,
    /// Empty means no declared outputs. Outputs are optional, even on success.
    #[serde(default)]
    pub outputs: BTreeMap<String, OutputDeclaration>,
}

impl HarnessContract {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.environment_inputs.len() > 64
            || self.outputs.len() > RUN_OUTPUT_MAX_COUNT
            || serde_json::to_vec(self)
                .map_err(|_| "Invalid contract JSON.")?
                .len()
                > 64 * 1024
        {
            return Err("Harness contract is limited to 64 inputs, 64 outputs and 64 KiB.");
        }
        let mut pointers = HashSet::new();
        for input in &self.environment_inputs {
            let p = &input.config_pointer;
            if !p.starts_with('/')
                || p.len() > 1024
                || p.chars().any(char::is_control)
                || !pointers.insert(p)
            {
                return Err("Environment inputs require unique RFC 6901 config pointers.");
            }
            let mut chars = p.chars();
            while let Some(c) = chars.next() {
                if c == '~' && !matches!(chars.next(), Some('0' | '1')) {
                    return Err("Invalid escape in environment config pointer.");
                }
            }
        }
        for (name, output) in &self.outputs {
            if !valid_output_name(name)
                || output.description.as_ref().is_some_and(|v| v.len() > 4096)
            {
                return Err("Invalid output name or description exceeding 4 KiB.");
            }
        }
        Ok(())
    }

    /// Discover declared references without allocating or rewriting environments.
    pub fn environment_ids(&self, config: &Value) -> Result<Vec<Uuid>, &'static str> {
        self.validate()?;
        let mut ids = Vec::new();
        for input in &self.environment_inputs {
            let Some(value) = config.pointer(&input.config_pointer) else {
                if input.required {
                    return Err("Required environment input is missing.");
                }
                continue;
            };
            let values = match input.cardinality {
                EnvironmentCardinality::Single => std::slice::from_ref(value),
                EnvironmentCardinality::Multiple => value
                    .as_array()
                    .filter(|v| v.len() <= 64)
                    .ok_or("Multiple environment input must be an array of at most 64 UUIDs.")?,
            };
            for value in values {
                let id = value
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .filter(|id| !id.is_nil())
                    .ok_or("Environment references must be non-nil UUID strings.")?;
                ids.push(id);
            }
        }
        ids.sort_unstable();
        ids.dedup();
        if ids.len() > 64 {
            return Err("Configuration references more than 64 environments.");
        }
        Ok(ids)
    }
}
