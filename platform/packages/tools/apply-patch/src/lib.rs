//! Codex-compatible freeform patches over an injected execution runtime.
#![doc = include_str!("../README.md")]

mod apply;
mod filesystem;
mod parser;
mod prepare;
mod update;

use execution_core::{
    ExecutionError, ExecutionErrorCode as Code, ExecutionHostId, ExecutionPath, ExecutionResult,
    ExecutionRuntime, OperationContext, OperationId, RemovePathRequest, SupervisorGenerationId,
    WriteFileRequest,
};
use llm_contracts::{CustomTool, CustomToolFormat, GrammarSyntax, ToolArguments, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use parser::{Hunk, ParseError, ParsedPatch, UpdateFileChunk, parse_patch};

pub const NAME: &str = "apply_patch";
pub const DESCRIPTION: &str = "The `apply_patch` tool can be used to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.";
pub const LARK_GRAMMAR: &str = include_str!("../assets/apply_patch.lark");

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPatchFileUpdateMode {
    #[default]
    NormalizeToLf,
    PreserveLineEndings,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ApplyPatchConfig {
    pub max_patch_bytes: u64,
    pub max_file_bytes: u64,
    pub chunk_bytes: u64,
    pub max_operations: usize,
    pub update_file_mode: ApplyPatchFileUpdateMode,
}

impl Default for ApplyPatchConfig {
    fn default() -> Self {
        Self {
            max_patch_bytes: u64::MAX,
            max_file_bytes: u64::MAX,
            chunk_bytes: 1024 * 1024,
            max_operations: usize::MAX,
            update_file_mode: ApplyPatchFileUpdateMode::default(),
        }
    }
}

/// One caller-owned identity from which the prepared per-mutation IDs derive.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyPatchState {
    pub operation_id: OperationId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppliedPatchChangeKind {
    Add,
    Modify,
    Delete,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedPatchChange {
    pub kind: AppliedPatchChangeKind,
    /// The path spelling from the patch, including a move destination when present.
    pub path: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum PreparedOperation {
    Write { request: WriteFileRequest },
    Remove { request: RemovePathRequest },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PreparedHunk {
    source: ExecutionPath,
    destination: Option<ExecutionPath>,
    hunk: Hunk,
}

/// Initial verification result. Persist before starting application. Updates
/// are derived again at application time, against the then-current filesystem.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedPatch {
    version: u32,
    host_id: ExecutionHostId,
    generation: SupervisorGenerationId,
    operation_id: OperationId,
    update_file_mode: ApplyPatchFileUpdateMode,
    hunks: Vec<PreparedHunk>,
    changes: Vec<AppliedPatchChange>,
}

/// Caller-owned application checkpoint. Persist after every successful step,
/// including the step that prepares a mutation without dispatching it.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunningPatch {
    prepared: PreparedPatch,
    hunk_index: usize,
    applied_operations: usize,
    pending: Vec<PreparedOperation>,
    next_operation: usize,
}

impl std::fmt::Debug for RunningPatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunningPatch")
            .field("prepared", &self.prepared)
            .field("hunk_index", &self.hunk_index)
            .field("applied_operations", &self.applied_operations)
            .field("pending_operations", &self.pending.len())
            .finish()
    }
}

impl RunningPatch {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.hunk_index == self.prepared.hunks.len()
    }

    #[must_use]
    pub fn applied_operations(&self) -> usize {
        self.applied_operations
    }
}

/// CheckpointRequired never dispatches the newly prepared next operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PatchProgress {
    CheckpointRequired,
    Complete(ApplyPatchOutput),
}

impl std::fmt::Debug for PreparedPatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedPatch")
            .field("host_id", &self.host_id)
            .field("operations", &self.operation_count())
            .field("changes", &self.changes)
            .finish()
    }
}

impl PreparedPatch {
    #[must_use]
    pub fn host_id(&self) -> &ExecutionHostId {
        &self.host_id
    }

    #[must_use]
    pub fn operation_count(&self) -> usize {
        self.hunks
            .iter()
            .map(|hunk| if hunk.destination.is_some() { 2 } else { 1 })
            .sum()
    }

    #[must_use]
    pub fn changes(&self) -> &[AppliedPatchChange] {
        &self.changes
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyPatchOutput {
    pub host_id: ExecutionHostId,
    pub changes: Vec<AppliedPatchChange>,
}

impl ApplyPatchOutput {
    /// Codex's git-style model-facing success summary.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut output = "Success. Updated the following files:\n".to_string();
        for kind in [
            AppliedPatchChangeKind::Add,
            AppliedPatchChangeKind::Modify,
            AppliedPatchChangeKind::Delete,
        ] {
            for change in self.changes.iter().filter(|change| change.kind == kind) {
                let prefix = match kind {
                    AppliedPatchChangeKind::Add => 'A',
                    AppliedPatchChangeKind::Modify => 'M',
                    AppliedPatchChangeKind::Delete => 'D',
                };
                output.push_str(&format!("{prefix} {}\n", change.path));
            }
        }
        output
    }

    /// Codex code mode returns an empty object after emitting the text result.
    #[must_use]
    pub fn code_mode_result(&self) -> Value {
        json!({})
    }
}

pub struct ApplyPatchTool<'a> {
    runtime: &'a dyn ExecutionRuntime,
    cwd: ExecutionPath,
    config: ApplyPatchConfig,
}

impl<'a> ApplyPatchTool<'a> {
    pub fn new(
        runtime: &'a dyn ExecutionRuntime,
        cwd: ExecutionPath,
        config: ApplyPatchConfig,
    ) -> ExecutionResult<Self> {
        tool_filesystem::validate_cwd(runtime.descriptor(), &cwd)?;
        if config.max_patch_bytes == 0
            || config.max_file_bytes == 0
            || config.chunk_bytes == 0
            || config.max_operations == 0
        {
            return Err(error(
                Code::InvalidRequest,
                "apply_patch size, chunk, and operation limits must be positive",
            ));
        }
        Ok(Self {
            runtime,
            cwd,
            config,
        })
    }

    #[must_use]
    pub fn definition(&self) -> ToolDefinition {
        definition()
    }

    /// Resolve one model-facing patch path without consulting the worker OS.
    pub fn resolve_path(&self, path: &str) -> ExecutionResult<ExecutionPath> {
        tool_filesystem::resolve_path_normalized(self.runtime.descriptor(), &self.cwd, path)
    }

    /// Parse and verify every hunk without mutating the filesystem.
    pub async fn prepare(
        &self,
        context: &OperationContext,
        patch: &str,
        state: ApplyPatchState,
    ) -> ExecutionResult<PreparedPatch> {
        prepare::prepare(self, context, patch, state).await
    }

    /// Convenience adapter for the raw-string custom-tool contract.
    pub async fn prepare_arguments(
        &self,
        context: &OperationContext,
        arguments: &ToolArguments,
        state: ApplyPatchState,
    ) -> ExecutionResult<PreparedPatch> {
        let ToolArguments::String(patch) = arguments else {
            return Err(error(
                Code::InvalidRequest,
                "apply_patch is a freeform tool and requires raw patch text, not JSON",
            ));
        };
        self.prepare(context, patch, state).await
    }

    /// Start a caller-owned checkpoint after initial verification.
    pub fn start(&self, prepared: &PreparedPatch) -> ExecutionResult<RunningPatch> {
        apply::validate(self, prepared)?;
        Ok(RunningPatch {
            prepared: prepared.clone(),
            hunk_index: 0,
            applied_operations: 0,
            pending: Vec::new(),
            next_operation: 0,
        })
    }

    /// Prepare or dispatch one checkpointed step. Persist the mutated running
    /// value before calling again. On transport uncertainty replay that exact
    /// value: it retains the same operation ID and request bytes.
    pub async fn step(
        &self,
        context: &OperationContext,
        running: &mut RunningPatch,
    ) -> ExecutionResult<PatchProgress> {
        apply::step(self, context, running).await
    }

    /// Convenience for in-process callers. Durable harnesses must use `step`
    /// and persist each checkpoint instead. Reusing a completed running value
    /// returns its result without repeating filesystem effects.
    pub async fn apply(
        &self,
        context: &OperationContext,
        running: &mut RunningPatch,
    ) -> ExecutionResult<ApplyPatchOutput> {
        loop {
            match self.step(context, running).await? {
                PatchProgress::CheckpointRequired => {}
                PatchProgress::Complete(output) => return Ok(output),
            }
        }
    }

    fn validate_target(&self, path: &ExecutionPath) -> ExecutionResult<()> {
        tool_filesystem::validate_cwd(self.runtime.descriptor(), path)?;
        if path.path == "." {
            return Err(error(Code::IsDirectory, "cannot patch an execution root"));
        }
        if self
            .runtime
            .descriptor()
            .roots
            .iter()
            .any(|root| root.id == path.root_id && root.read_only)
        {
            return Err(error(
                Code::ReadOnlyRoot,
                "the selected execution root is read-only",
            ));
        }
        Ok(())
    }
}

/// Exact custom-tool definition consumed by `llm-contracts` and `llm-client`.
#[must_use]
pub fn definition() -> ToolDefinition {
    ToolDefinition::Custom(CustomTool {
        name: NAME.to_string(),
        description: DESCRIPTION.to_string(),
        format: CustomToolFormat {
            syntax: GrammarSyntax::Lark,
            definition: LARK_GRAMMAR.to_string(),
        },
    })
}

pub(crate) fn error(code: Code, message: impl Into<String>) -> ExecutionError {
    ExecutionError::new(code, message).with_detail("source", "tool-apply-patch")
}
