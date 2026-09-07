use execution_core::*;
use llm_contracts::{ContentPart, JsonObject, TextContent, ToolArguments, ToolDefinition};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tool_apply_patch::{
    ApplyPatchConfig, ApplyPatchState, ApplyPatchTool, PatchProgress, PreparedPatch, RunningPatch,
};
use tool_unified_exec::{
    ExecCommandIds, ExecCommandInput, ExecSession, PreparedExecCommand, PreparedWriteStdin,
    RunningExecCommand, RunningWriteStdin, UnifiedExecConfig, UnifiedExecTool, WriteStdinIds,
    WriteStdinInput,
};
use tool_view_image::{
    ImageDelivery, ViewImageConfig, ViewImageInput, ViewImageOptions, ViewImageTool,
};

/// Execution and image capability choices pinned with each run's tool definitions.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolPolicy {
    pub patch: ApplyPatchConfig,
    pub max_output_bytes: usize,
    pub max_output_tokens: usize,
    pub max_image_bytes: u64,
    pub supports_images: bool,
    pub original_detail: bool,
}
impl Default for ToolPolicy {
    fn default() -> Self {
        Self {
            patch: ApplyPatchConfig {
                max_patch_bytes: 16 * 1024,
                max_file_bytes: 64 * 1024,
                chunk_bytes: 64 * 1024,
                max_operations: 64,
                ..Default::default()
            },
            max_output_bytes: 64 * 1024,
            max_output_tokens: 10_000,
            max_image_bytes: 20 * 1024 * 1024,
            supports_images: true,
            original_detail: true,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Plan {
    Image {
        input: ViewImageInput,
    },
    PatchPrepared {
        prepared: Box<PreparedPatch>,
    },
    Patch {
        running: Box<RunningPatch>,
    },
    ExecPrepared {
        prepared: Box<PreparedExecCommand>,
        alias: i32,
    },
    Exec {
        running: Box<RunningExecCommand>,
        alias: i32,
    },
    StdinPrepared {
        prepared: Box<PreparedWriteStdin>,
    },
    Stdin {
        running: Box<RunningWriteStdin>,
    },
}
impl Plan {
    pub fn session(&self) -> Option<&ExecSession> {
        match self {
            Self::Exec { running, .. } => running.session(),
            Self::StdinPrepared { prepared } => Some(prepared.session()),
            Self::Stdin { running } => Some(running.session()),
            _ => None,
        }
    }
}

pub(crate) struct Output {
    pub content: Vec<ContentPart>,
    pub error: bool,
    pub details: Option<JsonObject>,
    pub session_update: Option<Box<(i32, Option<ExecSession>)>>,
}
impl Output {
    pub fn text(text: String, error: bool) -> Self {
        Self {
            content: vec![ContentPart::Text(TextContent {
                content: text,
                metadata: None,
            })],
            error,
            details: None,
            session_update: None,
        }
    }
    pub fn error(error: ExecutionError) -> Self {
        let mut output = Self::text(error.to_string(), true);
        output.details = Some(json!(error).as_object().unwrap().clone());
        output
    }
    fn execution(result: tool_unified_exec::UnifiedExecCallResult, alias: i32) -> Self {
        let mut output = Self::text(result.output.to_text(), false);
        output.details = Some(
            json!({"exec_session_id":alias})
                .as_object()
                .unwrap()
                .clone(),
        );
        output.session_update = Some(Box::new((alias, result.session)));
        output
    }
}
pub(crate) enum Progress {
    Pending(Box<Plan>),
    Done(Output),
}

pub(crate) struct Tools<'a> {
    pub host: &'a dyn ExecutionRuntime,
    pub cwd: ExecutionPath,
    pub policy: &'a ToolPolicy,
    pub delivery: ImageDelivery,
}
impl Tools<'_> {
    pub fn exec(&self) -> ExecutionResult<UnifiedExecTool<'_>> {
        UnifiedExecTool::new(
            self.host,
            self.cwd.clone(),
            UnifiedExecConfig {
                patch_config: self.policy.patch.clone(),
                max_output_bytes: self.policy.max_output_bytes,
                max_output_tokens: self.policy.max_output_tokens,
                default_max_output_tokens: self.policy.max_output_tokens,
                ..Default::default()
            },
        )
    }
    fn patch(&self) -> ExecutionResult<ApplyPatchTool<'_>> {
        ApplyPatchTool::new(self.host, self.cwd.clone(), self.policy.patch.clone())
    }
    fn image(&self) -> ExecutionResult<ViewImageTool<'_>> {
        ViewImageTool::new(
            self.host,
            self.cwd.clone(),
            ViewImageConfig {
                max_image_bytes: self.policy.max_image_bytes,
                delivery: self.delivery.clone(),
                options: ViewImageOptions {
                    supports_images: self.policy.supports_images,
                    can_request_original_detail: self.policy.original_detail,
                    unified_image_budget: false,
                },
                ..Default::default()
            },
        )
    }
    pub fn definitions(&self) -> ExecutionResult<Vec<ToolDefinition>> {
        let mut definitions = self.exec()?.definitions();
        definitions.push(self.patch()?.definition());
        definitions.push(self.image()?.definition());
        Ok(definitions)
    }
    pub async fn prepare(
        &self,
        context: &OperationContext,
        name: &str,
        arguments: &ToolArguments,
        alias: Option<i32>,
        session: Option<ExecSession>,
    ) -> ExecutionResult<Plan> {
        match name {
            "exec_command" => {
                let input: ExecCommandInput = decode(arguments)?;
                if input.cmd.len() > 32 * 1024 {
                    return Err(invalid("Command exceeds 32 KiB; split the command"));
                }
                let alias = alias.ok_or_else(|| invalid("Missing process alias"))?;
                Ok(Plan::ExecPrepared {
                    prepared: Box::new(self.exec()?.prepare_exec_command(
                        input,
                        ExecCommandIds {
                            session_id: alias,
                            operation_id: OperationId::generate(),
                            execution_id: ExecutionId::generate(),
                            terminate_operation_id: OperationId::generate(),
                        },
                    )?),
                    alias,
                })
            }
            "write_stdin" => {
                let input: WriteStdinInput = decode(arguments)?;
                if input.chars.len() > 8 * 1024 {
                    return Err(invalid("Input exceeds 8 KiB; split the input"));
                }
                let session = session.ok_or_else(|| {
                    invalid("Unknown or expired process session; sessions do not transfer to forks")
                })?;
                Ok(Plan::StdinPrepared {
                    prepared: Box::new(self.exec()?.prepare_write_stdin(
                        input,
                        session,
                        WriteStdinIds {
                            write_id: WriteId::generate(),
                            interrupt_operation_id: OperationId::generate(),
                        },
                    )?),
                })
            }
            "apply_patch" => Ok(Plan::PatchPrepared {
                prepared: Box::new(
                    self.patch()?
                        .prepare_arguments(
                            context,
                            arguments,
                            ApplyPatchState {
                                operation_id: OperationId::generate(),
                            },
                        )
                        .await?,
                ),
            }),
            "view_image" => Ok(Plan::Image {
                input: self.image()?.parse_arguments(arguments)?,
            }),
            _ => Err(invalid("Unknown tool")),
        }
    }
    pub async fn execute(
        &self,
        context: &OperationContext,
        plan: Plan,
    ) -> ExecutionResult<Progress> {
        let next = match plan {
            Plan::Image { input } => {
                return Ok(Progress::Done(Output {
                    content: vec![self.image()?.execute_for_model(context, input).await?],
                    error: false,
                    details: None,
                    session_update: None,
                }));
            }
            Plan::PatchPrepared { prepared } => Plan::Patch {
                running: Box::new(self.patch()?.start(&prepared)?),
            },
            Plan::Patch { mut running } => match self.patch()?.step(context, &mut running).await? {
                PatchProgress::CheckpointRequired => Plan::Patch { running },
                PatchProgress::Complete(output) => {
                    return Ok(Progress::Done(Output::text(output.to_text(), false)));
                }
            },
            Plan::ExecPrepared { prepared, alias } => Plan::Exec {
                running: Box::new(self.exec()?.start_exec_command(context, &prepared).await?),
                alias,
            },
            Plan::Exec { mut running, alias } => {
                if running.ready() {
                    return Ok(Progress::Done(Output::execution(
                        self.exec()?.finish_exec_command(&running)?,
                        alias,
                    )));
                }
                self.exec()?
                    .poll_exec_command(context, &mut running)
                    .await?;
                Plan::Exec { running, alias }
            }
            Plan::StdinPrepared { prepared } => Plan::Stdin {
                running: Box::new(self.exec()?.start_write_stdin(context, &prepared).await?),
            },
            Plan::Stdin { mut running } => {
                if running.ready() {
                    return Ok(Progress::Done(Output::execution(
                        self.exec()?.finish_write_stdin(&running)?,
                        running.session().session_id,
                    )));
                }
                self.exec()?.poll_write_stdin(context, &mut running).await?;
                Plan::Stdin { running }
            }
        };
        // A pending patch mutation has only been staged: refuse an oversized
        // checkpoint before dispatching it, with any prior mutation prefix visible.
        if serde_json::to_vec(&next).unwrap().len() > 512 * 1024 {
            return Err(ExecutionError::new(
                ExecutionErrorCode::ResourceExhausted,
                "Tool checkpoint exceeds 512 KiB; prior patch steps may have applied. Inspect files and split the operation.",
            ));
        }
        Ok(Progress::Pending(Box::new(next)))
    }
}

pub(crate) fn decode<T: DeserializeOwned>(arguments: &ToolArguments) -> ExecutionResult<T> {
    let value = match arguments {
        ToolArguments::Object(value) => Value::Object(value.clone()),
        ToolArguments::String(raw) => {
            serde_json::from_str(raw).map_err(|_| invalid("Expected JSON function arguments"))?
        }
    };
    serde_json::from_value(value).map_err(|e| invalid(&e.to_string()))
}
pub(crate) fn uncertain(error: &ExecutionError) -> bool {
    matches!(
        error.code,
        ExecutionErrorCode::Unavailable
            | ExecutionErrorCode::DeadlineExceeded
            | ExecutionErrorCode::Cancelled
            | ExecutionErrorCode::Internal
    )
}
pub(crate) fn invalid(message: &str) -> ExecutionError {
    ExecutionError::new(ExecutionErrorCode::InvalidRequest, message)
}
