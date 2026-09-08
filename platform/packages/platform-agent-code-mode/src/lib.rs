//! Leased Platform adapter for the reusable code-mode engine.
#![doc = include_str!("../README.md")]
mod journal;
use futures_util::future::BoxFuture;
pub use journal::RunJournal;
use platform_runtime_client::RunClient;
use serde_json::{Value, json};
use std::path::Path;
use tokio::sync::watch;
pub use tool_code_mode as code_mode;
use tool_code_mode::{
    Call, CallStatus, Cell, CellStatus, Dispatcher, Effect, Engine, Error, Extension, Input,
    Journal, Registry, Result, Tool, ToolError,
};
use uuid::Uuid;

pub fn register_platform(registry: &mut Registry) -> Result<()> {
    for method in platform_javascript_sdk::methods() {
        registry.register(Tool {
            name: format!("platform.{}", method.name),
            description: method.description.into(),
            input_schema: method.argument_schema(),
            effect: if method.mutation {
                Effect::Mutation
            } else {
                Effect::Read
            },
        })?;
    }
    Ok(())
}
pub fn platform_extension() -> Extension {
    Extension {
        name: "platform".into(),
        factory: platform_javascript_sdk::agent_factory(),
    }
}
#[derive(Clone)]
pub struct PlatformDispatcher {
    client: RunClient,
}
impl PlatformDispatcher {
    pub fn new(client: RunClient) -> Self {
        Self { client }
    }
}
impl Dispatcher for PlatformDispatcher {
    fn prepare(
        &self,
        tool: &Tool,
        key: &str,
        mut input: Value,
    ) -> std::result::Result<Value, ToolError> {
        let method = if let Some(name) = tool.name.strip_prefix("platform.") {
            platform_javascript_sdk::methods()
                .into_iter()
                .find(|m| m.name == name)
        } else {
            platform_javascript_sdk::sites_methods()
                .into_iter()
                .find(|m| m.name == tool.name)
        }
        .ok_or_else(|| ToolError::rejected("UNKNOWN_TOOL", "Not a shared Platform method"))?;
        if method.mutation {
            let object = input.as_object_mut().ok_or_else(|| {
                ToolError::rejected("INVALID_TOOL_INPUT", "Expected capability arguments")
            })?;
            let options = object
                .entry("options")
                .or_insert(json!({}))
                .as_object_mut()
                .ok_or_else(|| {
                    ToolError::rejected("INVALID_TOOL_INPUT", "Expected mutation options")
                })?;
            options.entry("idempotencyKey").or_insert(json!(key));
        }
        Ok(input)
    }
    fn invoke<'a>(
        &'a self,
        call: &'a Call,
    ) -> BoxFuture<'a, std::result::Result<Value, ToolError>> {
        Box::pin(async move {
            let name = call.tool.strip_prefix("platform.").unwrap_or(&call.tool);
            let known = if call.tool.starts_with("platform.") {
                platform_javascript_sdk::methods()
                    .iter()
                    .any(|m| m.name == name)
            } else {
                platform_javascript_sdk::sites_methods()
                    .iter()
                    .any(|m| m.name == name)
            };
            if !known {
                return Err(ToolError::rejected(
                    "UNKNOWN_TOOL",
                    "Tool is not a registered capability",
                ));
            }
            self.client.platform().call(name,call.input.clone()).await.map_err(|error| {
                match error {
                    platform_runtime_client::Error::Server(e) if (400..500).contains(&e.status) => {
                        // ServerError intentionally omits bodies/credentials in Display.
                        let message = if name == "sites.applyPatch" && matches!(e.status, 400 | 409 | 413 | 422) {
                            format!("{e}. Read ctx.sites.read() again. Use only Update File sections for index.html or backend.js; match entire existing lines, including long HTML lines. Add, Delete and Move are unsupported. Check patch limits and backend syntax; inspect ctx.sites.logs() and the saved operation before retrying uncertain work.")
                        } else {
                            e.to_string()
                        };
                        ToolError::rejected(e.code().unwrap_or("PLATFORM_REJECTED"),&message)
                    }
                    _=>ToolError::uncertain("PLATFORM_UNCERTAIN","Platform outcome may be unknown; inspect and explicitly reconcile the saved nested call"),
                }
            })
        })
    }
}

/// Ready-to-use Platform adapter. For another harness's tools, compose its own
/// Registry/Dispatcher with `RunJournal` and the generic Engine instead.
pub struct AgentCodeMode {
    pub engine: Engine,
    pub registry: Registry,
    pub journal: RunJournal,
    pub dispatcher: PlatformDispatcher,
}
impl AgentCodeMode {
    /// Opt into session-bound live authoring; the server requires a Sites harness
    /// lease and project access. No snapshot/restore tools are registered.
    pub fn with_sites(mut self) -> Result<Self> {
        for method in platform_javascript_sdk::sites_methods() {
            self.registry.register(Tool {
                name: method.name.into(),
                description: method.description.into(),
                input_schema: method.argument_schema(),
                effect: if method.mutation {
                    Effect::Mutation
                } else {
                    Effect::Read
                },
            })?;
        }
        self.engine.extensions.push(Extension {
            name: "sites".into(),
            factory: platform_javascript_sdk::sites_factory(),
        });
        Ok(self)
    }

    pub async fn inspect(
        &self,
        id: Uuid,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Option<tool_code_mode::Inspect>> {
        Engine::inspect(&self.journal, id, after, limit).await
    }
    pub async fn recover_abandoned(&self, id: Uuid) -> Result<Option<Cell>> {
        Engine::recover_abandoned(&self.journal, id).await
    }
    pub fn new(client: RunClient, executable: impl AsRef<Path>) -> Result<Self> {
        let mut registry = Registry::default();
        register_platform(&mut registry)?;
        let mut engine = Engine::new(executable);
        engine.extensions.push(platform_extension());
        Ok(Self {
            engine,
            registry,
            journal: RunJournal::new(client.clone()),
            dispatcher: PlatformDispatcher::new(client),
        })
    }
    pub async fn execute(
        &self,
        input: Input,
        mut signals: watch::Receiver<harness_runtime::Signals>,
    ) -> Result<Cell> {
        let stop =
            |s: harness_runtime::Signals| s.abort_requested || s.draining || s.ownership_lost;
        let (send, cancel) = watch::channel(stop(*signals.borrow()));
        let work = self.engine.execute(
            input,
            &self.registry,
            &self.journal,
            &self.dispatcher,
            cancel,
        );
        tokio::pin!(work);
        loop {
            tokio::select! {
                result=&mut work=>return result,
                changed=signals.changed()=>{
                    if changed.is_err()||stop(*signals.borrow_and_update()) {let _=send.send(true);return work.await;}
                }
            }
        }
    }
    /// Explicit operation reconciliation, never cell-source replay. If the original
    /// request never reached Platform this can submit it for the first time. Requires
    /// an interrupted/failed cell, its exact saved arguments, and a live run lease.
    pub async fn reconcile_call(&self, cell_id: Uuid, sequence: u32) -> Result<Call> {
        let cell = self
            .journal
            .get(&tool_code_mode::cell_key(cell_id))
            .await?
            .ok_or(Error::Invalid("Cell not found"))?;
        let cell: Cell = serde_json::from_value(cell.value).map_err(|_| Error::Storage)?;
        if !matches!(cell.status, CellStatus::Interrupted | CellStatus::Failed) {
            return Err(Error::Invalid(
                "Interrupt the cell before reconciling a call",
            ));
        }
        let key = tool_code_mode::call_key(cell_id, sequence);
        let entry = self
            .journal
            .get(&key)
            .await?
            .ok_or(Error::Invalid("Call not found"))?;
        let mut call: Call = serde_json::from_value(entry.value).map_err(|_| Error::Storage)?;
        if matches!(call.status, CallStatus::Succeeded | CallStatus::Rejected) {
            return Ok(call);
        }
        let tool = self
            .registry
            .get(&call.tool)
            .ok_or(Error::Invalid("Saved tool is not registered"))?;
        if tool.effect != Effect::Mutation || call.effect != Effect::Mutation {
            return Err(Error::Invalid(
                "Only saved Platform mutations can be reconciled",
            ));
        }
        match self.dispatcher.invoke(&call).await {
            Ok(value)
                if serde_json::to_vec(&value)
                    .map_err(|_| Error::Storage)?
                    .len()
                    <= self.engine.limits.result_bytes =>
            {
                call.status = CallStatus::Succeeded;
                call.result = Some(value);
                call.error = None;
            }
            Ok(_) => {
                call.status = CallStatus::Uncertain;
                call.error = Some(ToolError::uncertain(
                    "TOOL_RESULT_LIMIT",
                    "Result remains too large; inspect the operation through a bounded getter",
                ));
            }
            Err(e) => {
                call.status = if e.uncertain {
                    CallStatus::Uncertain
                } else {
                    CallStatus::Rejected
                };
                call.error = Some(e);
            }
        }
        self.journal.put(&key, entry.version, json!(call)).await?;
        Ok(call)
    }
}
