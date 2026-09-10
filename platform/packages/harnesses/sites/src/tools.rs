use futures_util::future::BoxFuture;
use llm_contracts::{
    CustomTool, CustomToolFormat, FunctionTool, GrammarSyntax, ToolArguments, ToolDefinition,
};
use platform_runtime_client::RunClient;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use tool_code_mode::live::{ContentItem, protocol::*};
use tool_code_mode::*;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Plan {
    Exec { input: ExecInput, admitted: bool },
    Wait { input: WaitInput },
}
pub(crate) struct Output {
    pub content: Vec<ContentItem>,
    pub error: bool,
}
impl Output {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![ContentItem::InputText {
                text: message.into(),
            }],
            error: true,
        }
    }
    pub fn report(report: Report) -> Self {
        Self {
            error: !report.success(),
            content: report.render(),
        }
    }
}
pub(crate) fn definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition::Custom(CustomTool {
            name: "exec".into(), description: EXEC_DESCRIPTION.into(),
            format: CustomToolFormat { syntax: GrammarSyntax::Lark, definition: EXEC_GRAMMAR.into() },
        }),
        ToolDefinition::Function(FunctionTool {
            name: "wait".into(), description: WAIT_DESCRIPTION.into(), strict: Some(false), output_schema: None,
            parameters: json!({"type":"object","properties":{
                "cell_id":{"type":"string","description":"Identifier of the running exec cell."},
                "yield_time_ms":{"type":"number","description":"Wait before yielding more output. Defaults to 10000 ms."},
                "max_tokens":{"type":"number","description":"Output token budget for this wait call. Defaults to 10000 tokens."},
                "terminate":{"type":"boolean","description":"True stops the running exec cell; false or omitted waits for output."}
            },"required":["cell_id"],"additionalProperties":false}).as_object().unwrap().clone(),
        }),
    ]
}
pub(crate) fn prepare(name: &str, arguments: &ToolArguments) -> std::result::Result<Plan, String> {
    match name {
        "exec" => match arguments {
            ToolArguments::String(source) => Ok(Plan::Exec {
                input: ExecInput::parse(source)?,
                admitted: false,
            }),
            _ => Err("exec expects raw JavaScript source text".into()),
        },
        "wait" => {
            let value = match arguments {
                ToolArguments::String(input) => {
                    serde_json::from_str(input).map_err(|_| "Invalid JSON wait arguments")?
                }
                ToolArguments::Object(input) => Value::Object(input.clone()),
            };
            Ok(Plan::Wait {
                input: serde_json::from_value(value)
                    .map_err(|e| format!("Invalid wait arguments: {e}"))?,
            })
        }
        _ => Err("Unknown tool; use exec or wait".into()),
    }
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

pub(crate) fn register(registry: &mut Registry) -> Result<()> {
    let mut read = tool_read::input_schema(&tool_read::ReadConfig::default());
    read["properties"]["file_path"] = json!({"type":"string","enum":["index.html","backend.js"]});
    let scalar = json!({"type":["null","string","number"]});
    for (name, description, input_schema, effect) in [
        (
            "metadata",
            "Get the bound site's metadata, without source files. No arguments.",
            object(json!({}), &[]),
            Effect::Read,
        ),
        (
            "read",
            "Read index.html or backend.js with optional one-based offset and line limit. Returns unnumbered source, revision and continuation information.",
            read,
            Effect::Read,
        ),
        (
            "apply_patch",
            "Apply a raw patch string, not JSON, to index.html and/or backend.js. Update edits matching context; Add replaces the entire file; Delete resets it to a blank HTML document or a backend returning 404. Hunks run in order, including same-file Delete/Add; only the validated final pair activates. No other paths or moves. Success returns {}.",
            json!({"type":"string","minLength":1,"maxLength":49152}),
            Effect::Mutation,
        ),
        (
            "invoke",
            "Call a live backend endpoint and return its response, errors and logs. Database and Platform effects are real.",
            object(
                json!({
                    "method":{"type":"string","enum":["GET","POST","PUT","PATCH","DELETE","HEAD","OPTIONS"]},
                    "path":{"type":"string","minLength":1,"maxLength":2048},
                    "query":{"type":"object","additionalProperties":{"type":"string"}},
                    "body":{}, "idempotency_key":{"type":"string","minLength":1,"maxLength":256}
                }),
                &["method", "path"],
            ),
            Effect::Mutation,
        ),
        (
            "browser",
            "Inspect the current site in a persistent browser: evaluate runs an async JavaScript function body in the site frame and returns JSON; screenshot returns a viewport image for image(result.image); reload loads the latest release. Backend effects are real. No page API or Node globals.",
            json!({"oneOf":[
                object(json!({"action":{"const":"evaluate"},"code":{"type":"string","minLength":1,"maxLength":49152}}), &["action","code"]),
                object(json!({"action":{"enum":["screenshot","reload"]}}), &["action"])
            ]}),
            Effect::Mutation,
        ),
        (
            "sql",
            "Execute one bounded SQLite statement on the bound site: reads, schema inspection, schema changes or data writes. Use parameters. Writes are real and have durable retry receipts.",
            object(
                json!({
                    "sql":{"type":"string","minLength":1,"maxLength":49152},
                    "params":{"type":"array","maxItems":256,"items":scalar},
                    "idempotency_key":{"type":"string","minLength":1,"maxLength":256}
                }),
                &["sql"],
            ),
            Effect::Mutation,
        ),
    ] {
        registry.register(Tool {
            name: name.into(),
            description: description.into(),
            input_schema,
            effect,
        })?;
    }
    Ok(())
}

pub(crate) struct SitesDispatcher {
    pub client: RunClient,
    pub site_id: Arc<Mutex<Option<Uuid>>>,
    pub browser: crate::browser::BrowserSession,
}
impl SitesDispatcher {
    async fn call(&self, method: &str, input: Value) -> std::result::Result<Value, ToolError> {
        let result = self.client.platform().call(method, input).await.map_err(|error| match error {
            platform_runtime_client::Error::Server(e) if (400..500).contains(&e.status) =>
                site_rejection(&e),
            _ => ToolError::uncertain("SITE_UNCERTAIN", "The site operation outcome is unknown. Do not repeat a mutation with a new key."),
        })?;
        if let Some(id) = result["siteId"]
            .as_str()
            .or_else(|| result["site_id"].as_str())
            .or_else(|| {
                if method == "sites.metadata" {
                    result["id"].as_str()
                } else {
                    None
                }
            })
            .and_then(|v| v.parse().ok())
        {
            *self.site_id.lock().unwrap() = Some(id);
        }
        Ok(result)
    }
}
// ServerError's Display intentionally omits details to keep generic logs safe.
// The authenticated runtime error envelope contains the public, model-facing
// explanation; expose it explicitly here, not in generic transport logging.
pub(crate) fn site_rejection(error: &platform_runtime_client::ServerError) -> ToolError {
    let message = error
        .error
        .as_ref()
        .map(|e| e.message.as_str())
        .filter(|message| !message.is_empty())
        .map(|message| format!("{error}: {message}"))
        .unwrap_or_else(|| error.to_string());
    ToolError::rejected(error.code().unwrap_or("SITE_REJECTED"), &message)
}
impl Dispatcher for SitesDispatcher {
    fn invoke<'a>(
        &'a self,
        call: &'a Call,
    ) -> BoxFuture<'a, std::result::Result<Value, ToolError>> {
        Box::pin(async move {
            match call.tool.as_str() {
                "metadata" => self.call("sites.metadata", json!({})).await,
                "read" => {
                    let input: tool_read::ReadInput = serde_json::from_value(call.input.clone())
                        .map_err(|_| {
                            ToolError::rejected("INVALID_INPUT", "Invalid read arguments")
                        })?;
                    let source = self.call("sites.read", json!({})).await?;
                    read_source(&source, &input)
                }
                "apply_patch" => {
                    let patch = call.input.as_str().ok_or_else(|| {
                        ToolError::rejected("INVALID_INPUT", "apply_patch takes raw patch text")
                    })?;
                    let result = self.call("sites.applyPatch", json!({
                        "input":{"patch":patch},"options":{"idempotencyKey":call.operation_key}
                    })).await?;
                    match result["status"].as_str() {
                        Some("succeeded") => Ok(json!({})),
                        Some("conflict") => Err(ToolError::rejected(
                            "PATCH_CONFLICT",
                            "Site changed before activation. Read current source and prepare a new patch.",
                        )),
                        _ => Err(ToolError::uncertain(
                            "PATCH_UNCERTAIN",
                            "Patch activation was not confirmed. Inspect current source before further edits.",
                        )),
                    }
                }
                "invoke" | "sql" => {
                    let mut input = call.input.clone();
                    let object = input.as_object_mut().ok_or_else(|| {
                        ToolError::rejected("INVALID_INPUT", "Expected arguments object")
                    })?;
                    let key = object
                        .remove("idempotency_key")
                        .unwrap_or_else(|| json!(call.operation_key));
                    let method = if call.tool == "invoke" {
                        "sites.invoke"
                    } else {
                        "sites.sql"
                    };
                    let mut result = self
                        .call(
                            method,
                            json!({"input":input,"options":{"idempotencyKey":key}}),
                        )
                        .await?;
                    if call.tool == "invoke" {
                        Ok(invocation_result(result))
                    } else {
                        if let Some(object) = result.as_object_mut() {
                            object.remove("site_id");
                        }
                        Ok(result)
                    }
                }
                "browser" => {
                    // Even a cached page must reauthorize its run before each
                    // action; background backend calls authorize independently.
                    self.call("sites.metadata", json!({})).await?;
                    self.browser.invoke(&self.client, &call.input).await
                }
                _ => Err(ToolError::rejected("UNKNOWN_TOOL", "Unknown Sites tool")),
            }
        })
    }
}

pub(crate) fn read_source(
    source: &Value,
    input: &tool_read::ReadInput,
) -> std::result::Result<Value, ToolError> {
    let field = match input.file_path.as_str() {
        "index.html" => "frontend",
        "backend.js" => "backend",
        _ => {
            return Err(ToolError::rejected(
                "INVALID_PATH",
                "Only index.html and backend.js can be read",
            ));
        }
    };
    let text = source["files"][field]
        .as_str()
        .ok_or_else(|| ToolError::rejected("INVALID_SOURCE", "Site source is missing"))?;
    let window = tool_read::read_text(
        text,
        input.offset,
        input.limit,
        &tool_read::ReadConfig::default(),
    )
    .map_err(|e| ToolError::rejected("READ_FAILED", &e.to_string()))?;
    let mut result = serde_json::to_value(window).expect("read window serializes");
    result["file_path"] = json!(input.file_path);
    result["revision"] = json!(format!("{:x}", Sha256::digest(text.as_bytes())));
    Ok(result)
}

pub(crate) fn invocation_result(value: Value) -> Value {
    let error = value["error_code"].as_str().map(|code| json!({"code":code,"message":format!("Backend invocation failed: {code}. See logs for diagnostics.")}));
    json!({
        "invocation_id":value["id"],"status":value["status"],"response":value["response"],
        "error":error,"logs":value["logs"],
        "response_truncated":value["responseTruncated"].as_bool().unwrap_or(false),
        "logs_truncated":value["logsTruncated"].as_bool().unwrap_or(false)
    })
}
