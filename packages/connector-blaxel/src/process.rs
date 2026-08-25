use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use execution_contracts::{
    AttachExecutionRequest, ExecutionErrorCode, ExecutionHandle, ExecutionId, ExecutionState,
    ExecutionStatus, InspectExecutionRequest, NetworkMode, ProcessEvent, ProcessEventKind,
    ProcessInputStatus, ResizePtyRequest, SignalExecutionRequest, StartExecutionRequest,
    TerminateExecutionRequest, TerminateExecutionResult, TimestampMs, Validate,
    WriteProcessInputRequest, WriteProcessInputResult,
};
use execution_runtime::{ExecutionResult, OperationContext, ProcessEventStream, ProcessRuntime};
use futures_util::{StreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock, broadcast};

use crate::{
    BlaxelRuntimeConfig, BlaxelTransport, RemoteProcessEvent, RemoteProcessRequest,
    RemoteProcessStream, RemoteStreamKind,
    error::{execution_error, invalid_request, transport_execution_error},
    runner::{InlineRunner, encode_inline_argument},
};

const PROCESS_WRAPPER: &str = include_str!("process_wrapper.py");
const PROCESS_TAG_PREFIX: &str = "agent-pane-execution-";

#[derive(Clone)]
pub(crate) struct BlaxelProcessRuntime {
    inner: Arc<ProcessInner>,
}

struct ProcessInner {
    transport: Arc<dyn BlaxelTransport>,
    runner: Arc<InlineRunner>,
    python_command: String,
    state_directory: String,
    roots: Value,
    grants: Value,
    entries: RwLock<HashMap<ExecutionId, Arc<ProcessEntry>>>,
    start_lock: Mutex<()>,
}

struct ProcessEntry {
    pid: u32,
    status: RwLock<ExecutionStatus>,
    events: RwLock<Vec<ProcessEvent>>,
    sender: broadcast::Sender<ProcessEvent>,
    write_ids: Mutex<HashSet<execution_contracts::OperationId>>,
}

#[derive(Deserialize)]
struct ReplayResult {
    events: Vec<ProcessEvent>,
    status: Option<ExecutionStatus>,
    #[serde(default)]
    runner_pid: Option<u32>,
}

impl BlaxelProcessRuntime {
    pub fn new(
        transport: Arc<dyn BlaxelTransport>,
        runner: Arc<InlineRunner>,
        python_command: String,
        config: &BlaxelRuntimeConfig,
    ) -> Self {
        Self {
            inner: Arc::new(ProcessInner {
                transport,
                runner,
                python_command,
                state_directory: config.state_directory.clone(),
                roots: json!(
                    config
                        .workspace_roots
                        .iter()
                        .map(|root| json!({
                            "id": root.id.as_str(), "path": root.path, "read_only": root.read_only,
                        }))
                        .collect::<Vec<_>>()
                ),
                grants: json!(config.native_grants.iter().map(|grant| json!({
                    "id": grant.id.as_str(), "path": grant.path, "read_only": grant.read_only,
                })).collect::<Vec<_>>()),
                entries: RwLock::new(HashMap::new()),
                start_lock: Mutex::new(()),
            }),
        }
    }

    async fn entry(&self, execution_id: &ExecutionId) -> ExecutionResult<Arc<ProcessEntry>> {
        if let Some(entry) = self.inner.entries.read().await.get(execution_id).cloned() {
            return Ok(entry);
        }
        self.hydrate(execution_id).await
    }

    async fn hydrate(&self, execution_id: &ExecutionId) -> ExecutionResult<Arc<ProcessEntry>> {
        let replay: ReplayResult = self
            .inner
            .runner
            .call(
                "process_replay",
                &json!({
                    "execution_id": execution_id,
                    "after_sequence": 0,
                }),
            )
            .await?;
        let status = replay.status.ok_or_else(|| {
            execution_error(
                ExecutionErrorCode::UnknownExecution,
                format!("execution `{execution_id}` was not found"),
            )
        })?;
        let processes = self
            .inner
            .transport
            .list_processes()
            .await
            .map_err(transport_execution_error)?;
        let expected_tag = process_tag(execution_id);
        let pid = replay
            .runner_pid
            .filter(|pid| {
                processes.iter().any(|process| {
                    process.pid == *pid
                        && process.status == "running"
                        && process.tag.as_deref() == Some(expected_tag.as_str())
                })
            })
            .unwrap_or(0);
        let (sender, _) = broadcast::channel(512);
        let entry = Arc::new(ProcessEntry {
            pid,
            status: RwLock::new(status.clone()),
            events: RwLock::new(replay.events),
            sender,
            write_ids: Mutex::new(HashSet::new()),
        });
        self.inner
            .entries
            .write()
            .await
            .insert(execution_id.clone(), Arc::clone(&entry));
        if status.state == ExecutionState::Running && pid != 0 {
            let inner = Arc::clone(&self.inner);
            let entry_for_task = Arc::clone(&entry);
            tokio::spawn(async move {
                monitor_with_recovery(inner, entry_for_task, None).await;
            });
        }
        Ok(entry)
    }

    async fn send_control(&self, entry: &ProcessEntry, value: Value) -> ExecutionResult<()> {
        if entry.pid == 0 {
            return Err(execution_error(
                ExecutionErrorCode::UnknownExecution,
                "execution process is no longer running",
            ));
        }
        let execution_id = entry.status.read().await.execution_id.clone();
        let control_id = uuid::Uuid::now_v7().to_string();
        let path = format!(
            "{}/processes/{}/control/{}.json",
            self.inner.state_directory.trim_end_matches('/'),
            safe_id(execution_id.as_str()),
            safe_id(&control_id),
        );
        let content = serde_json::to_vec(&value)
            .map_err(|source| execution_error(ExecutionErrorCode::Internal, source.to_string()))?;
        self.inner
            .transport
            .write_control_file(&path, &content)
            .await
            .map_err(transport_execution_error)
    }
}

#[async_trait]
impl ProcessRuntime for BlaxelProcessRuntime {
    async fn start(
        &self,
        context: &OperationContext,
        request: StartExecutionRequest,
    ) -> ExecutionResult<ExecutionHandle> {
        request.validate().map_err(invalid_request)?;
        if context.is_cancelled() {
            return Err(execution_error(
                ExecutionErrorCode::Cancelled,
                "process start was cancelled",
            ));
        }
        if request.policy.network == NetworkMode::Denied {
            return Err(execution_error(
                ExecutionErrorCode::Unsupported,
                "Blaxel cannot securely deny network access for only one process; configure the sandbox network instead",
            ));
        }
        if request.policy.profile.is_some() {
            return Err(execution_error(
                ExecutionErrorCode::Unsupported,
                "Blaxel process policy profiles are unsupported",
            ));
        }
        let _guard = self.inner.start_lock.lock().await;
        if let Some(entry) = self
            .inner
            .entries
            .read()
            .await
            .get(&request.execution_id)
            .cloned()
        {
            let status = entry.status.read().await;
            return Ok(ExecutionHandle {
                execution_id: request.execution_id,
                state: status.state,
                started_at: status.started_at,
            });
        }
        if let Ok(status) = self
            .inner
            .runner
            .call::<_, ExecutionStatus>(
                "process_status",
                &json!({
                    "execution_id": request.execution_id,
                }),
            )
            .await
        {
            return Ok(ExecutionHandle {
                execution_id: status.execution_id,
                state: status.state,
                started_at: status.started_at,
            });
        }
        let argument = encode_inline_argument(&json!({
            "request": request,
            "state_directory": self.inner.state_directory,
            "roots": self.inner.roots,
            "grants": self.inner.grants,
        }))?;
        let mut remote = self
            .inner
            .transport
            .start_process(RemoteProcessRequest {
                command: self.inner.python_command.clone(),
                arguments: vec![
                    "-u".to_owned(),
                    "-c".to_owned(),
                    PROCESS_WRAPPER.to_owned(),
                    argument,
                ],
                environment: Default::default(),
                cwd: None,
                tag: Some(process_tag(&request.execution_id)),
                stdin: false,
                pty: None,
                timeout_ms: None,
            })
            .await
            .map_err(transport_execution_error)?;
        let pid = loop {
            match remote.next().await {
                Some(Ok(RemoteProcessEvent::Started { pid })) => break pid,
                Some(Ok(_)) => continue,
                Some(Err(source)) => return Err(transport_execution_error(source)),
                None => {
                    return Err(execution_error(
                        ExecutionErrorCode::Disconnected,
                        "Blaxel process stream ended before start",
                    ));
                }
            }
        };
        let (sender, _) = broadcast::channel(512);
        let entry = Arc::new(ProcessEntry {
            pid,
            status: RwLock::new(ExecutionStatus {
                execution_id: request.execution_id.clone(),
                state: ExecutionState::Starting,
                started_at: None,
                finished_at: None,
                exit_code: None,
                last_sequence: 0,
                full_output_artifact: None,
            }),
            events: RwLock::new(Vec::new()),
            sender,
            write_ids: Mutex::new(HashSet::new()),
        });
        self.inner
            .entries
            .write()
            .await
            .insert(request.execution_id.clone(), Arc::clone(&entry));
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            monitor_with_recovery(inner, entry, Some(remote)).await;
        });
        Ok(ExecutionHandle {
            execution_id: request.execution_id,
            state: ExecutionState::Starting,
            started_at: None,
        })
    }

    async fn inspect(
        &self,
        _context: &OperationContext,
        request: InspectExecutionRequest,
    ) -> ExecutionResult<ExecutionStatus> {
        if let Some(entry) = self
            .inner
            .entries
            .read()
            .await
            .get(&request.execution_id)
            .cloned()
        {
            return Ok(entry.status.read().await.clone());
        }
        self.inner.runner.call("process_status", &request).await
    }

    async fn attach(
        &self,
        _context: &OperationContext,
        request: AttachExecutionRequest,
    ) -> ExecutionResult<ProcessEventStream> {
        let entry = self.entry(&request.execution_id).await?;
        let after = request.after_sequence.unwrap_or(0);
        let replay: VecDeque<_> = entry
            .events
            .read()
            .await
            .iter()
            .filter(|event| event.sequence > after)
            .cloned()
            .collect();
        let receiver = entry.sender.subscribe();
        let replay_stream = stream::iter(replay.into_iter().map(Ok));
        let live_stream = stream::unfold(receiver, |mut receiver| async move {
            match receiver.recv().await {
                Ok(event) => Some((Ok(event), receiver)),
                Err(broadcast::error::RecvError::Lagged(count)) => Some((
                    Err(execution_error(
                        ExecutionErrorCode::Disconnected,
                        format!(
                            "process event subscriber lagged by {count} events; reattach with the last sequence"
                        ),
                    )),
                    receiver,
                )),
                Err(broadcast::error::RecvError::Closed) => None,
            }
        });
        Ok(Box::pin(replay_stream.chain(live_stream)))
    }

    async fn write_input(
        &self,
        _context: &OperationContext,
        request: WriteProcessInputRequest,
    ) -> ExecutionResult<WriteProcessInputResult> {
        let entry = match self.entry(&request.execution_id).await {
            Ok(entry) => entry,
            Err(error) if error.code == ExecutionErrorCode::UnknownExecution => {
                return Ok(WriteProcessInputResult {
                    status: ProcessInputStatus::UnknownExecution,
                });
            }
            Err(error) => return Err(error),
        };
        if entry.write_ids.lock().await.contains(&request.write_id) {
            return Ok(WriteProcessInputResult {
                status: ProcessInputStatus::Accepted,
            });
        }
        let status = entry.status.read().await.state;
        if status == ExecutionState::Starting {
            return Ok(WriteProcessInputResult {
                status: ProcessInputStatus::Starting,
            });
        }
        if status != ExecutionState::Running {
            return Ok(WriteProcessInputResult {
                status: ProcessInputStatus::StdinClosed,
            });
        }
        self.send_control(
            &entry,
            json!({
                "type": "input", "write_id": request.write_id,
                "data": request.data.0,
            }),
        )
        .await?;
        entry.write_ids.lock().await.insert(request.write_id);
        Ok(WriteProcessInputResult {
            status: ProcessInputStatus::Accepted,
        })
    }

    async fn resize_pty(
        &self,
        _context: &OperationContext,
        request: ResizePtyRequest,
    ) -> ExecutionResult<()> {
        request.validate().map_err(invalid_request)?;
        let entry = self.entry(&request.execution_id).await?;
        self.send_control(
            &entry,
            json!({"type": "resize", "columns": request.columns, "rows": request.rows}),
        )
        .await
    }

    async fn signal(
        &self,
        _context: &OperationContext,
        request: SignalExecutionRequest,
    ) -> ExecutionResult<()> {
        let entry = self.entry(&request.execution_id).await?;
        self.send_control(&entry, json!({"type": "signal", "signal": request.signal}))
            .await
    }

    async fn terminate(
        &self,
        _context: &OperationContext,
        request: TerminateExecutionRequest,
    ) -> ExecutionResult<TerminateExecutionResult> {
        let entry = match self.entry(&request.execution_id).await {
            Ok(entry) => entry,
            Err(error) if error.code == ExecutionErrorCode::UnknownExecution => {
                return Ok(TerminateExecutionResult { was_running: false });
            }
            Err(error) => return Err(error),
        };
        let running = matches!(
            entry.status.read().await.state,
            ExecutionState::Starting | ExecutionState::Running
        );
        if running
            && self
                .send_control(&entry, json!({"type": "terminate"}))
                .await
                .is_err()
            && entry.pid != 0
        {
            self.inner
                .transport
                .signal_process(entry.pid, false)
                .await
                .map_err(transport_execution_error)?;
        }
        Ok(TerminateExecutionResult {
            was_running: running,
        })
    }
}

async fn monitor_with_recovery(
    inner: Arc<ProcessInner>,
    entry: Arc<ProcessEntry>,
    mut remote: Option<RemoteProcessStream>,
) {
    let mut decoder = LineDecoder::default();
    loop {
        if let Some(mut stream) = remote.take() {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(RemoteProcessEvent::Output {
                        stream: RemoteStreamKind::Stdout,
                        data,
                    }) => {
                        for event in decoder.push(&data) {
                            let Ok(event) = event else { continue };
                            apply_event(&entry, event).await;
                        }
                    }
                    Ok(RemoteProcessEvent::Output {
                        stream: RemoteStreamKind::Stderr,
                        data,
                    }) => {
                        let message = String::from_utf8_lossy(&data).trim().to_owned();
                        if !message.is_empty() {
                            // Wrapper stderr is reserved for wrapper failures, never child stderr.
                            let sequence =
                                entry.status.read().await.last_sequence.saturating_add(1);
                            let event = ProcessEvent {
                                execution_id: entry.status.read().await.execution_id.clone(),
                                sequence,
                                timestamp: now(),
                                event: ProcessEventKind::Failed { message },
                            };
                            apply_event(&entry, event).await;
                        }
                    }
                    Ok(RemoteProcessEvent::Exited { .. }) | Err(_) => break,
                    Ok(RemoteProcessEvent::Started { .. } | RemoteProcessEvent::KeepAlive)
                    | Ok(RemoteProcessEvent::Output {
                        stream: RemoteStreamKind::Pty,
                        ..
                    }) => {}
                }
            }
        }

        let after = entry.status.read().await.last_sequence;
        let execution_id = entry.status.read().await.execution_id.clone();
        let connected = if entry.pid == 0 {
            None
        } else {
            inner.transport.connect_process(entry.pid, None).await.ok()
        };
        let replay = inner
            .runner
            .call::<_, ReplayResult>(
                "process_replay",
                &json!({
                    "execution_id": execution_id,
                    "after_sequence": after,
                }),
            )
            .await;
        if let Ok(replay) = replay {
            for event in replay.events {
                apply_event(&entry, event).await;
            }
            if let Some(status) = replay.status {
                let terminal = !matches!(
                    status.state,
                    ExecutionState::Queued | ExecutionState::Starting | ExecutionState::Running
                );
                *entry.status.write().await = status;
                if terminal {
                    return;
                }
            }
        }
        if let Some(stream) = connected {
            remote = Some(stream);
            continue;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn apply_event(entry: &ProcessEntry, event: ProcessEvent) {
    let mut status = entry.status.write().await;
    if event.sequence <= status.last_sequence {
        return;
    }
    status.last_sequence = event.sequence;
    match &event.event {
        ProcessEventKind::Started => {
            status.state = ExecutionState::Running;
            status.started_at = Some(event.timestamp);
        }
        ProcessEventKind::Exited { exit_code, .. } => {
            status.state = ExecutionState::Exited;
            status.exit_code = Some(*exit_code);
            status.finished_at = Some(event.timestamp);
        }
        ProcessEventKind::Failed { .. } => {
            status.state = ExecutionState::Failed;
            status.finished_at = Some(event.timestamp);
        }
        ProcessEventKind::Closed | ProcessEventKind::Output { .. } => {}
    }
    drop(status);
    entry.events.write().await.push(event.clone());
    let _ = entry.sender.send(event);
}

#[derive(Default)]
struct LineDecoder {
    buffer: Vec<u8>,
}

impl LineDecoder {
    fn push(&mut self, bytes: &[u8]) -> Vec<Result<ProcessEvent, serde_json::Error>> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<_> = self.buffer.drain(..=index).collect();
            if line.len() > 1 {
                events.push(serde_json::from_slice(&line[..line.len() - 1]));
            }
        }
        events
    }
}

fn now() -> TimestampMs {
    TimestampMs(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX),
    )
}

fn process_tag(execution_id: &ExecutionId) -> String {
    let suffix = safe_id(execution_id.as_str());
    let suffix = &suffix[..32];
    format!("{PROCESS_TAG_PREFIX}{suffix}")
}

fn safe_id(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_split_journal_lines() {
        let mut decoder = LineDecoder::default();
        let first =
            br#"{"execution_id":"x","sequence":1,"timestamp":1,"event":{"type":"started"}}"#;
        assert!(decoder.push(&first[..20]).is_empty());
        let mut rest = first[20..].to_vec();
        rest.push(b'\n');
        let values = decoder.push(&rest);
        assert_eq!(values.len(), 1);
        assert!(values[0].is_ok());
    }

    #[test]
    fn process_tags_are_stable_and_provider_safe() {
        let execution_id = ExecutionId::new("unsafe execution/id").unwrap();
        let first = process_tag(&execution_id);
        assert_eq!(first, process_tag(&execution_id));
        assert!(first.starts_with(PROCESS_TAG_PREFIX));
        assert!(
            first
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
        );
    }
}
