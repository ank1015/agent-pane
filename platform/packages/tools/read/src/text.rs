//! Line-window reading shared by filesystem streams and caller-owned UTF-8 source.
use crate::{FOOTER_RESERVE, ReadConfig, Truncation, error};
use execution_core::{ExecutionErrorCode as Code, ExecutionResult};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct TextReadOutput {
    pub content: String,
    pub start_line: u64,
    pub end_line: Option<u64>,
    pub eof: bool,
    pub truncation: Option<Truncation>,
    pub next_offset: Option<u64>,
}

/// Read immutable source without inventing an execution host or filesystem.
pub fn read_text(
    source: &str,
    offset: Option<u64>,
    limit: Option<u64>,
    config: &ReadConfig,
) -> ExecutionResult<TextReadOutput> {
    let mut reader = LineReader::new(
        offset.unwrap_or(1),
        limit.unwrap_or(config.max_lines),
        config,
    )?;
    for (index, byte) in source.bytes().enumerate() {
        if index as u64 >= config.max_scan_bytes {
            return Err(error(Code::ResourceExhausted, "read scan limit reached"));
        }
        if reader.feed(byte)? {
            return Ok(reader.output);
        }
    }
    reader.finish(source.is_empty())
}

pub(crate) struct LineReader {
    pub output: TextReadOutput,
    limit: u64,
    budget: usize,
    number: u64,
    line: Vec<u8>,
    rendered_bytes: usize,
}
impl LineReader {
    pub fn new(start: u64, limit: u64, config: &ReadConfig) -> ExecutionResult<Self> {
        if start == 0
            || limit == 0
            || config.max_lines == 0
            || config.max_output_bytes <= FOOTER_RESERVE
        {
            return Err(error(
                Code::InvalidRequest,
                "offset, limit and read budgets must be positive",
            ));
        }
        Ok(Self {
            output: TextReadOutput {
                content: String::new(),
                start_line: start,
                end_line: None,
                eof: false,
                truncation: None,
                next_offset: None,
            },
            limit: limit.min(config.max_lines),
            budget: config.max_output_bytes - FOOTER_RESERVE,
            number: 1,
            line: Vec::new(),
            rendered_bytes: 0,
        })
    }
    pub fn feed(&mut self, byte: u8) -> ExecutionResult<bool> {
        if self
            .output
            .end_line
            .is_some_and(|last| last - self.output.start_line + 1 >= self.limit)
        {
            self.output.truncation = Some(Truncation::LineLimit);
            self.output.next_offset = Some(self.number);
            return Ok(true);
        }
        if byte == 0 {
            return Err(error(
                Code::Unsupported,
                "file contains binary data (NUL); read supports UTF-8 text only",
            ));
        }
        if self.number >= self.output.start_line {
            self.line.push(byte);
            let prefix = self.number.to_string().len().max(6) + 1;
            if self.rendered_bytes + prefix + self.line.len() + usize::from(byte != b'\n')
                > self.budget
            {
                if self.output.end_line.is_none() {
                    return Err(error(
                        Code::ResourceExhausted,
                        format!("line {} exceeds the output limit", self.number),
                    ));
                }
                self.output.truncation = Some(Truncation::ByteLimit);
                self.output.next_offset = Some(self.number);
                return Ok(true);
            }
            if byte == b'\n' {
                self.append()?;
            }
        }
        if byte == b'\n' {
            self.number = self
                .number
                .checked_add(1)
                .ok_or_else(|| error(Code::ResourceExhausted, "line number overflow"))?;
        }
        Ok(false)
    }
    fn append(&mut self) -> ExecutionResult<()> {
        let text = std::str::from_utf8(&self.line).map_err(|_| {
            error(
                Code::Unsupported,
                "requested content is not valid UTF-8 text",
            )
        })?;
        self.rendered_bytes += self.number.to_string().len().max(6)
            + 1
            + self.line.len()
            + usize::from(!self.line.ends_with(b"\n"));
        self.output.content.push_str(text);
        self.output.end_line = Some(self.number);
        self.line.clear();
        Ok(())
    }
    pub fn finish(mut self, empty: bool) -> ExecutionResult<TextReadOutput> {
        if !self.line.is_empty() {
            self.append()?;
        }
        if self.output.end_line.is_none() && !(empty && self.output.start_line == 1) {
            return Err(error(
                Code::InvalidRequest,
                format!(
                    "offset {} is beyond the end of the file",
                    self.output.start_line
                ),
            ));
        }
        self.output.eof = true;
        Ok(self.output)
    }
}
