use crate::{SitesHarness, browser::Browser};
use chrono::{DateTime, Utc};
use futures_util::future::BoxFuture;
use harness_runtime::Signals;
use llm_contracts::{FunctionTool, ToolArguments, ToolDefinition};
use platform_agent_code_mode::{PlatformDispatcher, code_mode::*};
use platform_runtime_client::{Error as RuntimeError, Result as RuntimeResult, RunClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Clone)]
pub struct WebTools {
    pub search: tool_firecrawl_search::FirecrawlSearchToolContext,
    pub scrape: tool_firecrawl_scrape::FirecrawlScrapeToolContext,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Plan {
    Code {
        input: Input,
    },
    Inspect {
        id: Uuid,
        after: Option<String>,
        limit: u32,
    },
    Reconcile {
        id: Uuid,
        sequence: u32,
    },
    Wait {
        key: Uuid,
        run_ids: Vec<Uuid>,
        wake_at: DateTime<Utc>,
    },
}
pub(crate) struct Output {
    pub text: String,
    pub error: bool,
    pub site_id: Option<Uuid>,
}
impl Output {
    pub fn success(value: Value) -> Self {
        let mut text = value.to_string();
        if text.len() > 192 * 1024 {
            // Keep receipt identities and status even when source or results dominate
            // the trace page. A bounded report must still permit exact recovery.
            let summarize = |call: &Value| {
                json!({
                    "cellId":call["cell_id"],"sequence":call["sequence"],
                    "tool":call["tool"],"operationKey":call["operation_key"],
                    "status":call["status"],"error":call["error"],
                    "payloadOmitted":true
                })
            };
            let calls: Vec<Value> = value["calls"]["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|entry| summarize(&entry["value"]))
                .collect();
            text=json!({
                "truncated":true,
                "reason":"Tool report exceeds 192 KiB; inspect one call per page or use a bounded operation getter. Receipt identities and status are preserved below.",
                "cellId":value.get("cellId").or_else(||value.get("cell_id")).or_else(||value.get("cell").and_then(|v|v.get("id"))),
                "status":value.get("status").or_else(||value.get("cell").and_then(|v|v.get("status"))),
                "call":value.get("sequence").map(|_|summarize(&value)),
                "calls":calls,
                "next_cursor":value["calls"]["next_cursor"]
            }).to_string();
        }
        Self {
            text,
            error: false,
            site_id: None,
        }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            text: message.into(),
            error: true,
            site_id: None,
        }
    }
}
fn definition(name: &str, description: &str, schema: Value) -> ToolDefinition {
    ToolDefinition::Function(FunctionTool {
        name: name.into(),
        description: description.into(),
        parameters: schema.as_object().unwrap().clone(),
        output_schema: None,
        strict: Some(false),
    })
}
pub(crate) fn definitions() -> Vec<ToolDefinition> {
    vec![
        definition(
            "code_mode",
            "Run an isolated async JavaScript cell using ctx.platform, ctx.sites, tools and text. The harness assigns and persists a cell ID before dispatch. Return handles for long operations. Lost cells are interrupted, never replayed; inspect saved calls before resuming.",
            json!({"type":"object","required":["source"],"properties":{"source":{"type":"string","maxLength":65536}},"additionalProperties":false}),
        ),
        definition(
            "inspect_cell",
            "Inspect a saved cell and bounded nested-call trace in this run. Follow next_cursor. Includes exact operation identities and results; never executes JavaScript.",
            json!({"type":"object","required":["id"],"properties":{"id":{"type":"string","format":"uuid"},"after":{"type":"string","maxLength":256},"limit":{"type":"integer","minimum":1,"maximum":10}},"additionalProperties":false}),
        ),
        definition(
            "reconcile_call",
            "Explicitly reconcile one uncertain/prepared mutation from an interrupted or failed cell using its original saved input and operation key. This may submit a request that never arrived originally. Never reruns source.",
            json!({"type":"object","required":["id","sequence"],"properties":{"id":{"type":"string","format":"uuid"},"sequence":{"type":"integer","minimum":0,"maximum":999}},"additionalProperties":false}),
        ),
        definition(
            "wait",
            "Release this run's worker slot until any named project run completes, the timer expires, or input arrives. Always inspect status after waking. Use timer-only waits to poll durable commands/sandboxes. Other tool calls in this response resume after waking.",
            json!({"type":"object","required":["seconds"],"properties":{"seconds":{"type":"integer","minimum":1,"maximum":3600},"runIds":{"type":"array","maxItems":16,"uniqueItems":true,"items":{"type":"string","format":"uuid"}}},"additionalProperties":false}),
        ),
    ]
}
pub(crate) fn prepare(name: &str, arguments: &ToolArguments) -> std::result::Result<Plan, String> {
    let value = match arguments {
        ToolArguments::Object(v) => Value::Object(v.clone()),
        ToolArguments::String(v) => {
            serde_json::from_str(v).map_err(|_| "Invalid JSON tool arguments")?
        }
    };
    let schema = definitions()
        .into_iter()
        .find_map(|t| match t {
            ToolDefinition::Function(f) if f.name == name => Some(Value::Object(f.parameters)),
            _ => None,
        })
        .ok_or("Unknown tool")?;
    if !jsonschema::validator_for(&schema)
        .map_err(|_| "Invalid tool schema")?
        .is_valid(&value)
    {
        return Err("Tool arguments do not match the declared schema".into());
    }
    let id = || {
        value["id"]
            .as_str()
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| "Invalid cell UUID".to_owned())
    };
    match name {
        "code_mode" => Ok(Plan::Code {
            input: Input {
                id: Uuid::now_v7(),
                source: value["source"].as_str().unwrap().into(),
            },
        }),
        "inspect_cell" => Ok(Plan::Inspect {
            id: id()?,
            after: value["after"].as_str().map(str::to_owned),
            limit: value["limit"].as_u64().unwrap_or(1) as u32,
        }),
        "reconcile_call" => Ok(Plan::Reconcile {
            id: id()?,
            sequence: value["sequence"].as_u64().unwrap() as u32,
        }),
        "wait" => Ok(Plan::Wait {
            key: Uuid::now_v7(),
            run_ids: serde_json::from_value(value.get("runIds").cloned().unwrap_or(json!([])))
                .map_err(|_| "Invalid run UUID")?,
            wake_at: Utc::now() + chrono::Duration::seconds(value["seconds"].as_i64().unwrap()),
        }),
        _ => Err("Unknown tool".into()),
    }
}
pub(crate) fn register(registry: &mut Registry, web: bool, browser: bool) -> Result<()> {
    if web {
        for definition in [
            tool_firecrawl_search::definition(),
            tool_firecrawl_scrape::definition(),
        ] {
            if let ToolDefinition::Function(f) = definition {
                registry.register(Tool {
                    name: format!("web.{}", f.name),
                    description: f.description,
                    input_schema: Value::Object(f.parameters),
                    effect: Effect::Read,
                })?;
            }
        }
    }
    if browser {
        registry.register(Tool { name:"browser.verify".into(), description:"Load this session's current site frontend in isolated Chromium and report DOM, runtime errors and read-only selector checks. No arbitrary URL, clicks or backend bridge; test backend separately with ctx.sites.invoke.".into(), input_schema:json!({"type":"object","properties":{"selectors":{"type":"array","maxItems":20,"items":{"type":"string","maxLength":256}}},"additionalProperties":false}), effect:Effect::Read })?;
    }
    Ok(())
}
struct DispatcherWithExtras<'a> {
    platform: &'a PlatformDispatcher,
    web: Option<&'a WebTools>,
    browser: Option<&'a Browser>,
    client: &'a RunClient,
}
impl Dispatcher for DispatcherWithExtras<'_> {
    fn prepare(
        &self,
        tool: &Tool,
        key: &str,
        input: Value,
    ) -> std::result::Result<Value, ToolError> {
        if tool.name.starts_with("web.") || tool.name == "browser.verify" {
            Ok(input)
        } else {
            self.platform.prepare(tool, key, input)
        }
    }
    fn invoke<'a>(
        &'a self,
        call: &'a Call,
    ) -> BoxFuture<'a, std::result::Result<Value, ToolError>> {
        Box::pin(async move {
            match call.tool.as_str() {
                "web.search" => {
                    let web = self.web.ok_or_else(|| {
                        ToolError::rejected("UNAVAILABLE", "Web research is unavailable")
                    })?;
                    let args = serde_json::from_value(call.input.clone()).map_err(|_| {
                        ToolError::rejected("INVALID_INPUT", "Invalid search input")
                    })?;
                    tool_firecrawl_search::execute(args, &web.search)
                        .await
                        .map(|v| json!({"content":v.content,"details":v.details}))
                        .map_err(|_| {
                            ToolError::rejected(
                                "SEARCH_FAILED",
                                "Search failed; try a narrower query or check service availability",
                            )
                        })
                }
                "web.scrape" => {
                    let web = self.web.ok_or_else(|| {
                        ToolError::rejected("UNAVAILABLE", "Web research is unavailable")
                    })?;
                    let args = serde_json::from_value(call.input.clone()).map_err(|_| {
                        ToolError::rejected("INVALID_INPUT", "Invalid scrape input")
                    })?;
                    tool_firecrawl_scrape::execute(args, &web.scrape)
                        .await
                        .map(|v| json!({"content":v.content,"details":v.details}))
                        .map_err(|_| {
                            ToolError::rejected(
                                "SCRAPE_FAILED",
                                "Scrape failed; check that the public URL is supported",
                            )
                        })
                }
                "browser.verify" => {
                    self.browser
                        .ok_or_else(|| {
                            ToolError::rejected(
                                "UNAVAILABLE",
                                "Browser verification is unavailable",
                            )
                        })?
                        .verify(self.client, &call.input)
                        .await
                }
                _ => self.platform.invoke(call).await,
            }
        })
    }
}
pub(crate) async fn execute(
    harness: &SitesHarness,
    client: &RunClient,
    mut signals: watch::Receiver<Signals>,
    plan: Plan,
) -> RuntimeResult<Output> {
    let mode = harness.code_mode(client.clone())?;
    let result = match plan {
        Plan::Code { input } => {
            // Recovery must not depend on the worker's current optional-tool configuration.
            let recovered = mode.recover_abandoned(input.id).await;
            let cell = match recovered {
                Ok(Some(cell)) => Ok(cell),
                Ok(None) => {
                    let dispatcher = DispatcherWithExtras {
                        platform: &mode.dispatcher,
                        web: harness.web.as_ref(),
                        browser: harness.browser.as_ref(),
                        client,
                    };
                    let stop = |s: Signals| s.abort_requested || s.draining || s.ownership_lost;
                    let (send, cancel) = watch::channel(stop(*signals.borrow()));
                    let work = mode.engine.execute(
                        input.clone(),
                        &mode.registry,
                        &mode.journal,
                        &dispatcher,
                        cancel,
                    );
                    tokio::pin!(work);
                    loop {
                        tokio::select! {
                            result=&mut work=>break result,
                            changed=signals.changed()=>if changed.is_err()||stop(*signals.borrow_and_update()) { let _=send.send(true);break work.await; }
                        }
                    }
                }
                Err(e) => Err(e),
            };
            let cell = match cell {
                Ok(cell) => cell,
                Err(Error::Invalid(message)) => return Ok(Output::error(message)),
                Err(Error::Conflict) => {
                    return Ok(Output::error(
                        "Saved cell conflicts; inspect the original cell",
                    ));
                }
                Err(Error::Storage) => {
                    return Err(RuntimeError::Invalid(
                        "Cannot recover saved code cell; storage unavailable",
                    ));
                }
            };
            let mut output = Output::success(
                json!({"cellId":cell.id,"status":cell.status,"output":cell.output,"value":cell.value,"error":cell.error,"trace":"Use inspect_cell with cellId to inspect accepted calls; do not replay interrupted source."}),
            );
            output.error = matches!(cell.status, CellStatus::Failed | CellStatus::Interrupted);
            let mut after = None;
            while let Some(inspected) = mode
                .inspect(cell.id, after.as_deref(), 10)
                .await
                .map_err(|_| RuntimeError::Invalid("Cannot inspect saved code cell"))?
            {
                for entry in inspected.calls.items {
                    let value = &entry.value;
                    if value["tool"]
                        .as_str()
                        .is_some_and(|v| v.starts_with("sites."))
                    {
                        if let Some(site) = value["result"]["siteId"]
                            .as_str()
                            .and_then(|v| v.parse().ok())
                        {
                            output.site_id = Some(site);
                        }
                    }
                }
                after = inspected.calls.next_cursor;
                if after.is_none() {
                    break;
                }
            }
            return Ok(output);
        }
        Plan::Inspect { id, after, limit } => mode
            .inspect(id, after.as_deref(), limit)
            .await
            .map(|v| json!(v)),
        Plan::Reconcile { id, sequence } => {
            mode.reconcile_call(id, sequence).await.map(|v| json!(v))
        }
        Plan::Wait { .. } => unreachable!(),
    };
    match result {
        Ok(value) => Ok(Output::success(value)),
        Err(Error::Invalid(message)) => Ok(Output::error(message)),
        Err(Error::Conflict) => Ok(Output::error(
            "Saved operation conflicts; inspect its original identity and arguments",
        )),
        Err(Error::Storage) => Err(RuntimeError::Invalid(
            "Code-mode storage unavailable; recover the saved plan",
        )),
    }
}
