use execution_core::*;
use llm_contracts::{FunctionTool, ToolArguments, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tool_bash_minimal::{BashConfig, BashIds, BashInput, BashTool, PreparedBash, RunningBash};
use tool_edit::{EditConfig, EditInput, EditState, EditTool, PreparedEdit};
use tool_read::{ReadConfig, ReadInput, ReadTool};
use tool_write::{ObservedFile, WriteConfig, WriteInput, WriteState, WriteTool};

// Worst-case JSON escaping must fit together with the checkpoint in a 1 MiB commit.
pub const FILE_LIMIT: u64 = 64 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Plan {
    Read {
        input: ReadInput,
    },
    Write {
        input: WriteInput,
        state: WriteState,
        generation: SupervisorGenerationId,
    },
    Edit {
        prepared: PreparedEdit,
        generation: SupervisorGenerationId,
    },
    Bash {
        prepared: Box<PreparedBash>,
        running: Option<Box<RunningBash>>,
    },
}

pub struct Output {
    pub text: String,
    pub error: bool,
    pub observation: Option<ObservedFile>,
    pub consume: Option<ExecutionPath>,
}
impl Output {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            text: message.into(),
            error: true,
            observation: None,
            consume: None,
        }
    }
    fn success(text: String) -> Self {
        Self {
            text,
            error: false,
            observation: None,
            consume: None,
        }
    }
}

pub enum Progress {
    Done(Output),
    Pending(Box<Plan>),
}

pub struct Tools<'a> {
    pub host: &'a dyn ExecutionRuntime,
    pub cwd: ExecutionPath,
}

impl Tools<'_> {
    fn read(&self) -> ExecutionResult<ReadTool<'_>> {
        ReadTool::new(self.host, self.cwd.clone(), ReadConfig::default())
    }
    fn write(&self) -> ExecutionResult<WriteTool<'_>> {
        WriteTool::new(
            self.host,
            self.cwd.clone(),
            WriteConfig {
                max_content_bytes: FILE_LIMIT,
            },
        )
    }
    fn edit(&self) -> ExecutionResult<EditTool<'_>> {
        EditTool::new(
            self.host,
            self.cwd.clone(),
            EditConfig {
                max_file_bytes: FILE_LIMIT,
                chunk_bytes: FILE_LIMIT,
            },
        )
    }
    pub fn bash(&self) -> ExecutionResult<BashTool<'_>> {
        BashTool::new(self.host, self.cwd.clone(), BashConfig::default())
    }

    pub fn definitions(&self) -> ExecutionResult<Vec<ToolDefinition>> {
        definitions()
    }

    pub async fn prepare(
        &self,
        ctx: &OperationContext,
        name: &str,
        arguments: &ToolArguments,
        observations: &[ObservedFile],
    ) -> ExecutionResult<Plan> {
        let value = match arguments {
            ToolArguments::Object(object) => Value::Object(object.clone()),
            ToolArguments::String(raw) => serde_json::from_str(raw)
                .map_err(|_| invalid("Tool arguments must be a JSON object"))?,
        };
        let schema = match name {
            "read" => self.read()?.input_schema(),
            "write" => tool_write::input_schema(),
            "edit" => tool_edit::input_schema(),
            "bash" => tool_bash_minimal::input_schema(),
            _ => {
                return Err(invalid(
                    "Unknown tool. Available tools: read, write, edit, bash",
                ));
            }
        };
        if !jsonschema::validator_for(&schema)
            .map_err(|_| invalid("Invalid tool schema"))?
            .is_valid(&value)
        {
            return Err(invalid("Arguments do not match the tool schema"));
        }
        let generation = self.host.descriptor().supervisor_generation_id.clone();
        let find = |path: &ExecutionPath| {
            observations
                .iter()
                .find(|old| old.host_id == self.host.descriptor().host_id && old.path == *path)
                .cloned()
        };
        match name {
            "read" => Ok(Plan::Read {
                input: decode(value)?,
            }),
            "write" => {
                let input: WriteInput = decode(value)?;
                if input.content.len() as u64 > FILE_LIMIT {
                    return Err(invalid(
                        "Write content exceeds the harness's 64 KiB durable-write limit",
                    ));
                }
                let path = self.write()?.resolve_path(&input.file_path)?;
                Ok(Plan::Write {
                    input,
                    state: WriteState {
                        operation_id: OperationId::generate(),
                        observed: find(&path),
                    },
                    generation,
                })
            }
            "edit" => {
                let input: EditInput = decode(value)?;
                let path = self.edit()?.resolve_path(&input.file_path)?;
                let observed = find(&path).ok_or_else(|| invalid("Read this file before editing it; each successful edit consumes the previous read"))?;
                let prepared = self
                    .edit()?
                    .prepare(
                        ctx,
                        input,
                        EditState {
                            operation_id: OperationId::generate(),
                            observed,
                        },
                    )
                    .await?;
                Ok(Plan::Edit {
                    prepared,
                    generation,
                })
            }
            "bash" => {
                let input: BashInput = decode(value)?;
                let prepared = self.bash()?.prepare(
                    input,
                    BashIds {
                        operation_id: OperationId::generate(),
                        execution_id: ExecutionId::generate(),
                        terminate_operation_id: OperationId::generate(),
                    },
                )?;
                Ok(Plan::Bash {
                    prepared: Box::new(prepared),
                    running: None,
                })
            }
            _ => unreachable!(),
        }
    }

    /// Only invoked after Plan is durably committed. Ambiguous mutation errors
    /// are propagated, leaving that exact plan available to the next activation.
    pub async fn execute(&self, ctx: &OperationContext, plan: Plan) -> ExecutionResult<Progress> {
        let result = match plan {
            Plan::Read { input } => {
                let result = self.read()?.execute(ctx, input).await?;
                let mut output = Output::success(result.to_text());
                output.observation = Some(ObservedFile {
                    host_id: self.host.descriptor().host_id.clone(),
                    path: result.path,
                    revision: result.revision,
                });
                output
            }
            Plan::Write {
                input,
                state,
                generation,
            } => {
                self.generation(&generation)?;
                let result = self.write()?.execute(ctx, input, state).await?;
                let mut output = Output::success(result.to_text());
                output.consume = Some(result.path);
                output
            }
            Plan::Edit {
                prepared,
                generation,
            } => {
                self.generation(&generation)?;
                let result = self.edit()?.apply(ctx, &prepared).await?;
                let mut output = Output::success(result.to_text());
                output.consume = Some(result.path);
                output
            }
            Plan::Bash { prepared, running } => {
                let bash = self.bash()?;
                let Some(mut running) = running else {
                    let running = bash.start(ctx, &prepared).await?;
                    return Ok(Progress::Pending(Box::new(Plan::Bash {
                        prepared,
                        running: Some(Box::new(running)),
                    })));
                };
                let page = bash.poll(ctx, &mut running).await?;
                if !page.complete {
                    return Ok(Progress::Pending(Box::new(Plan::Bash {
                        prepared,
                        running: Some(running),
                    })));
                }
                let result = running.output();
                Output {
                    text: result.to_text(),
                    error: result.is_error(),
                    observation: None,
                    consume: None,
                }
            }
        };
        Ok(Progress::Done(result))
    }

    fn generation(&self, saved: &SupervisorGenerationId) -> ExecutionResult<()> {
        if *saved != self.host.descriptor().supervisor_generation_id {
            return Err(ExecutionError::new(
                ExecutionErrorCode::ExecutionLost,
                "Supervisor restarted. The pending mutation's outcome must be checked before retrying.",
            ));
        }
        Ok(())
    }
}

pub fn uncertain(error: &ExecutionError) -> bool {
    matches!(
        error.code,
        ExecutionErrorCode::Unavailable
            | ExecutionErrorCode::DeadlineExceeded
            | ExecutionErrorCode::Cancelled
            | ExecutionErrorCode::Internal
    )
}
fn invalid(message: &str) -> ExecutionError {
    ExecutionError::new(ExecutionErrorCode::InvalidRequest, message)
}
fn decode<T: serde::de::DeserializeOwned>(value: Value) -> ExecutionResult<T> {
    serde_json::from_value(value).map_err(|e| invalid(&e.to_string()))
}
fn definition(name: &str, description: String, schema: Value) -> ToolDefinition {
    ToolDefinition::Function(FunctionTool {
        name: name.into(),
        description,
        parameters: schema.as_object().unwrap().clone(),
        output_schema: None,
        strict: Some(false),
    })
}
pub fn definitions() -> ExecutionResult<Vec<ToolDefinition>> {
    Ok(vec![
        definition(
            "read",
            tool_read::description(&ReadConfig::default()),
            tool_read::input_schema(&ReadConfig::default()),
        ),
        definition(
            "write",
            format!(
                "{} This harness limits content to {FILE_LIMIT} UTF-8 bytes. Read again before every overwrite, even after a successful mutation.",
                tool_write::DESCRIPTION
            ),
            tool_write::input_schema(),
        ),
        definition(
            "edit",
            format!(
                "{} This harness supports files/results up to {FILE_LIMIT} UTF-8 bytes. Read again before every edit, even after a successful mutation.",
                tool_edit::DESCRIPTION
            ),
            tool_edit::input_schema(),
        ),
        definition(
            "bash",
            tool_bash_minimal::DESCRIPTION.into(),
            tool_bash_minimal::input_schema(),
        ),
    ])
}
