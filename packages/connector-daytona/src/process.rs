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
use tokio::sync::{Mutex, Notify, RwLock, broadcast};

use crate::{
    DaytonaRuntimeConfig, DaytonaTransport, RemoteProcessEvent, RemoteProcessId,
    RemoteProcessRequest, RemoteProcessStream, RemoteStreamKind,
    error::{execution_error, invalid_request, transport_execution_error},
    runner::{InlineRunner, encode_inline_argument},
};

const PROCESS_WRAPPER: &str = include_str!("process_wrapper.py");
const PROCESS_TAG_PREFIX: &str = "agent-pane-execution:";

#[derive(Clone)]
pub(crate) struct DaytonaProcessRuntime {
    inner: Arc<ProcessInner>,
}

struct ProcessInner {
    transport: Arc<dyn DaytonaTransport>,
    runner: Arc<InlineRunner>,
    python_command: String,
    state_directory: String,
    roots: Value,
    grants: Value,
    network_block_all: bool,
    entries: RwLock<HashMap<ExecutionId, Arc<ProcessEntry>>>,
    start_lock: Mutex<()>,
}

struct ProcessEntry {
    process_id: Option<RemoteProcessId>,
    status: RwLock<ExecutionStatus>,
    events: RwLock<Vec<ProcessEvent>>,
    sender: broadcast::Sender<ProcessEvent>,
    write_ids: Mutex<HashSet<execution_contracts::OperationId>>,
    control_acks: Mutex<HashMap<String, Arc<Notify>>>,
}

#[derive(Deserialize)]
struct ReplayResult {
    events: Vec<ProcessEvent>,
    status: Option<ExecutionStatus>,
}

impl DaytonaProcessRuntime {
    pub fn new(
        transport: Arc<dyn DaytonaTransport>,
        runner: Arc<InlineRunner>,
        python_command: String,
        network_block_all: bool,
        config: &DaytonaRuntimeConfig,
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
                network_block_all,
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
        let session_id = execution_session_id(execution_id);
        let process_id = processes
            .into_iter()
            .find(|process| {
                process.process_id.session_id == session_id && process.status == "running"
            })
            .map(|process| process.process_id);
        let (sender, _) = broadcast::channel(512);
        let entry = Arc::new(ProcessEntry {
            process_id: process_id.clone(),
            status: RwLock::new(status.clone()),
            events: RwLock::new(replay.events),
            sender,
            write_ids: Mutex::new(HashSet::new()),
            control_acks: Mutex::new(HashMap::new()),
        });
        self.inner
            .entries
            .write()
            .await
            .insert(execution_id.clone(), Arc::clone(&entry));
        if status.state == ExecutionState::Running && process_id.is_some() {
            let inner = Arc::clone(&self.inner);
            let entry_for_task = Arc::clone(&entry);
            tokio::spawn(async move {
                monitor_with_recovery(inner, entry_for_task, None).await;
            });
        }
        Ok(entry)
    }

    async fn send_control(&self, entry: &ProcessEntry, value: Value) -> ExecutionResult<()> {
        let Some(process_id) = entry.process_id.as_ref() else {
            return Err(execution_error(
                ExecutionErrorCode::UnknownExecution,
                "execution process is no longer running",
            ));
        };
        let mut bytes = serde_json::to_vec(&value)
            .map_err(|source| execution_error(ExecutionErrorCode::Internal, source.to_string()))?;
        bytes.push(b'\n');
        self.inner
            .transport
            .send_input(process_id, false, &bytes)
            .await
            .map_err(transport_execution_error)
    }

    async fn send_control_acked(
        &self,
        entry: &ProcessEntry,
        mut value: Value,
    ) -> ExecutionResult<()> {
        let control_id = uuid::Uuid::now_v7().to_string();
        let Some(object) = value.as_object_mut() else {
            return Err(execution_error(
                ExecutionErrorCode::Internal,
                "process control payload must be an object",
            ));
        };
        object.insert("control_id".to_owned(), json!(control_id));
        let acknowledged = Arc::new(Notify::new());
        entry
            .control_acks
            .lock()
            .await
            .insert(control_id.clone(), Arc::clone(&acknowledged));
        if let Err(error) = self.send_control(entry, value).await {
            entry.control_acks.lock().await.remove(&control_id);
            return Err(error);
        }
        if tokio::time::timeout(Duration::from_secs(10), acknowledged.notified())
            .await
            .is_err()
        {
            entry.control_acks.lock().await.remove(&control_id);
            return Err(execution_error(
                ExecutionErrorCode::DeadlineExceeded,
                "Daytona process wrapper did not acknowledge the control request",
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl ProcessRuntime for DaytonaProcessRuntime {
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
        if request.policy.network == NetworkMode::Denied && !self.inner.network_block_all {
            return Err(execution_error(
                ExecutionErrorCode::Unsupported,
                "Daytona cannot securely deny network access for only one process; configure networkBlockAll on the sandbox and mark it in the connection config",
            ));
        }
        if request.policy.profile.is_some() {
            return Err(execution_error(
                ExecutionErrorCode::Unsupported,
                "Daytona process policy profiles are unsupported",
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
                session_id: Some(execution_session_id(&request.execution_id)),
                command: self.inner.python_command.clone(),
                arguments: vec![
                    "-u".to_owned(),
                    "-c".to_owned(),
                    PROCESS_WRAPPER.to_owned(),
                    argument,
                ],
                environment: Default::default(),
                cwd: None,
                tag: Some(format!("{PROCESS_TAG_PREFIX}{}", request.execution_id)),
                stdin: true,
                pty: None,
                timeout_ms: None,
            })
            .await
            .map_err(transport_execution_error)?;
        let process_id = loop {
            match remote.next().await {
                Some(Ok(RemoteProcessEvent::Started { process_id })) => break process_id,
                Some(Ok(_)) => continue,
                Some(Err(source)) => return Err(transport_execution_error(source)),
                None => {
                    return Err(execution_error(
                        ExecutionErrorCode::Disconnected,
                        "Daytona process stream ended before start",
                    ));
                }
            }
        };
        let (sender, _) = broadcast::channel(512);
        let entry = Arc::new(ProcessEntry {
            process_id: Some(process_id),
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
            control_acks: Mutex::new(HashMap::new()),
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
        self.send_control_acked(
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
        self.send_control_acked(
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
        self.send_control_acked(&entry, json!({"type": "signal", "signal": request.signal}))
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
                .send_control_acked(&entry, json!({"type": "terminate"}))
                .await
                .is_err()
            && entry.process_id.is_some()
        {
            if let Some(process_id) = entry.process_id.as_ref() {
                self.inner
                    .transport
                    .signal_process(process_id, false)
                    .await
                    .map_err(transport_execution_error)?;
            }
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
                        for output in decoder.push(&data) {
                            match output {
                                Ok(WrapperOutput::Event(event)) => apply_event(&entry, event).await,
                                Ok(WrapperOutput::ControlAck { control_ack }) => {
                                    apply_control_ack(&entry, &control_ack).await;
                                }
                                Err(_) => {}
                            }
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
        let connected = match entry.process_id.as_ref() {
            Some(process_id) => inner.transport.connect_process(process_id, None).await.ok(),
            None => None,
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

async fn apply_control_ack(entry: &ProcessEntry, control_id: &str) {
    if let Some(acknowledged) = entry.control_acks.lock().await.remove(control_id) {
        acknowledged.notify_one();
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WrapperOutput {
    ControlAck { control_ack: String },
    Event(ProcessEvent),
}

#[derive(Default)]
struct LineDecoder {
    buffer: Vec<u8>,
}

impl LineDecoder {
    fn push(&mut self, bytes: &[u8]) -> Vec<Result<WrapperOutput, serde_json::Error>> {
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

fn execution_session_id(execution_id: &ExecutionId) -> String {
    let digest = Sha256::digest(execution_id.as_str().as_bytes());
    let suffix = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("ap-exec-{suffix}")
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
        assert!(matches!(values[0], Ok(WrapperOutput::Event(_))));
    }

    #[test]
    fn execution_session_ids_are_stable_and_provider_safe() {
        let id = execution_contracts::ExecutionId::new("user execution/1").unwrap();
        let first = execution_session_id(&id);
        assert_eq!(first, execution_session_id(&id));
        assert!(first.starts_with("ap-exec-"));
        assert!(
            first
                .chars()
                .all(|value| value.is_ascii_alphanumeric() || value == '-')
        );
    }
}
