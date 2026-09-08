//! Disposable JavaScript cells with a caller-selected tool registry and durable journal.
#![doc = include_str!("../README.md")]
mod engine;
pub mod guest;
pub mod sandbox;
pub use engine::{Engine, Inspect};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("Durable code-mode storage is unavailable; inspect before retrying")]
    Storage,
    #[error("The saved cell or journal entry conflicts with this request")]
    Conflict,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Read,
    Mutation,
}

/// A trusted adapter's tool declaration. Names are exact keys in `tools`, so
/// namespaced and freeform tools need no lossy identifier normalization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub effect: Effect,
}
#[derive(Clone, Default)]
pub struct Registry {
    tools: BTreeMap<String, Tool>,
}
impl Registry {
    pub fn register(&mut self, tool: Tool) -> Result<()> {
        if tool.name.is_empty()
            || tool.name.len() > 128
            || tool.name.chars().any(char::is_control)
            || tool.description.len() > 8192
            || self.tools.len() >= 128
            || self.tools.contains_key(&tool.name)
        {
            return Err(Error::Invalid(
                "Invalid, duplicate or excessive code-mode tool definition",
            ));
        }
        jsonschema::draft202012::new(&tool.input_schema)
            .map_err(|_| Error::Invalid("Tool schema must resolve locally"))?;
        if serde_json::to_vec(&tool).map_err(|_| Error::Storage)?.len() > 65536 {
            return Err(Error::Invalid("Tool definition exceeds 64 KiB"));
        }
        self.tools.insert(tool.name.clone(), tool);
        Ok(())
    }
    pub fn tools(&self) -> impl Iterator<Item = &Tool> {
        self.tools.values()
    }
    pub fn get(&self, name: &str) -> Option<&Tool> {
        self.tools.get(name)
    }
    /// Embedders can render this definition into their model provider's tool
    /// format. The harness must persist the cell ID before invoking the engine.
    pub fn code_mode_tool(&self) -> Tool {
        Tool {
            name: "code_mode".into(),
            description: format!(
                "Run a disposable JavaScript async cell. Use tools[name](input), text(value), and ALL_TOOLS for discovery. Registered tools: {}. Nested calls run sequentially. Return durable handles for long work. Cell IDs are stable: repeating an ID inspects saved state and never reruns source. Each accepted call remains inspectable even if the cell fails or is interrupted.",
                self.tools
                    .keys()
                    .take(32)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            input_schema: serde_json::json!({"type":"object","required":["id","source"],"properties":{"id":{"type":"string","format":"uuid"},"source":{"type":"string","maxLength":65536}},"additionalProperties":false}),
            effect: Effect::Mutation,
        }
    }
}

/// Trusted SDK factory receiving `(callTool)`, returning one `ctx[name]` value.
/// Factories contain no credentials. They cannot expand the parent registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extension {
    pub name: String,
    pub factory: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Limits {
    pub max_calls: u32,
    pub source_bytes: usize,
    pub output_bytes: usize,
    pub argument_bytes: usize,
    pub result_bytes: usize,
    pub timeout_ms: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_calls: 100,
            source_bytes: 65536,
            output_bytes: 65536,
            argument_bytes: 65536,
            result_bytes: 131072,
            timeout_ms: 30000,
        }
    }
}
impl Limits {
    pub fn validate(&self) -> Result<()> {
        if self.max_calls == 0
            || self.max_calls > 1000
            || self.source_bytes == 0
            || self.source_bytes > 65536
            || self.output_bytes == 0
            || self.output_bytes > 65536
            || self.argument_bytes == 0
            || self.argument_bytes > 65536
            || self.result_bytes == 0
            || self.result_bytes > 131072
            || self.timeout_ms == 0
            || self.timeout_ms > 60000
        {
            return Err(Error::Invalid("Code-mode limits exceed supported bounds"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub id: Uuid,
    pub source: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellStatus {
    Running,
    Completed,
    Failed,
    Interrupted,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cell {
    pub id: Uuid,
    pub owner: String,
    pub fingerprint: String,
    pub source: String,
    pub status: CellStatus,
    pub output: Vec<Value>,
    pub value: Option<Value>,
    pub error: Option<ToolError>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    Prepared,
    Succeeded,
    Rejected,
    Uncertain,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Call {
    pub cell_id: Uuid,
    pub sequence: u32,
    /// Stable identity available to every tool adapter before dispatch.
    pub operation_key: String,
    pub tool: String,
    pub effect: Effect,
    pub input: Value,
    pub status: CallStatus,
    pub result: Option<Value>,
    pub error: Option<ToolError>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolError {
    pub code: String,
    pub message: String,
    /// True means the effect may have been accepted; never auto-replay source.
    pub uncertain: bool,
}
impl ToolError {
    pub fn rejected(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            uncertain: false,
        }
    }
    pub fn uncertain(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            uncertain: true,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub key: String,
    pub version: u64,
    pub value: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub items: Vec<Entry>,
    pub next_cursor: Option<String>,
}

/// Durable compare-and-set storage scoped by the embedding harness. `put` must
/// atomically check both version and current owner authority. Version zero means
/// absent. A new owner may inspect old records but must never replay their source.
pub trait Journal: Send + Sync {
    fn owner(&self) -> &str;
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Entry>>>;
    fn put<'a>(&'a self, key: &'a str, expected: u64, value: Value)
    -> BoxFuture<'a, Result<Entry>>;
    fn list<'a>(
        &'a self,
        prefix: &'a str,
        after: Option<&'a str>,
        limit: u32,
    ) -> BoxFuture<'a, Result<Page>>;
}
/// Trusted tool implementation. `prepare` is pure normalization only; no effects
/// may precede journal admission. Mutations should forward the operation key to
/// an idempotent target. Errors must be sanitized, including upstream diagnostics.
pub trait Dispatcher: Send + Sync {
    fn prepare(
        &self,
        _tool: &Tool,
        _operation_key: &str,
        input: Value,
    ) -> std::result::Result<Value, ToolError> {
        Ok(input)
    }
    fn invoke<'a>(&'a self, call: &'a Call)
    -> BoxFuture<'a, std::result::Result<Value, ToolError>>;
}
pub fn cell_key(id: Uuid) -> String {
    format!("cell.{id}")
}
pub fn call_prefix(id: Uuid) -> String {
    format!("call.{id}.")
}
pub fn call_key(id: Uuid, sequence: u32) -> String {
    format!("call.{id}.{sequence:06}")
}
