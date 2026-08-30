use std::{
    collections::HashMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub const DEFAULT_EXEC_YIELD_TIME_MS: u64 = 10_000;
pub const DEFAULT_WAIT_YIELD_TIME_MS: u64 = 10_000;
pub const DEFAULT_MAX_OUTPUT_TOKENS_PER_EXEC_CALL: usize = 10_000;
pub const DEFAULT_IMAGE_DETAIL: ImageDetail = ImageDetail::High;

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct CellId(String);

impl CellId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for CellId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for CellId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolName {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

impl ToolName {
    #[must_use]
    pub fn plain(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            namespace: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeModeToolKind {
    Function,
    Freeform,
}

/// Runtime metadata for one function exposed on the JavaScript `tools` object.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolDefinition {
    /// JavaScript-facing name after any harness namespace normalization.
    pub name: String,
    /// Canonical name sent back to the harness dispatcher.
    pub tool_name: ToolName,
    pub description: String,
    pub kind: CodeModeToolKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EnabledToolMetadata {
    pub(crate) tool_name: ToolName,
    pub(crate) global_name: String,
    pub(crate) description: String,
    pub(crate) kind: CodeModeToolKind,
}

pub(crate) fn enabled_tool_metadata(definition: &ToolDefinition) -> EnabledToolMetadata {
    EnabledToolMetadata {
        tool_name: definition.tool_name.clone(),
        global_name: definition.name.clone(),
        description: definition.description.clone(),
        kind: definition.kind,
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExecuteRequest {
    pub tool_call_id: String,
    pub enabled_tools: Vec<ToolDefinition>,
    pub source: String,
    pub yield_time_ms: Option<u64>,
    pub max_output_tokens: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WaitRequest {
    pub cell_id: CellId,
    pub yield_time_ms: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WaitToPendingRequest {
    pub cell_id: CellId,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum RuntimeResponse {
    Yielded {
        cell_id: CellId,
        content_items: Vec<FunctionCallOutputContentItem>,
    },
    Terminated {
        cell_id: CellId,
        content_items: Vec<FunctionCallOutputContentItem>,
    },
    Result {
        cell_id: CellId,
        content_items: Vec<FunctionCallOutputContentItem>,
        error_text: Option<String>,
    },
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub enum WaitOutcome {
    LiveCell(RuntimeResponse),
    MissingCell(RuntimeResponse),
}

impl From<WaitOutcome> for RuntimeResponse {
    fn from(outcome: WaitOutcome) -> Self {
        match outcome {
            WaitOutcome::LiveCell(response) | WaitOutcome::MissingCell(response) => response,
        }
    }
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub enum ExecuteToPendingOutcome {
    Pending {
        cell_id: CellId,
        content_items: Vec<FunctionCallOutputContentItem>,
        pending_tool_call_ids: Vec<String>,
    },
    Completed(RuntimeResponse),
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub enum WaitToPendingOutcome {
    LiveCell(ExecuteToPendingOutcome),
    MissingCell(RuntimeResponse),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CodeModeNestedToolCall {
    pub cell_id: CellId,
    pub runtime_tool_call_id: String,
    pub tool_name: ToolName,
    pub tool_kind: CodeModeToolKind,
    pub input: Option<Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageDetail {
    Auto,
    Low,
    High,
    Original,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FunctionCallOutputContentItem {
    InputText {
        text: String,
    },
    InputImage {
        image_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ImageDetail>,
    },
    InputAudio {
        audio_url: String,
    },
}

pub type CodeModeSessionResultFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;
pub type ToolInvocationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;
pub type NotificationFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
pub type StateStoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// Serializable state shared by all JavaScript cells in one Codex session.
///
/// Harnesses may implement this with Postgres. The in-process runtime asks for
/// a fresh value snapshot when a cell starts and commits `store()` writes only
/// when that cell completes.
pub trait CodeModeStateStore: Send + Sync {
    fn allocate_cell_id<'a>(&'a self) -> StateStoreFuture<'a, CellId>;

    fn load_values<'a>(&'a self) -> StateStoreFuture<'a, HashMap<String, Value>>;

    fn commit_values<'a>(&'a self, writes: HashMap<String, Value>) -> StateStoreFuture<'a, ()>;
}

/// Default process-local state store used by standalone sessions and tests.
pub struct InMemoryCodeModeStateStore {
    next_cell_id: AtomicU64,
    stored_values: Mutex<HashMap<String, Value>>,
}

impl InMemoryCodeModeStateStore {
    #[must_use]
    pub fn new(next_cell_id: u64, stored_values: HashMap<String, Value>) -> Self {
        Self {
            next_cell_id: AtomicU64::new(next_cell_id.max(1)),
            stored_values: Mutex::new(stored_values),
        }
    }
}

impl Default for InMemoryCodeModeStateStore {
    fn default() -> Self {
        Self::new(1, HashMap::new())
    }
}

impl CodeModeStateStore for InMemoryCodeModeStateStore {
    fn allocate_cell_id<'a>(&'a self) -> StateStoreFuture<'a, CellId> {
        Box::pin(async move {
            self.next_cell_id
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                    next.checked_add(1)
                })
                .map(|value| CellId::new(value.to_string()))
                .map_err(|_| "code mode session exhausted its cell ID space".to_owned())
        })
    }

    fn load_values<'a>(&'a self) -> StateStoreFuture<'a, HashMap<String, Value>> {
        Box::pin(async move { Ok(self.stored_values.lock().await.clone()) })
    }

    fn commit_values<'a>(&'a self, writes: HashMap<String, Value>) -> StateStoreFuture<'a, ()> {
        Box::pin(async move {
            self.stored_values.lock().await.extend(writes);
            Ok(())
        })
    }
}

/// Optional resource limits shared by every cell in one session.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CodeModeSessionCellExecutionLimits {
    pub max_yield_time_ms: Option<u64>,
    /// Reserved for runtime providers with enforceable heap limits. The
    /// current in-process V8 session intentionally ignores this field.
    pub max_heap_size_bytes: Option<usize>,
}

pub struct StartedCell {
    pub cell_id: CellId,
    initial_response: CodeModeSessionResultFuture<'static, RuntimeResponse>,
}

impl StartedCell {
    pub fn from_result_receiver(
        cell_id: CellId,
        receiver: oneshot::Receiver<Result<RuntimeResponse, String>>,
    ) -> Self {
        Self::from_future(cell_id, async move {
            receiver
                .await
                .map_err(|_| "exec runtime ended unexpectedly".to_string())?
        })
    }

    pub fn from_future(
        cell_id: CellId,
        response: impl Future<Output = Result<RuntimeResponse, String>> + Send + 'static,
    ) -> Self {
        Self {
            cell_id,
            initial_response: Box::pin(response),
        }
    }

    pub async fn initial_response(self) -> Result<RuntimeResponse, String> {
        self.initial_response.await
    }
}

/// Harness callbacks used by live JavaScript cells.
pub trait CodeModeSessionDelegate: Send + Sync {
    fn invoke_tool<'a>(
        &'a self,
        invocation: CodeModeNestedToolCall,
        cancellation_token: CancellationToken,
    ) -> ToolInvocationFuture<'a>;

    fn notify<'a>(
        &'a self,
        call_id: String,
        cell_id: CellId,
        text: String,
        cancellation_token: CancellationToken,
    ) -> NotificationFuture<'a>;

    fn cell_closed(&self, cell_id: &CellId);
}

pub struct NoopCodeModeSessionDelegate;

impl CodeModeSessionDelegate for NoopCodeModeSessionDelegate {
    fn invoke_tool<'a>(
        &'a self,
        _invocation: CodeModeNestedToolCall,
        cancellation_token: CancellationToken,
    ) -> ToolInvocationFuture<'a> {
        Box::pin(async move {
            cancellation_token.cancelled().await;
            Err("code mode nested tools are unavailable".to_string())
        })
    }

    fn notify<'a>(
        &'a self,
        _call_id: String,
        _cell_id: CellId,
        _text: String,
        _cancellation_token: CancellationToken,
    ) -> NotificationFuture<'a> {
        Box::pin(async { Ok(()) })
    }

    fn cell_closed(&self, _cell_id: &CellId) {}
}

/// Durable session contract implemented by the in-process runtime.
pub trait CodeModeSession: Send + Sync {
    fn execute<'a>(
        &'a self,
        request: ExecuteRequest,
    ) -> CodeModeSessionResultFuture<'a, StartedCell>;

    fn wait<'a>(&'a self, request: WaitRequest) -> CodeModeSessionResultFuture<'a, WaitOutcome>;

    fn terminate<'a>(&'a self, cell_id: CellId) -> CodeModeSessionResultFuture<'a, WaitOutcome>;

    fn shutdown<'a>(&'a self) -> CodeModeSessionResultFuture<'a, ()>;
}
