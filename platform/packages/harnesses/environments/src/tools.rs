//! Model-facing environment tools. Every effect has a persisted plan; filesystem
//! adapters reuse the same prepared operations as the basic coding harness.
use cc_harness_support::filesystem as fs;
use execution_api::*;
use execution_client::{ExecutionClient, HostFilter, SnapshotFilter};
use execution_core::*;
pub(crate) use fs::uncertain;
use llm_contracts::{ContentPart, FunctionTool, ToolArguments, ToolDefinition};
use platform_runtime_client::{
    Command, RequestKey, RunClient,
    types::{ConflictCode, CreateEnvironment},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use tool_write::ObservedFile;
use uuid::Uuid;

#[derive(Clone)]
pub struct WebTools {
    pub search: tool_firecrawl_search::FirecrawlSearchToolContext,
    pub scrape: tool_firecrawl_scrape::FirecrawlScrapeToolContext,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Plan {
    Filesystem {
        host_id: ExecutionHostId,
        cwd: ExecutionPath,
        operation: Box<fs::Plan>,
    },
    ReadOnly {
        name: String,
        arguments: Value,
    },
    CreateSandbox {
        key: String,
        request: CreateExecutionHostRequest,
        host: Option<Uuid>,
        started_at: i64,
    },
    Snapshot {
        key: String,
        host_id: Uuid,
        request: CreateSnapshotRequest,
        snapshot: Option<Uuid>,
        started_at: i64,
    },
    CreateEnvironment {
        key: String,
        request: CreateEnvironment,
    },
}

pub(crate) struct Output {
    pub text: String,
    pub error: bool,
    pub observation: Option<ObservedFile>,
    pub consume: Option<(ExecutionHostId, ExecutionPath)>,
    pub details: Option<Value>,
}
impl Output {
    pub fn error(text: impl Into<String>) -> Self {
        let mut text = text.into();
        if text.len() > 16 * 1024 {
            let mut end = 16 * 1024;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str(" [truncated]");
        }
        Self {
            error: true,
            ..Self::text(text)
        }
    }
    fn text(text: String) -> Self {
        Self {
            text,
            error: false,
            observation: None,
            consume: None,
            details: None,
        }
    }
    fn json(value: Value) -> ExecutionResult<Self> {
        let text =
            serde_json::to_string(&value).map_err(|_| invalid("Cannot encode tool result"))?;
        if text.len() > 256 * 1024 {
            return Err(invalid("Tool result exceeds the 256 KiB limit"));
        }
        Ok(Self::text(text))
    }
}
pub(crate) enum Progress {
    Done(Box<Output>),
    Pending(Box<Plan>),
}

impl Progress {
    fn done(output: Output) -> Self {
        Self::Done(Box::new(output))
    }
}

pub(crate) struct Tools<'a> {
    pub gateway: &'a ExecutionClient,
    pub platform: &'a RunClient,
    pub web: Option<&'a WebTools>,
    pub project_id: Uuid,
    pub session_id: Uuid,
    pub run_id: Uuid,
}

pub fn definitions(web: bool) -> Vec<ToolDefinition> {
    let mut tools = fs::definitions().expect("static filesystem definitions");
    for tool in &mut tools {
        if let ToolDefinition::Function(tool) = tool {
            tool.parameters.get_mut("properties").unwrap().as_object_mut().unwrap().insert("host_id".into(), json!({"type":"string","format":"uuid","description":"Logical execution gateway host ID, returned by discovery or sandbox creation."}));
            tool.parameters
                .get_mut("required")
                .unwrap()
                .as_array_mut()
                .unwrap()
                .push(json!("host_id"));
            tool.description.push_str(" Specify host_id on every call. Relative paths start at the host's sole workspace root. On a host with multiple roots, use an absolute file_path or absolute bash workdir inside a registered root; the harness never guesses the root.");
        }
    }
    let uuid = json!({"type":"string","format":"uuid"});
    let name = json!({"type":"string","minLength":1,"maxLength":128});
    tools.extend([
        definition("list_environments", "List environment records in this run's project. Records point to registered machines or reusable snapshots, not running sandboxes.", json!({}), json!([])),
        definition("list_execution_resources", "List non-deleted registered machines with roots and state, plus E2B account IDs/names/status/default selection. Does not start compute or reveal credentials.", json!({}), json!([])),
        definition("list_snapshots", "List ready, non-deleted logical gateway snapshots. Optionally filter by account or source host. Snapshot restoration inherits its account.", json!({"e2b_account_id":uuid,"source_host_id":uuid}), json!([])),
        definition("create_sandbox", "Create an E2B builder from the gateway's supervisor-enabled base or a logical gateway snapshot. Returns a ready host ID and roots. Snapshot sources inherit their account; base uses the specified account or gateway default. timeout_seconds is sandbox lifetime, not command timeout (gateway default if omitted).", json!({
            "name":name,"timeout_seconds":{"type":"integer","minimum":1,"maximum":86400},
            "source":{"oneOf":[
                {"type":"object","additionalProperties":false,"required":["type"],"properties":{"type":{"const":"base"},"e2b_account_id":uuid}},
                {"type":"object","additionalProperties":false,"required":["type","snapshot_id"],"properties":{"type":{"const":"snapshot"},"snapshot_id":uuid}}
            ]}
        }), json!(["source"])),
        definition("snapshot_sandbox", "Snapshot a ready E2B builder after setup and verification. Returns a ready logical snapshot ID. The gateway may pause the builder; later filesystem calls automatically resume it. Registered machines cannot be snapshotted.", json!({"host_id":uuid,"name":name}), json!(["host_id"])),
    ]);
    let mut create = definition(
        "create_environment",
        "Register a prepared directory in this run's project. Machine requires only machine_id; sandbox requires only a ready snapshot_id. workspace_root is the root ID returned by discovery, path is root-relative (use '.' for the root). Does not provision anything or clean up builders. Verify the directory before registration/snapshotting.",
        json!({"name":name,"type":{"enum":["machine","sandbox"]},"machine_id":uuid,"snapshot_id":uuid,"workspace_root":{"type":"string","minLength":1,"maxLength":128},"path":{"type":"string","minLength":1,"maxLength":4096}}),
        json!(["name", "type", "workspace_root", "path"]),
    );
    if let ToolDefinition::Function(tool) = &mut create {
        tool.parameters.insert("oneOf".into(), json!([
            {"properties":{"type":{"const":"machine"}},"required":["machine_id"],"not":{"required":["snapshot_id"]}},
            {"properties":{"type":{"const":"sandbox"}},"required":["snapshot_id"],"not":{"required":["machine_id"]}}
        ]));
    }
    tools.push(create);
    if web {
        tools.extend([
            tool_firecrawl_search::definition(),
            tool_firecrawl_scrape::definition(),
        ]);
    }
    tools
}

fn definition(name: &str, description: &str, properties: Value, required: Value) -> ToolDefinition {
    ToolDefinition::Function(FunctionTool { name: name.into(), description: description.into(), parameters: json!({"type":"object","additionalProperties":false,"properties":properties,"required":required}).as_object().unwrap().clone(), output_schema: None, strict: Some(false) })
}
fn invalid(message: impl Into<String>) -> ExecutionError {
    ExecutionError::new(ExecutionErrorCode::InvalidRequest, message)
}
fn decode<T: serde::de::DeserializeOwned>(value: Value) -> ExecutionResult<T> {
    serde_json::from_value(value).map_err(|e| invalid(e.to_string()))
}

/// Root selection is deterministic. Absolute paths may select any registered
/// root using the shared remote path resolver, never the worker's native paths.
pub(crate) fn target_cwd(
    host: &ExecutionHostDescriptor,
    name: &str,
    args: &Value,
) -> ExecutionResult<ExecutionPath> {
    let first = host
        .roots
        .first()
        .ok_or_else(|| invalid("Host has no registered workspace roots"))?;
    let base = ExecutionPath::root(first.id.clone());
    if host.roots.len() == 1 {
        return Ok(base);
    }
    let field = if name == "bash" {
        "workdir"
    } else {
        "file_path"
    };
    let path = args.get(field).and_then(Value::as_str).ok_or_else(|| {
        invalid(format!(
            "This host has multiple roots. Provide an absolute {field}."
        ))
    })?;
    let absolute = match host.path_convention {
        PathConvention::Unix => path.starts_with('/'),
        PathConvention::Windows => {
            path.starts_with("\\\\")
                || path.starts_with("//")
                || (path.as_bytes().get(1) == Some(&b':')
                    && matches!(path.as_bytes().get(2), Some(b'/' | b'\\')))
        }
    };
    if !absolute {
        return Err(invalid(format!(
            "This host has multiple roots. Provide an absolute {field}."
        )));
    }
    let resolved = tool_filesystem::resolve_path(host, &base, path)?;
    Ok(ExecutionPath::root(resolved.root_id))
}

impl Tools<'_> {
    pub async fn prepare(
        &self,
        ctx: &OperationContext,
        name: &str,
        arguments: &ToolArguments,
        observations: &[ObservedFile],
    ) -> ExecutionResult<Plan> {
        let mut args: Value = match arguments {
            ToolArguments::Object(v) => Value::Object(v.clone()),
            ToolArguments::String(s) => {
                serde_json::from_str(s).map_err(|_| invalid("Tool arguments must be JSON"))?
            }
        };
        let definition = definitions(self.web.is_some())
            .into_iter()
            .find(|d| d.name() == name)
            .ok_or_else(|| invalid("Unknown or disabled tool"))?;
        let ToolDefinition::Function(definition) = definition else {
            unreachable!()
        };
        if !jsonschema::validator_for(&Value::Object(definition.parameters))
            .map_err(|_| invalid("Invalid tool schema"))?
            .is_valid(&args)
        {
            return Err(invalid("Arguments do not match the tool schema"));
        }
        if matches!(name, "read" | "write" | "edit" | "bash") {
            let id: Uuid = decode(args.as_object_mut().unwrap().remove("host_id").unwrap())?;
            let host_id = ExecutionHostId::new(id.to_string())?;
            let host = self.gateway.connect_host(ctx, host_id.clone()).await?;
            let cwd = target_cwd(host.descriptor(), name, &args)?;
            let tools = fs::Tools {
                host: &host,
                cwd: cwd.clone(),
            };
            let operation = tools
                .prepare(
                    ctx,
                    name,
                    &ToolArguments::Object(args.as_object().unwrap().clone()),
                    observations,
                )
                .await?;
            return Ok(Plan::Filesystem {
                host_id,
                cwd,
                operation: Box::new(operation),
            });
        }
        let key = format!("environments:{}:{}", self.run_id, Uuid::new_v4());
        let metadata = json!({"project_id":self.project_id,"session_id":self.session_id,"run_id":self.run_id,"harness":"environments","operation_key":key});
        let started_at = chrono::Utc::now().timestamp_millis();
        match name {
            "create_sandbox" => {
                args["metadata"] = metadata;
                Ok(Plan::CreateSandbox {
                    key,
                    request: decode(args)?,
                    host: None,
                    started_at,
                })
            }
            "snapshot_sandbox" => {
                let host_id = decode(args.as_object_mut().unwrap().remove("host_id").unwrap())?;
                args["metadata"] = metadata;
                Ok(Plan::Snapshot {
                    key,
                    host_id,
                    request: decode(args)?,
                    snapshot: None,
                    started_at,
                })
            }
            "create_environment" => Ok(Plan::CreateEnvironment {
                key,
                request: decode(args)?,
            }),
            _ => Ok(Plan::ReadOnly {
                name: name.into(),
                arguments: args,
            }),
        }
    }

    pub async fn execute(
        &self,
        ctx: &OperationContext,
        mut plan: Plan,
    ) -> ExecutionResult<Progress> {
        match &mut plan {
            Plan::Filesystem {
                host_id,
                cwd,
                operation,
            } => {
                let host = self.gateway.connect_host(ctx, host_id.clone()).await?;
                let tools = fs::Tools {
                    host: &host,
                    cwd: cwd.clone(),
                };
                return match tools.execute(ctx, *operation.clone()).await? {
                    fs::Progress::Pending(next) => {
                        *operation = next;
                        Ok(Progress::Pending(Box::new(plan)))
                    }
                    fs::Progress::Done(result) => Ok(Progress::done(Output {
                        text: result.text,
                        error: result.error,
                        observation: result.observation,
                        consume: result.consume.map(|p| (host_id.clone(), p)),
                        details: None,
                    })),
                };
            }
            Plan::ReadOnly { name, arguments } => {
                return Ok(Progress::done(
                    self.read_only(ctx, name, arguments.clone()).await?,
                ));
            }
            Plan::CreateEnvironment { key, request } => {
                let command = Command::new(
                    RequestKey::new(key.clone()).map_err(platform_error)?,
                    request.clone(),
                );
                let record = self
                    .platform
                    .create_environment(&command)
                    .await
                    .map_err(platform_error)?;
                return Ok(Progress::done(Output::json(json!(record))?));
            }
            Plan::CreateSandbox {
                key,
                request,
                host,
                started_at,
            } => {
                if let Some(id) = *host {
                    let record = self.gateway.get_host(ctx, id).await?;
                    if record.state == ExecutionHostState::Ready
                        && record.desired_state == DesiredHostState::Ready
                    {
                        let connected = self
                            .gateway
                            .connect_host(ctx, ExecutionHostId::new(id.to_string())?)
                            .await?;
                        return Ok(Progress::done(Output::json(
                            json!({"host_id":id,"name":record.name,"state":"ready","e2b_account_id":record.e2b.map(|e| e.e2b_account_id),"roots":connected.descriptor().roots,"operating_system":connected.descriptor().operating_system,"path_convention":connected.descriptor().path_convention}),
                        )?));
                    }
                    if matches!(
                        record.state,
                        ExecutionHostState::Failed
                            | ExecutionHostState::Lost
                            | ExecutionHostState::Deleting
                            | ExecutionHostState::Deleted
                    ) || record.desired_state == DesiredHostState::Deleted
                    {
                        return Ok(Progress::done(Output::error(format!(
                            "Sandbox {id} could not become ready: {:?}, {}",
                            record.state,
                            record.status_message.unwrap_or_default()
                        ))));
                    }
                    if expired(*started_at) {
                        return Ok(Progress::done(Output::error(format!(
                            "Timed out waiting for sandbox {id}; it may still complete. Do not blindly create a duplicate."
                        ))));
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                } else {
                    *host = Some(self.gateway.create_host(ctx, key, request).await?.id);
                }
            }
            Plan::Snapshot {
                key,
                host_id,
                request,
                snapshot,
                started_at,
            } => {
                if let Some(id) = *snapshot {
                    let record = self.gateway.get_snapshot(ctx, id).await?;
                    if record.state == SnapshotState::Ready
                        && record.desired_state == DesiredSnapshotState::Ready
                    {
                        return Ok(Progress::done(Output::json(
                            json!({"snapshot_id":id,"host_id":host_id,"e2b_account_id":record.e2b_account_id,"name":record.name,"state":"ready","note":"Filesystem calls resume the source builder automatically if needed."}),
                        )?));
                    }
                    if matches!(
                        record.state,
                        SnapshotState::Failed | SnapshotState::Deleting | SnapshotState::Deleted
                    ) || record.desired_state == DesiredSnapshotState::Deleted
                    {
                        return Ok(Progress::done(Output::error(format!(
                            "Snapshot {id} failed: {:?}, {}",
                            record.state,
                            record.status_message.unwrap_or_default()
                        ))));
                    }
                    if expired(*started_at) {
                        return Ok(Progress::done(Output::error(format!(
                            "Timed out waiting for snapshot {id}; inspect this ID before retrying."
                        ))));
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                } else {
                    *snapshot = Some(
                        self.gateway
                            .create_snapshot(ctx, *host_id, key, request)
                            .await?
                            .id,
                    );
                }
            }
        }
        Ok(Progress::Pending(Box::new(plan)))
    }

    async fn read_only(
        &self,
        ctx: &OperationContext,
        name: &str,
        args: Value,
    ) -> ExecutionResult<Output> {
        match name {
            "list_environments" => {
                let records = self.platform.environments().await.map_err(platform_error)?;
                Output::json(inventory(
                    records.items.into_iter().map(|r| json!(r)).collect(),
                ))
            }
            "list_execution_resources" => {
                let machines = self
                    .gateway
                    .list_hosts(
                        ctx,
                        &HostFilter {
                            kind: Some(ExecutionHostKind::Registered),
                            ..Default::default()
                        },
                    )
                    .await?;
                let accounts = self.gateway.list_accounts(ctx).await?;
                Output::json(
                    json!({"machines":inventory(machines.into_iter().map(|h| json!({"host_id":h.id,"name":h.name,"state":h.state,"last_seen_at":h.last_seen_at,"roots":h.roots,"operating_system":h.descriptor.map(|d|d.operating_system)})).collect()),"sandbox_accounts":inventory(accounts.into_iter().map(|a| json!({"e2b_account_id":a.id,"name":a.name,"status":a.status,"is_default":a.is_default})).collect())}),
                )
            }
            "list_snapshots" => {
                let e2b_account_id = args
                    .get("e2b_account_id")
                    .cloned()
                    .map(decode)
                    .transpose()?;
                let source_host_id = args
                    .get("source_host_id")
                    .cloned()
                    .map(decode)
                    .transpose()?;
                let records = self
                    .gateway
                    .list_snapshots(
                        ctx,
                        &SnapshotFilter {
                            state: Some(SnapshotState::Ready),
                            e2b_account_id,
                            source_host_id,
                            ..Default::default()
                        },
                    )
                    .await?;
                Output::json(inventory(records.into_iter().filter(|s|s.desired_state == DesiredSnapshotState::Ready).map(|s| json!({"snapshot_id":s.id,"name":s.name,"e2b_account_id":s.e2b_account_id,"source_host_id":s.source_host_id,"state":s.state,"created_at":s.created_at})).collect()))
            }
            "search" => {
                let web = self
                    .web
                    .ok_or_else(|| invalid("Web tools are not configured"))?;
                let output = tool_firecrawl_search::execute(decode(args)?, &web.search)
                    .await
                    .map_err(|e| invalid(format!("{}: {}", e.name(), e.message())))?;
                Ok(web_output(output.content, output.details))
            }
            "scrape" => {
                let web = self
                    .web
                    .ok_or_else(|| invalid("Web tools are not configured"))?;
                let output = tool_firecrawl_scrape::execute(decode(args)?, &web.scrape)
                    .await
                    .map_err(|e| invalid(format!("{}: {}", e.name(), e.message())))?;
                Ok(web_output(output.content, output.details))
            }
            _ => Err(invalid("Unknown tool")),
        }
    }
}

fn web_output(content: Vec<ContentPart>, details: Option<Value>) -> Output {
    let text = content
        .into_iter()
        .filter_map(|p| {
            if let ContentPart::Text(t) = p {
                Some(t.content)
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    Output {
        details,
        ..Output::text(text)
    }
}
fn inventory(mut items: Vec<Value>) -> Value {
    let total = items.len();
    items.truncate(200);
    json!({"items":items,"total":total,"truncated":total>200})
}
fn expired(start: i64) -> bool {
    chrono::Utc::now().timestamp_millis().saturating_sub(start) > 600_000
}
fn platform_error(error: platform_runtime_client::Error) -> ExecutionError {
    let recover = matches!(
        &error,
        platform_runtime_client::Error::Transport { .. }
            | platform_runtime_client::Error::Protocol(_)
            | platform_runtime_client::Error::WorkerOffline
    ) || error.conflict() == Some(ConflictCode::LeaseLost)
        || matches!(&error, platform_runtime_client::Error::Server(e) if e.status >= 500 || matches!(e.status, 401 | 403 | 429));
    let message = match &error {
        platform_runtime_client::Error::Server(e) => e
            .error
            .as_ref()
            .map(|info| info.message.clone())
            .unwrap_or_else(|| error.to_string()),
        _ => error.to_string(),
    };
    ExecutionError::new(
        if recover {
            ExecutionErrorCode::Unavailable
        } else {
            ExecutionErrorCode::InvalidRequest
        },
        message,
    )
}

pub(crate) async fn abort_plan(gateway: &ExecutionClient, plan: &Plan) -> ExecutionResult<()> {
    if let Plan::Filesystem {
        host_id,
        cwd,
        operation,
    } = plan
    {
        if let fs::Plan::Bash { prepared, running } = operation.as_ref() {
            let ctx = OperationContext::with_timeout(Duration::from_secs(15));
            let host = gateway.connect_host(&ctx, host_id.clone()).await?;
            let result = if let Some(running) = running {
                fs::Tools {
                    host: &host,
                    cwd: cwd.clone(),
                }
                .bash()?
                .terminate(&ctx, running)
                .await
            } else {
                host.terminate(
                    &ctx,
                    TerminateExecutionRequest {
                        operation_id: prepared.terminate_operation_id().clone(),
                        execution_id: prepared.request().execution_id.clone(),
                        supervisor_generation_id: prepared.generation().clone(),
                    },
                )
                .await
            };
            if let Err(e) = result {
                if !matches!(
                    e.code,
                    ExecutionErrorCode::ExecutionNotFound | ExecutionErrorCode::ExecutionLost
                ) {
                    return Err(e);
                }
            }
        }
    }
    // Accepted lifecycle requests are durable gateway jobs, not cancellable
    // worker futures. Do not create resources just to discover IDs during abort.
    Ok(())
}

pub(crate) fn resource_summary(plan: &Plan) -> String {
    match plan {
        Plan::CreateSandbox { key, host, .. } => format!(
            "Sandbox host ID: {host:?}; gateway operation key: {key}. Provisioning is not cancelled."
        ),
        Plan::Snapshot {
            key,
            host_id,
            snapshot,
            ..
        } => format!(
            "Source host: {host_id}; snapshot ID: {snapshot:?}; gateway operation key: {key}. Snapshotting is not cancelled."
        ),
        Plan::CreateEnvironment { key, .. } => format!(
            "Environment creation may have committed; Platform operation key: {key}. Inspect project environments."
        ),
        _ => String::new(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn host() -> ExecutionHostDescriptor {
        ExecutionHostDescriptor {
            host_id: ExecutionHostId::generate(),
            supervisor_generation_id: SupervisorGenerationId::generate(),
            operating_system: OperatingSystem::Linux,
            architecture: "test".into(),
            path_convention: PathConvention::Unix,
            roots: vec![
                ExecutionRoot {
                    id: RootId::new("one").unwrap(),
                    name: "One".into(),
                    native_path: "/one".into(),
                    read_only: false,
                },
                ExecutionRoot {
                    id: RootId::new("two").unwrap(),
                    name: "Two".into(),
                    native_path: "/two".into(),
                    read_only: false,
                },
            ],
            features: ExecutionFeatures {
                pty: false,
                process_signals: true,
                file_revisions: true,
            },
            limits: ExecutionLimits::default(),
        }
    }
    #[test]
    fn sole_roots_resolve_but_multiple_roots_are_never_guessed() {
        let mut h = host();
        assert!(target_cwd(&h, "read", &json!({"file_path":"file"})).is_err());
        assert!(target_cwd(&h, "bash", &json!({"command":"pwd"})).is_err());
        assert_eq!(
            target_cwd(&h, "read", &json!({"file_path":"/two/file"}))
                .unwrap()
                .root_id
                .as_str(),
            "two"
        );
        assert_eq!(
            target_cwd(&h, "bash", &json!({"workdir":"/one/repo"}))
                .unwrap()
                .root_id
                .as_str(),
            "one"
        );
        assert!(target_cwd(&h, "read", &json!({"file_path":"/outside/file"})).is_err());
        h.roots.truncate(1);
        assert_eq!(
            target_cwd(&h, "read", &json!({"file_path":"file"}))
                .unwrap()
                .root_id
                .as_str(),
            "one"
        );
    }
}
