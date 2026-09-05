//! Whole-file writes with caller-owned operation identity and observed revisions.
#![doc = include_str!("../README.md")]

use execution_core::{
    BinaryData, ExecutionError, ExecutionErrorCode as Code, ExecutionHostId, ExecutionPath,
    ExecutionResult, ExecutionRuntime, FileRevision, OperationContext, OperationId, WriteCondition,
    WriteFileRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const NAME: &str = "write";
pub const DESCRIPTION: &str = "Write the complete UTF-8 content of a file on the execution host. Creates missing parent directories. An existing file must first be read; the write fails if its contents have changed since that read or the last successful mutation. Content replaces the entire file exactly as supplied, without adding a newline. Empty content creates or truncates an empty file. Use edit for partial replacements. Relative paths resolve against the current working directory; absolute paths must be inside a registered execution root. Parent (..) segments are unsupported; use an absolute path instead.";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteInput {
    pub file_path: String,
    pub content: String,
}

pub fn input_schema() -> Value {
    json!({"type":"object","properties":{
        "file_path":{"type":"string","minLength":1,"description":"Path to the file to write. Relative paths resolve against the current working directory; absolute paths refer to the execution host."},
        "content":{"type":"string","description":"The complete content to write, replacing any existing content."}
    },"required":["file_path","content"],"additionalProperties":false})
}

/// Supplied by the trusted harness from a read or successful mutation. Persist
/// under (host_id, path.root_id, path.path), never just a model-facing filename.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedFile {
    pub host_id: ExecutionHostId,
    pub path: ExecutionPath,
    pub revision: FileRevision,
}

/// Persist alongside the input before dispatching recovery-critical writes.
/// Reuse the exact state and input on a deliberate replay, including the old
/// revision; replacing it with a new observation changes the operation payload.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteState {
    pub operation_id: OperationId,
    pub observed: Option<ObservedFile>,
}

#[derive(Clone, Debug)]
pub struct WriteConfig {
    /// UTF-8 bytes, also bounded by the host's advertised max_write_bytes.
    pub max_content_bytes: u64,
}
impl Default for WriteConfig {
    fn default() -> Self {
        Self {
            max_content_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WriteOutput {
    pub host_id: ExecutionHostId,
    pub path: ExecutionPath,
    pub created: bool,
    pub bytes_written: u64,
    pub revision: FileRevision,
}
impl WriteOutput {
    pub fn to_text(&self) -> String {
        // JSON quoting keeps unusual filenames on one display line.
        format!(
            "Wrote {} bytes to {} (root {}).",
            self.bytes_written,
            serde_json::to_string(&self.path.path).expect("string serialization"),
            serde_json::to_string(self.path.root_id.as_str()).expect("string serialization")
        )
    }
    pub fn observation(&self) -> ObservedFile {
        ObservedFile {
            host_id: self.host_id.clone(),
            path: self.path.clone(),
            revision: self.revision.clone(),
        }
    }
}

pub struct WriteTool<'a> {
    runtime: &'a dyn ExecutionRuntime,
    cwd: ExecutionPath,
    config: WriteConfig,
}
impl<'a> WriteTool<'a> {
    pub fn new(
        runtime: &'a dyn ExecutionRuntime,
        cwd: ExecutionPath,
        config: WriteConfig,
    ) -> ExecutionResult<Self> {
        tool_filesystem::validate_cwd(runtime.descriptor(), &cwd)?;
        if config.max_content_bytes == 0 {
            return Err(error(
                Code::InvalidRequest,
                "max_content_bytes must be positive",
            ));
        }
        Ok(Self {
            runtime,
            cwd,
            config,
        })
    }

    /// Resolve before looking up the harness's stored observation for this file.
    pub fn resolve_path(&self, file_path: &str) -> ExecutionResult<ExecutionPath> {
        tool_filesystem::resolve_path(self.runtime.descriptor(), &self.cwd, file_path)
    }

    pub async fn execute(
        &self,
        context: &OperationContext,
        input: WriteInput,
        state: WriteState,
    ) -> ExecutionResult<WriteOutput> {
        context.checkpoint()?;
        let path = self.resolve_path(&input.file_path)?;
        let host = self.runtime.descriptor();
        let bytes = input.content.len() as u64;
        let limit = self
            .config
            .max_content_bytes
            .min(host.limits.max_write_bytes.unwrap_or(u64::MAX));
        if bytes > limit {
            return Err(error(
                Code::ResourceExhausted,
                format!("content contains {bytes} UTF-8 bytes; write limit is {limit}"),
            ));
        }
        if path.path == "." {
            return Err(error(
                Code::IsDirectory,
                "cannot replace an execution root with a file",
            ));
        }
        if host
            .roots
            .iter()
            .any(|root| root.id == path.root_id && root.read_only)
        {
            return Err(error(
                Code::ReadOnlyRoot,
                "the selected execution root is read-only",
            ));
        }
        let observed = state.observed.is_some();
        let condition = if let Some(observation) = state.observed {
            if observation.host_id != host.host_id || observation.path != path {
                return Err(error(
                    Code::InvalidRequest,
                    "observed revision belongs to another host or path; read this file first",
                ));
            }
            WriteCondition::MatchRevision {
                revision: observation.revision,
            }
        } else {
            WriteCondition::MustNotExist
        };
        let result = self.runtime.filesystem().write(context, WriteFileRequest {
            operation_id: state.operation_id,
            path: path.clone(),
            data: BinaryData::new(input.content.into_bytes()),
            condition,
            create_parents: true,
            follow_symlinks: true,
        }).await.map_err(|mut failure| {
            if !observed && failure.code == Code::AlreadyExists {
                failure.message = format!("File already exists; read it before overwriting. {}", failure.message);
            } else if failure.code == Code::RevisionConflict {
                failure.message = format!("File changed since it was last observed; read it again before overwriting. {}", failure.message);
            }
            failure
        })?;
        // Do not perform another context checkpoint after an acknowledged write:
        // reporting cancellation now would conceal a successful remote mutation.
        if result.path != path || result.bytes_written != bytes || result.existed != observed {
            return Err(error(
                Code::Internal,
                "host returned an inconsistent write result; the write may have completed",
            ));
        }
        Ok(WriteOutput {
            host_id: host.host_id.clone(),
            path,
            created: !result.existed,
            bytes_written: result.bytes_written,
            revision: result.revision,
        })
    }
}

fn error(code: Code, message: impl Into<String>) -> ExecutionError {
    ExecutionError::new(code, message).with_detail("source", "tool-write")
}
