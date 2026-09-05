//! Exact text edits prepared before dispatching a replayable conditional write.
#![doc = include_str!("../README.md")]

mod content;
mod matching;

use execution_core::{
    ExecutionError, ExecutionErrorCode as Code, ExecutionHostId, ExecutionPath, ExecutionResult,
    ExecutionRuntime, FileRevision, OperationContext, OperationId,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
pub use tool_write::ObservedFile;
use tool_write::{WriteConfig, WriteInput, WriteState, WriteTool};

pub const NAME: &str = "edit";
pub const DESCRIPTION: &str = "Replace exact text in an existing UTF-8 file on the execution host. Read the file first. old_string must match exactly, including whitespace, indentation, and line endings; omit read-output line-number prefixes. By default the match must be unique; provide more surrounding text if it is ambiguous, or set replace_all to replace every non-overlapping occurrence. Empty old_string and identical old_string/new_string are errors. Empty new_string deletes matched text. The edit fails if the file changed since it was observed. Relative paths resolve against the current working directory; absolute paths must be inside a registered execution root. Parent (..) segments are unsupported; use an absolute path instead. Use write to create or completely replace a file.";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EditInput {
    pub file_path: String,
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub replace_all: bool,
}

pub fn input_schema() -> Value {
    json!({"type":"object","properties":{
        "file_path":{"type":"string","minLength":1,"description":"Path to the file to edit. Relative paths resolve against the current working directory; absolute paths refer to the execution host."},
        "old_string":{"type":"string","minLength":1,"description":"The exact text to replace, including whitespace and indentation. Do not include line-number prefixes from read output."},
        "new_string":{"type":"string","description":"The replacement text. Must differ from old_string. Use an empty string to delete the matched text."},
        "replace_all":{"type":"boolean","description":"Replace every non-overlapping occurrence of old_string. Defaults to false."}
    },"required":["file_path","old_string","new_string"],"additionalProperties":false})
}

#[derive(Clone, Debug)]
pub struct EditConfig {
    /// Maximum original and resulting file size, measured in UTF-8 bytes.
    pub max_file_bytes: u64,
    /// Also bounded by the execution host's max_read_bytes.
    pub chunk_bytes: u64,
}
impl Default for EditConfig {
    fn default() -> Self {
        Self {
            max_file_bytes: 16 * 1024 * 1024,
            chunk_bytes: 1024 * 1024,
        }
    }
}

/// Trusted harness input, excluded from the model schema. Observations must
/// come from a read or successful mutation of this exact host/root/path.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EditState {
    pub operation_id: OperationId,
    pub observed: ObservedFile,
}

/// Persist this whole value before applying a recovery-critical edit. Applying
/// it again uses the original revision and resulting content without re-reading.
/// Treat persisted plans as trusted harness state, never as model arguments.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedEdit {
    state: EditState,
    content: String,
    replacements: u64,
}
impl std::fmt::Debug for PreparedEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedEdit")
            .field("state", &self.state)
            .field("bytes", &self.content.len())
            .field("replacements", &self.replacements)
            .finish()
    }
}
impl PreparedEdit {
    pub fn state(&self) -> &EditState {
        &self.state
    }
    pub fn content(&self) -> &str {
        &self.content
    }
    pub fn replacements(&self) -> u64 {
        self.replacements
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EditOutput {
    pub host_id: ExecutionHostId,
    pub path: ExecutionPath,
    pub replacements: u64,
    pub bytes_written: u64,
    pub revision: FileRevision,
}
impl EditOutput {
    pub fn observation(&self) -> ObservedFile {
        ObservedFile {
            host_id: self.host_id.clone(),
            path: self.path.clone(),
            revision: self.revision.clone(),
        }
    }
    pub fn to_text(&self) -> String {
        format!(
            "Replaced {} occurrence{} in {} (root {}).",
            self.replacements,
            if self.replacements == 1 { "" } else { "s" },
            serde_json::to_string(&self.path.path).expect("string serialization"),
            serde_json::to_string(self.path.root_id.as_str()).expect("string serialization")
        )
    }
}

pub struct EditTool<'a> {
    runtime: &'a dyn ExecutionRuntime,
    cwd: ExecutionPath,
    config: EditConfig,
}
impl<'a> EditTool<'a> {
    pub fn new(
        runtime: &'a dyn ExecutionRuntime,
        cwd: ExecutionPath,
        config: EditConfig,
    ) -> ExecutionResult<Self> {
        tool_filesystem::validate_cwd(runtime.descriptor(), &cwd)?;
        if config.max_file_bytes == 0 || config.chunk_bytes == 0 {
            return Err(error(
                Code::InvalidRequest,
                "edit size and chunk limits must be positive",
            ));
        }
        Ok(Self {
            runtime,
            cwd,
            config,
        })
    }
    pub fn resolve_path(&self, file_path: &str) -> ExecutionResult<ExecutionPath> {
        tool_filesystem::resolve_path(self.runtime.descriptor(), &self.cwd, file_path)
    }

    /// Read and match only. No filesystem mutation occurs during preparation.
    pub async fn prepare(
        &self,
        context: &OperationContext,
        input: EditInput,
        state: EditState,
    ) -> ExecutionResult<PreparedEdit> {
        context.checkpoint()?;
        if input.old_string.is_empty() {
            return Err(error(
                Code::InvalidRequest,
                "old_string must not be empty; use write to create or replace a file",
            ));
        }
        if input.old_string == input.new_string {
            return Err(error(
                Code::InvalidRequest,
                "old_string and new_string are identical; no change to apply",
            ));
        }
        if input.old_string.len() as u64 > self.config.max_file_bytes
            || input.new_string.len() as u64 > self.config.max_file_bytes
        {
            return Err(error(
                Code::ResourceExhausted,
                "edit text exceeds the configured file size limit",
            ));
        }
        let path = self.resolve_path(&input.file_path)?;
        self.validate_observation(&state.observed)?;
        if path != state.observed.path {
            return Err(error(
                Code::InvalidRequest,
                "observed revision belongs to another path; read this file first",
            ));
        }
        let source = content::read(self.runtime, context, &state.observed, &self.config).await?;
        let max_bytes = self.config.max_file_bytes.min(
            self.runtime
                .descriptor()
                .limits
                .max_write_bytes
                .unwrap_or(u64::MAX),
        );
        let (content, replacements) = matching::replace(
            context,
            &source,
            &input.old_string,
            &input.new_string,
            input.replace_all,
            max_bytes,
        )?;
        context.checkpoint()?;
        Ok(PreparedEdit {
            state,
            content,
            replacements,
        })
    }

    /// Apply or deliberately replay a previously persisted plan. The plan's
    /// resolved path is used even if this tool was recreated with a different cwd.
    pub async fn apply(
        &self,
        context: &OperationContext,
        prepared: &PreparedEdit,
    ) -> ExecutionResult<EditOutput> {
        context.checkpoint()?;
        self.validate_observation(&prepared.state.observed)?;
        if prepared.replacements == 0 {
            return Err(error(
                Code::InvalidRequest,
                "prepared edit must contain at least one replacement",
            ));
        }
        let target = &prepared.state.observed.path;
        let writer = WriteTool::new(
            self.runtime,
            ExecutionPath::root(target.root_id.clone()),
            WriteConfig {
                max_content_bytes: self.config.max_file_bytes,
            },
        )?;
        let result = writer
            .execute(
                context,
                WriteInput {
                    file_path: target.path.clone(),
                    content: prepared.content.clone(),
                },
                WriteState {
                    operation_id: prepared.state.operation_id.clone(),
                    observed: Some(prepared.state.observed.clone()),
                },
            )
            .await?;
        Ok(EditOutput {
            host_id: result.host_id,
            path: result.path,
            replacements: prepared.replacements,
            bytes_written: result.bytes_written,
            revision: result.revision,
        })
    }

    fn validate_observation(&self, observed: &ObservedFile) -> ExecutionResult<()> {
        if observed.host_id != self.runtime.descriptor().host_id {
            return Err(error(
                Code::InvalidRequest,
                "observed revision belongs to another host; read this file first",
            ));
        }
        tool_filesystem::validate_cwd(self.runtime.descriptor(), &observed.path)?;
        if observed.path.path == "." {
            return Err(error(Code::IsDirectory, "cannot edit an execution root"));
        }
        if self
            .runtime
            .descriptor()
            .roots
            .iter()
            .any(|root| root.id == observed.path.root_id && root.read_only)
        {
            return Err(error(
                Code::ReadOnlyRoot,
                "the selected execution root is read-only",
            ));
        }
        Ok(())
    }
}

fn error(code: Code, message: impl Into<String>) -> ExecutionError {
    ExecutionError::new(code, message).with_detail("source", "tool-edit")
}
