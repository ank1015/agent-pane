//! Codex-compatible `web.run` metadata and default search configuration.

use codex_code_mode_runtime::{CodeModeToolKind, ToolDefinition, ToolName};
use llm_contracts::{AllowedCaller, ExternalWebAccess, SearchCommands, SearchSettings};
use schemars::generate::SchemaSettings;
use serde_json::{Map, Value};

pub const WEB_NAMESPACE: &str = "web";
pub const RUN_TOOL_NAME: &str = "run";
pub const CODE_MODE_TOOL_NAME: &str = "web__run";
pub const WEB_RUN_DESCRIPTION: &str = include_str!("web_run_description.md");
/// Codex's default 10,000-byte tool-output policy converted to its approximate
/// four-bytes-per-token budget.
pub const DEFAULT_SEARCH_MAX_OUTPUT_TOKENS: u64 = 2_500;

/// Returns the namespaced function installed into the code-mode runtime.
#[must_use]
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: CODE_MODE_TOOL_NAME.to_owned(),
        tool_name: ToolName {
            name: RUN_TOOL_NAME.to_owned(),
            namespace: Some(WEB_NAMESPACE.to_owned()),
        },
        description: WEB_RUN_DESCRIPTION.to_owned(),
        kind: CodeModeToolKind::Function,
        input_schema: Some(commands_schema()),
        output_schema: None,
    }
}

/// Matches Codex's default cached web-search mode.
#[must_use]
pub fn default_search_settings() -> SearchSettings {
    SearchSettings {
        allowed_callers: Some(vec![AllowedCaller::Direct]),
        external_web_access: Some(ExternalWebAccess::Boolean(false)),
        ..Default::default()
    }
}

#[must_use]
pub fn commands_schema() -> Value {
    let schema = SchemaSettings::draft2019_09()
        .with(|settings| {
            settings.inline_subschemas = true;
        })
        .into_generator()
        .into_root_schema_for::<SearchCommands>();
    let mut schema =
        serde_json::to_value(schema).expect("SearchCommands JSON Schema must serialize");
    remove_option_null_types(&mut schema);
    let Value::Object(mut schema) = schema else {
        unreachable!("SearchCommands schema is an object")
    };
    let mut parameters = Map::new();
    for key in [
        "properties",
        "required",
        "type",
        "additionalProperties",
        "$defs",
        "definitions",
    ] {
        if let Some(value) = schema.remove(key) {
            parameters.insert(key.to_owned(), value);
        }
    }
    Value::Object(parameters)
}

/// Schemars 0.8 exposes `option_add_null_type`; Schemars 1.x does not. Codex
/// disables that setting, so apply the equivalent transform after generation.
fn remove_option_null_types(value: &mut Value) {
    match value {
        Value::Array(values) => {
            values.retain(|value| !value.is_null() && value.as_str() != Some("null"));
            for value in values {
                remove_option_null_types(value);
            }
        }
        Value::Object(object) => {
            for value in object.values_mut() {
                remove_option_null_types(value);
            }
            let single_type = match object.get_mut("type") {
                Some(Value::Array(types)) if types.len() == 1 => Some(types.remove(0)),
                _ => None,
            };
            if let Some(single_type) = single_type {
                object.insert("type".to_owned(), single_type);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use llm_contracts::{ExternalWebAccess, SearchCommands};

    use super::{
        CODE_MODE_TOOL_NAME, RUN_TOOL_NAME, WEB_NAMESPACE, WEB_RUN_DESCRIPTION, commands_schema,
        default_search_settings, definition,
    };

    #[test]
    fn definition_is_the_codex_web_namespace_tool() {
        let definition = definition();
        assert_eq!(definition.name, CODE_MODE_TOOL_NAME);
        assert_eq!(
            definition.tool_name.namespace.as_deref(),
            Some(WEB_NAMESPACE)
        );
        assert_eq!(definition.tool_name.name, RUN_TOOL_NAME);
        let schema = definition.input_schema.expect("input schema");
        for command in [
            "search_query",
            "image_query",
            "open",
            "click",
            "find",
            "screenshot",
            "finance",
            "weather",
            "sports",
            "time",
            "response_length",
        ] {
            assert!(
                schema["properties"].get(command).is_some(),
                "missing {command}"
            );
        }
        serde_json::from_value::<SearchCommands>(serde_json::json!({
            "search_query": [{"q": "Codex"}]
        }))
        .expect("schema's request shape is deserializable");
        assert!(
            !schema.to_string().contains("\"null\""),
            "Codex disables null types for optional command fields: {schema}"
        );
        assert!(WEB_RUN_DESCRIPTION.contains("## Decision boundary"));
        assert!(WEB_RUN_DESCRIPTION.contains("## Citations"));
        assert!(WEB_RUN_DESCRIPTION.contains("## Word limits"));
    }

    #[test]
    fn defaults_to_codex_cached_direct_search() {
        let settings = default_search_settings();
        assert_eq!(
            settings.external_web_access,
            Some(ExternalWebAccess::Boolean(false))
        );
        assert_eq!(
            serde_json::to_value(settings).expect("settings"),
            serde_json::json!({
                "allowed_callers": ["direct"],
                "external_web_access": false
            })
        );
    }

    #[test]
    fn schema_has_descriptions_from_the_codex_contract() {
        let schema = commands_schema();
        assert_eq!(
            schema["properties"]["search_query"]["description"],
            "Query the internet search engine for a given list of queries."
        );
    }
}
