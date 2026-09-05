//! Shared text-file reading through an injected execution runtime.
#![doc = include_str!("../README.md")]

mod reader;

use execution_core::{
    ExecutionError, ExecutionErrorCode, ExecutionPath, ExecutionResult, ExecutionRuntime,
    FileRevision, OperationContext,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const NAME: &str = "read";
const FOOTER_RESERVE: usize = 256;

/// Canonical model-facing arguments. Host selection is supplied by the caller.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadInput {
    pub file_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

/// Harness-controlled limits, shared by direct calls and code mode.
#[derive(Clone, Debug)]
pub struct ReadConfig {
    pub max_lines: u64,
    /// Maximum UTF-8 bytes of the rendered result, including numbering/footer.
    pub max_output_bytes: usize,
    /// Maximum bytes scanned, including lines before the requested offset.
    pub max_scan_bytes: u64,
    /// Also capped by the host's advertised max_read_bytes.
    pub chunk_bytes: u64,
}

impl Default for ReadConfig {
    fn default() -> Self {
        Self {
            max_lines: 2000,
            max_output_bytes: 50 * 1024,
            max_scan_bytes: 64 * 1024 * 1024,
            chunk_bytes: 64 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Truncation {
    LineLimit,
    ByteLimit,
}

/// Content preserves file bytes as UTF-8, including line endings. Numbering and
/// continuation instructions are added only by `to_text`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReadOutput {
    pub path: ExecutionPath,
    pub revision: FileRevision,
    pub content: String,
    pub start_line: u64,
    pub end_line: Option<u64>,
    pub eof: bool,
    pub truncation: Option<Truncation>,
    pub next_offset: Option<u64>,
}

impl ReadOutput {
    /// Claude-style numbered content for a model-facing text result.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for (index, line) in self.content.split_inclusive('\n').enumerate() {
            text.push_str(&format!("{:>6}\t{}", self.start_line + index as u64, line));
        }
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        if let Some(end) = self.end_line {
            if let Some(next) = self.next_offset {
                let reason = match self.truncation {
                    Some(Truncation::ByteLimit) => "output byte limit",
                    _ => "line limit",
                };
                text.push_str(&format!(
                    "[Showing lines {}-{}; {} reached. Use offset={} to continue.]\n",
                    self.start_line, end, reason, next
                ));
            } else {
                text.push_str(&format!("[End of file at line {}.]\n", end));
            }
        } else {
            text.push_str("[File is empty.]\n");
        }
        text
    }
}

/// Reusable tool bound to a selected host and an execution-root-relative cwd.
/// `ExecutionClient::connect_host` supplies a compatible `GatewayHostRuntime`.
pub struct ReadTool<'a> {
    runtime: &'a dyn ExecutionRuntime,
    cwd: ExecutionPath,
    config: ReadConfig,
}

impl<'a> ReadTool<'a> {
    pub fn new(
        runtime: &'a dyn ExecutionRuntime,
        cwd: ExecutionPath,
        config: ReadConfig,
    ) -> ExecutionResult<Self> {
        tool_filesystem::validate_cwd(runtime.descriptor(), &cwd)?;
        if config.max_lines == 0
            || config.max_output_bytes <= FOOTER_RESERVE
            || config.max_scan_bytes == 0
            || config.chunk_bytes == 0
        {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "read limits must be positive and max_output_bytes must exceed 256",
            ));
        }
        Ok(Self {
            runtime,
            cwd,
            config,
        })
    }

    pub fn input_schema(&self) -> Value {
        input_schema(&self.config)
    }
    pub fn description(&self) -> String {
        description(&self.config)
    }

    pub async fn execute(
        &self,
        context: &OperationContext,
        input: ReadInput,
    ) -> ExecutionResult<ReadOutput> {
        context.checkpoint()?;
        let offset = input.offset.unwrap_or(1);
        let limit = input.limit.unwrap_or(self.config.max_lines);
        if offset == 0 || limit == 0 {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                "offset and limit must be positive integers",
            ));
        }
        let path =
            tool_filesystem::resolve_path(self.runtime.descriptor(), &self.cwd, &input.file_path)?;
        reader::read(
            self.runtime,
            context,
            path,
            offset,
            limit.min(self.config.max_lines),
            &self.config,
        )
        .await
    }
}

fn error(code: ExecutionErrorCode, message: impl Into<String>) -> ExecutionError {
    ExecutionError::new(code, message).with_detail("source", "tool-read")
}
pub fn input_schema(config: &ReadConfig) -> Value {
    json!({"type":"object","properties":{
            "file_path":{"type":"string","minLength":1,"description":"Path to the file to read. Relative paths resolve against the current working directory; absolute paths refer to the execution host."},
            "offset":{"type":"integer","minimum":1,"description":"The line number to start reading from, starting at 1. Defaults to 1."},
            "limit":{"type":"integer","minimum":1,"description":format!("The maximum number of lines to return. Defaults to {}. Output is also subject to the tool's size limit.", config.max_lines)}
        },"required":["file_path"],"additionalProperties":false})
}

pub fn description(config: &ReadConfig) -> String {
    format!(
        "Read a UTF-8 text file from the execution host. Returns numbered lines, up to {} lines and {} bytes including formatting. Use offset and limit to read a line window; follow the returned continuation offset when output is shortened. Empty files return an empty-file notice. Directories, binary files, images, PDFs, and notebooks rendered as cells are not supported; use bash for directory listings. Paths must resolve inside a registered execution root. Parent (..) path segments are not supported; use an absolute path instead. File content preserves indentation and line endings; do not include line-number prefixes when editing.",
        config.max_lines, config.max_output_bytes
    )
}
