use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{Read, Write},
    path::Path,
    process::Stdio,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use execution_contracts::{
    ArtifactKind, AttachExecutionRequest, Base64Data, CommandSpec, EnvironmentInheritance,
    ExecutionErrorCode, ExecutionHandle, ExecutionId, ExecutionState, ExecutionStatus,
    InspectExecutionRequest, NetworkMode, ProcessEvent, ProcessEventKind, ProcessInputStatus,
    ProcessOutputStream, ResizePtyRequest, SandboxMode, SignalExecutionRequest,
    StartExecutionRequest, StdinMode, TerminateExecutionRequest, TerminateExecutionResult,
    TimestampMs, Validate, WriteProcessInputRequest, WriteProcessInputResult,
};
use execution_runtime::{ExecutionResult, OperationContext, ProcessEventStream, ProcessRuntime};
use futures_util::stream;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::{Mutex, RwLock, broadcast},
};

use crate::{
    artifacts::LocalArtifactStore,
    error::{error, unsupported},
    path_resolver::PathResolver,
    platform,
};

pub(crate) struct LocalProcessRuntime {
    resolver: Arc<PathResolver>,
    artifacts: Arc<LocalArtifactStore>,
    executions: Arc<RwLock<HashMap<ExecutionId, Arc<ProcessEntry>>>>,
    pty_controls: Arc<RwLock<HashMap<ExecutionId, Arc<PtyControl>>>>,
}

struct ProcessEntry {
    execution_id: ExecutionId,
    status: RwLock<ExecutionStatus>,
    child: Option<Arc<Mutex<Child>>>,
    stdin: Mutex<Option<ChildStdin>>,
    write_ids: Mutex<HashSet<execution_contracts::OperationId>>,
    events: RwLock<Vec<ProcessEvent>>,
    sender: broadcast::Sender<ProcessEvent>,
    sequence: AtomicU64,
    full_output: Mutex<Vec<u8>>,
    persist_full_output: bool,
}

struct PtyControl {
    master: StdMutex<Box<dyn MasterPty + Send>>,
    writer: StdMutex<Box<dyn Write + Send>>,
    killer: StdMutex<Box<dyn ChildKiller + Send + Sync>>,
}

impl LocalProcessRuntime {
    pub fn new(resolver: Arc<PathResolver>, artifacts: Arc<LocalArtifactStore>) -> Self {
        Self {
            resolver,
            artifacts,
            executions: Arc::new(RwLock::new(HashMap::new())),
            pty_controls: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn entry(&self, execution_id: &ExecutionId) -> ExecutionResult<Arc<ProcessEntry>> {
        self.executions
            .read()
            .await
            .get(execution_id)
            .cloned()
            .ok_or_else(|| {
                error(
                    ExecutionErrorCode::UnknownExecution,
                    format!("execution `{execution_id}` was not found"),
                )
            })
    }

    async fn start_pty(
        &self,
        request: StartExecutionRequest,
        cwd: &Path,
        columns: u16,
        rows: u16,
    ) -> ExecutionResult<ExecutionHandle> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols: columns,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|source| {
                error(
                    ExecutionErrorCode::Internal,
                    format!("failed to allocate PTY: {source}"),
                )
            })?;
        let reader = pair.master.try_clone_reader().map_err(|source| {
            error(
                ExecutionErrorCode::Internal,
                format!("failed to open PTY output: {source}"),
            )
        })?;
        let writer = pair.master.take_writer().map_err(|source| {
            error(
                ExecutionErrorCode::Internal,
                format!("failed to open PTY input: {source}"),
            )
        })?;
        let mut command = build_pty_command(&request.command)?;
        command.cwd(cwd);
        configure_pty_environment(&mut command, &request.environment);
        let mut child = pair.slave.spawn_command(command).map_err(|source| {
            error(
                ExecutionErrorCode::Internal,
                format!("failed to start PTY process: {source}"),
            )
        })?;
        let killer = child.clone_killer();
        drop(pair.slave);

        let started_at = now();
        let (sender, _) = broadcast::channel(256);
        let entry = Arc::new(ProcessEntry {
            execution_id: request.execution_id.clone(),
            status: RwLock::new(ExecutionStatus {
                execution_id: request.execution_id.clone(),
                state: ExecutionState::Running,
                started_at: Some(started_at),
                finished_at: None,
                exit_code: None,
                last_sequence: 0,
                full_output_artifact: None,
            }),
            child: None,
            stdin: Mutex::new(None),
            write_ids: Mutex::new(HashSet::new()),
            events: RwLock::new(Vec::new()),
            sender,
            sequence: AtomicU64::new(0),
            full_output: Mutex::new(Vec::new()),
            persist_full_output: request.output.persist_full_output,
        });
        let control = Arc::new(PtyControl {
            master: StdMutex::new(pair.master),
            writer: StdMutex::new(writer),
            killer: StdMutex::new(killer),
        });
        self.executions
            .write()
            .await
            .insert(request.execution_id.clone(), Arc::clone(&entry));
        self.pty_controls
            .write()
            .await
            .insert(request.execution_id.clone(), Arc::clone(&control));
        entry.emit(ProcessEventKind::Started).await;

        let output_entry = Arc::clone(&entry);
        let chunk_size = request.output.max_chunk_bytes as usize;
        let reader_task =
            tokio::task::spawn_blocking(move || read_pty_output(output_entry, reader, chunk_size));
        let wait_task = tokio::task::spawn_blocking(move || child.wait());
        let artifacts = Arc::clone(&self.artifacts);
        let wait_entry = Arc::clone(&entry);
        tokio::spawn(async move {
            let exit_code = match wait_task.await {
                Ok(Ok(status)) => i32::try_from(status.exit_code()).unwrap_or(-1),
                Ok(Err(source)) => {
                    wait_entry
                        .emit(ProcessEventKind::Failed {
                            message: format!("failed to wait for PTY process: {source}"),
                        })
                        .await;
                    wait_entry.status.write().await.state = ExecutionState::Failed;
                    return;
                }
                Err(source) => {
                    wait_entry
                        .emit(ProcessEventKind::Failed {
                            message: format!("PTY wait task failed: {source}"),
                        })
                        .await;
                    wait_entry.status.write().await.state = ExecutionState::Failed;
                    return;
                }
            };
            let _ = reader_task.await;
            finish_execution(&wait_entry, &artifacts, exit_code).await;
        });

        if let Some(timeout_ms) = request.timeout_ms {
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
                if let Ok(mut killer) = control.killer.lock() {
                    let _ = killer.kill();
                }
            });
        }
        Ok(ExecutionHandle {
            execution_id: request.execution_id,
            state: ExecutionState::Running,
            started_at: Some(started_at),
        })
    }
}

impl ProcessEntry {
    async fn emit(&self, event: ProcessEventKind) {
        let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let value = ProcessEvent {
            execution_id: self.execution_id.clone(),
            sequence,
            timestamp: now(),
            event,
        };
        self.events.write().await.push(value.clone());
        let _ = self.sender.send(value);
        self.status.write().await.last_sequence = sequence;
    }

    async fn append_output(&self, bytes: &[u8]) {
        if self.persist_full_output {
            self.full_output.lock().await.extend_from_slice(bytes);
        }
    }
}

#[async_trait]
impl ProcessRuntime for LocalProcessRuntime {
    async fn start(
        &self,
        context: &OperationContext,
        request: StartExecutionRequest,
    ) -> ExecutionResult<ExecutionHandle> {
        request.validate().map_err(invalid_request)?;
        if context.is_cancelled() {
            return Err(error(
                ExecutionErrorCode::Cancelled,
                "execution was cancelled",
            ));
        }
        if let Some(existing) = self.executions.read().await.get(&request.execution_id) {
            let status = existing.status.read().await;
            return Ok(ExecutionHandle {
                execution_id: request.execution_id,
                state: status.state,
                started_at: status.started_at,
            });
        }
        if request.policy.sandbox == SandboxMode::Required {
            return Err(unsupported(
                "the local backend cannot currently guarantee an OS sandbox",
            ));
        }
        if request.policy.network == NetworkMode::Denied {
            return Err(unsupported(
                "the local backend cannot currently guarantee network isolation",
            ));
        }

        let cwd = self.resolver.resolve_existing(&request.cwd, true).await?;
        if let StdinMode::Pty { columns, rows } = request.stdin {
            return self.start_pty(request, &cwd.path, columns, rows).await;
        }
        let mut command = build_command(&request.command)?;
        command.current_dir(&cwd.path);
        configure_environment(&mut command, &request.environment);
        platform::configure_process(&mut command);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        match request.stdin {
            StdinMode::Pipe => {
                command.stdin(Stdio::piped());
            }
            StdinMode::Closed => {
                command.stdin(Stdio::null());
            }
            StdinMode::Pty { .. } => unreachable!(),
        }
        let mut child = command.spawn().map_err(|source| {
            error(
                ExecutionErrorCode::Internal,
                format!("failed to start process: {source}"),
            )
        })?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdin = child.stdin.take();
        let started_at = now();
        let (sender, _) = broadcast::channel(256);
        let child = Arc::new(Mutex::new(child));
        let entry = Arc::new(ProcessEntry {
            execution_id: request.execution_id.clone(),
            status: RwLock::new(ExecutionStatus {
                execution_id: request.execution_id.clone(),
                state: ExecutionState::Running,
                started_at: Some(started_at),
                finished_at: None,
                exit_code: None,
                last_sequence: 0,
                full_output_artifact: None,
            }),
            child: Some(Arc::clone(&child)),
            stdin: Mutex::new(stdin),
            write_ids: Mutex::new(HashSet::new()),
            events: RwLock::new(Vec::new()),
            sender,
            sequence: AtomicU64::new(0),
            full_output: Mutex::new(Vec::new()),
            persist_full_output: request.output.persist_full_output,
        });
        self.executions
            .write()
            .await
            .insert(request.execution_id.clone(), Arc::clone(&entry));
        entry.emit(ProcessEventKind::Started).await;

        let chunk_size = request.output.max_chunk_bytes as usize;
        let stdout_task = stdout.map(|reader| {
            tokio::spawn(read_output(
                Arc::clone(&entry),
                reader,
                ProcessOutputStream::Stdout,
                chunk_size,
            ))
        });
        let stderr_task = stderr.map(|reader| {
            tokio::spawn(read_output(
                Arc::clone(&entry),
                reader,
                ProcessOutputStream::Stderr,
                chunk_size,
            ))
        });
        let artifacts = Arc::clone(&self.artifacts);
        let wait_entry = Arc::clone(&entry);
        tokio::spawn(async move {
            let exit_code = loop {
                let child = wait_entry
                    .child
                    .as_ref()
                    .expect("piped execution has a Tokio child");
                let result = child.lock().await.try_wait();
                match result {
                    Ok(Some(status)) => break status.code().unwrap_or(-1),
                    Ok(None) => tokio::time::sleep(Duration::from_millis(25)).await,
                    Err(source) => {
                        wait_entry
                            .emit(ProcessEventKind::Failed {
                                message: format!("failed to inspect process: {source}"),
                            })
                            .await;
                        wait_entry.status.write().await.state = ExecutionState::Failed;
                        return;
                    }
                }
            };
            if let Some(task) = stdout_task {
                let _ = task.await;
            }
            if let Some(task) = stderr_task {
                let _ = task.await;
            }
            finish_execution(&wait_entry, &artifacts, exit_code).await;
        });

        if let Some(timeout_ms) = request.timeout_ms {
            let timeout_child = Arc::clone(&child);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
                let mut child = timeout_child.lock().await;
                if child.try_wait().ok().flatten().is_none() {
                    let _ = platform::terminate_process(
                        &mut child,
                        execution_contracts::ProcessSignal::Kill,
                    );
                }
            });
        }

        Ok(ExecutionHandle {
            execution_id: request.execution_id,
            state: ExecutionState::Running,
            started_at: Some(started_at),
        })
    }

    async fn inspect(
        &self,
        _context: &OperationContext,
        request: InspectExecutionRequest,
    ) -> ExecutionResult<ExecutionStatus> {
        Ok(self
            .entry(&request.execution_id)
            .await?
            .status
            .read()
            .await
            .clone())
    }

    async fn attach(
        &self,
        _context: &OperationContext,
        request: AttachExecutionRequest,
    ) -> ExecutionResult<ProcessEventStream> {
        let entry = self.entry(&request.execution_id).await?;
        let receiver = entry.sender.subscribe();
        let after = request.after_sequence.unwrap_or(0);
        let replay: VecDeque<_> = entry
            .events
            .read()
            .await
            .iter()
            .filter(|event| event.sequence > after)
            .cloned()
            .collect();
        let state = EventStreamState {
            replay,
            receiver,
            last_sequence: after,
            closed: false,
        };
        Ok(Box::pin(stream::unfold(state, |mut state| async move {
            if let Some(event) = state.replay.pop_front() {
                state.last_sequence = event.sequence;
                state.closed = matches!(
                    event.event,
                    ProcessEventKind::Closed | ProcessEventKind::Failed { .. }
                );
                return Some((Ok(event), state));
            }
            if state.closed {
                return None;
            }
            loop {
                match state.receiver.recv().await {
                    Ok(event) if event.sequence > state.last_sequence => {
                        state.last_sequence = event.sequence;
                        state.closed = matches!(
                            event.event,
                            ProcessEventKind::Closed | ProcessEventKind::Failed { .. }
                        );
                        return Some((Ok(event), state));
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        state.closed = true;
                        return Some((
                            Err(error(
                                ExecutionErrorCode::Disconnected,
                                "process event subscriber lagged; reattach after the last sequence",
                            )),
                            state,
                        ));
                    }
                }
            }
        })))
    }

    async fn write_input(
        &self,
        _context: &OperationContext,
        request: WriteProcessInputRequest,
    ) -> ExecutionResult<WriteProcessInputResult> {
        let entry = self.entry(&request.execution_id).await?;
        let mut write_ids = entry.write_ids.lock().await;
        if write_ids.contains(&request.write_id) {
            return Ok(WriteProcessInputResult {
                status: ProcessInputStatus::Accepted,
            });
        }
        let bytes = STANDARD.decode(&request.data.0).map_err(|source| {
            error(
                ExecutionErrorCode::InvalidRequest,
                format!("invalid input base64: {source}"),
            )
        })?;
        if let Some(control) = self.pty_controls.read().await.get(&request.execution_id) {
            let mut writer = control
                .writer
                .lock()
                .map_err(|_| error(ExecutionErrorCode::Internal, "PTY writer lock was poisoned"))?;
            writer.write_all(&bytes).map_err(|source| {
                error(
                    ExecutionErrorCode::StdinClosed,
                    format!("failed to write PTY input: {source}"),
                )
            })?;
            writer.flush().map_err(|source| {
                error(
                    ExecutionErrorCode::StdinClosed,
                    format!("failed to flush PTY input: {source}"),
                )
            })?;
            write_ids.insert(request.write_id);
            return Ok(WriteProcessInputResult {
                status: ProcessInputStatus::Accepted,
            });
        }
        let mut stdin = entry.stdin.lock().await;
        let Some(stdin) = stdin.as_mut() else {
            return Ok(WriteProcessInputResult {
                status: ProcessInputStatus::StdinClosed,
            });
        };
        stdin.write_all(&bytes).await.map_err(|source| {
            error(
                ExecutionErrorCode::StdinClosed,
                format!("failed to write process input: {source}"),
            )
        })?;
        stdin.flush().await.map_err(|source| {
            error(
                ExecutionErrorCode::StdinClosed,
                format!("failed to flush process input: {source}"),
            )
        })?;
        write_ids.insert(request.write_id);
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
        let controls = self.pty_controls.read().await;
        let control = controls.get(&request.execution_id).ok_or_else(|| {
            error(
                ExecutionErrorCode::Unsupported,
                "execution does not own a PTY",
            )
        })?;
        control
            .master
            .lock()
            .map_err(|_| error(ExecutionErrorCode::Internal, "PTY lock was poisoned"))?
            .resize(PtySize {
                rows: request.rows,
                cols: request.columns,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|source| {
                error(
                    ExecutionErrorCode::Internal,
                    format!("failed to resize PTY: {source}"),
                )
            })
    }

    async fn signal(
        &self,
        _context: &OperationContext,
        request: SignalExecutionRequest,
    ) -> ExecutionResult<()> {
        let entry = self.entry(&request.execution_id).await?;
        if let Some(child) = &entry.child {
            let mut child = child.lock().await;
            return platform::terminate_process(&mut child, request.signal);
        }
        let controls = self.pty_controls.read().await;
        let control = controls.get(&request.execution_id).ok_or_else(|| {
            error(
                ExecutionErrorCode::UnknownExecution,
                "PTY control was not found",
            )
        })?;
        match request.signal {
            execution_contracts::ProcessSignal::Kill
            | execution_contracts::ProcessSignal::Terminate
            | execution_contracts::ProcessSignal::Interrupt => control
                .killer
                .lock()
                .map_err(|_| error(ExecutionErrorCode::Internal, "PTY killer lock was poisoned"))?
                .kill()
                .map_err(|source| {
                    error(
                        ExecutionErrorCode::Internal,
                        format!("failed to signal PTY process: {source}"),
                    )
                }),
            _ => Err(unsupported(
                "this PTY backend only supports interrupt, terminate, and kill",
            )),
        }
    }

    async fn terminate(
        &self,
        _context: &OperationContext,
        request: TerminateExecutionRequest,
    ) -> ExecutionResult<TerminateExecutionResult> {
        let entry = self.entry(&request.execution_id).await?;
        let was_running = matches!(
            entry.status.read().await.state,
            ExecutionState::Queued | ExecutionState::Starting | ExecutionState::Running
        );
        if was_running {
            if let Some(child) = &entry.child {
                let mut child = child.lock().await;
                platform::terminate_process(&mut child, execution_contracts::ProcessSignal::Kill)?;
            } else {
                let controls = self.pty_controls.read().await;
                let control = controls.get(&request.execution_id).ok_or_else(|| {
                    error(
                        ExecutionErrorCode::UnknownExecution,
                        "PTY control was not found",
                    )
                })?;
                control
                    .killer
                    .lock()
                    .map_err(|_| {
                        error(ExecutionErrorCode::Internal, "PTY killer lock was poisoned")
                    })?
                    .kill()
                    .map_err(|source| {
                        error(
                            ExecutionErrorCode::Internal,
                            format!("failed to terminate PTY process: {source}"),
                        )
                    })?;
            }
        }
        Ok(TerminateExecutionResult { was_running })
    }
}

async fn read_output<R>(
    entry: Arc<ProcessEntry>,
    mut reader: R,
    stream: ProcessOutputStream,
    chunk_size: usize,
) where
    R: AsyncRead + Unpin,
{
    let mut bytes = vec![0; chunk_size.max(1)];
    loop {
        match reader.read(&mut bytes).await {
            Ok(0) => break,
            Ok(read) => {
                let chunk = &bytes[..read];
                entry.append_output(chunk).await;
                entry
                    .emit(ProcessEventKind::Output {
                        stream,
                        data: Base64Data(STANDARD.encode(chunk)),
                    })
                    .await;
            }
            Err(_) => break,
        }
    }
}

fn read_pty_output(entry: Arc<ProcessEntry>, mut reader: Box<dyn Read + Send>, chunk_size: usize) {
    let runtime = tokio::runtime::Handle::current();
    let mut bytes = vec![0; chunk_size.max(1)];
    loop {
        match reader.read(&mut bytes) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let chunk = &bytes[..read];
                runtime.block_on(async {
                    entry.append_output(chunk).await;
                    entry
                        .emit(ProcessEventKind::Output {
                            stream: ProcessOutputStream::Pty,
                            data: Base64Data(STANDARD.encode(chunk)),
                        })
                        .await;
                });
            }
        }
    }
}

async fn finish_execution(entry: &ProcessEntry, artifacts: &LocalArtifactStore, exit_code: i32) {
    let artifact = if entry.persist_full_output {
        let output = entry.full_output.lock().await.clone();
        artifacts
            .put(
                ArtifactKind::ProcessOutput,
                Some(format!("{}.log", entry.execution_id)),
                Some("application/octet-stream".to_owned()),
                &output,
            )
            .await
            .ok()
    } else {
        None
    };
    {
        let mut status = entry.status.write().await;
        status.state = ExecutionState::Exited;
        status.finished_at = Some(now());
        status.exit_code = Some(exit_code);
        status.full_output_artifact = artifact.map(|value| value.artifact_id);
    }
    entry
        .emit(ProcessEventKind::Exited {
            exit_code,
            sandbox_denied: false,
        })
        .await;
    entry.emit(ProcessEventKind::Closed).await;
}

fn build_command(spec: &CommandSpec) -> ExecutionResult<Command> {
    match spec {
        CommandSpec::Argv { program, arguments } => {
            let mut command = Command::new(program);
            command.args(arguments);
            Ok(command)
        }
        CommandSpec::Shell {
            command,
            shell,
            login,
        } => {
            let shell = shell
                .clone()
                .unwrap_or_else(|| platform::default_shell().executable);
            let mut process = Command::new(shell);
            #[cfg(unix)]
            process.arg(if *login { "-lc" } else { "-c" }).arg(command);
            #[cfg(windows)]
            {
                let _ = login;
                process.arg("/C").arg(command);
            }
            Ok(process)
        }
    }
}

fn build_pty_command(spec: &CommandSpec) -> ExecutionResult<CommandBuilder> {
    match spec {
        CommandSpec::Argv { program, arguments } => {
            let mut command = CommandBuilder::new(program);
            command.args(arguments);
            Ok(command)
        }
        CommandSpec::Shell {
            command,
            shell,
            login,
        } => {
            let shell = shell
                .clone()
                .unwrap_or_else(|| platform::default_shell().executable);
            let mut process = CommandBuilder::new(shell);
            #[cfg(unix)]
            {
                process.arg(if *login { "-lc" } else { "-c" });
                process.arg(command);
            }
            #[cfg(windows)]
            {
                let _ = login;
                process.arg("/C");
                process.arg(command);
            }
            Ok(process)
        }
    }
}

fn configure_environment(
    command: &mut Command,
    environment: &execution_contracts::EnvironmentVariables,
) {
    match environment.inherit {
        EnvironmentInheritance::All => {}
        EnvironmentInheritance::None => {
            command.env_clear();
        }
        EnvironmentInheritance::AllowList => {
            let inherited: Vec<_> = environment
                .allow
                .iter()
                .filter_map(|name| std::env::var_os(name).map(|value| (name, value)))
                .collect();
            command.env_clear();
            command.envs(inherited);
        }
    }
    for name in &environment.remove {
        command.env_remove(name);
    }
    command.envs(&environment.set);
}

fn configure_pty_environment(
    command: &mut CommandBuilder,
    environment: &execution_contracts::EnvironmentVariables,
) {
    match environment.inherit {
        EnvironmentInheritance::All => {}
        EnvironmentInheritance::None => command.env_clear(),
        EnvironmentInheritance::AllowList => {
            let inherited: Vec<_> = environment
                .allow
                .iter()
                .filter_map(|name| std::env::var_os(name).map(|value| (name.clone(), value)))
                .collect();
            command.env_clear();
            for (name, value) in inherited {
                command.env(name, value);
            }
        }
    }
    for name in &environment.remove {
        command.env_remove(name);
    }
    for (name, value) in &environment.set {
        command.env(name, value);
    }
}

struct EventStreamState {
    replay: VecDeque<ProcessEvent>,
    receiver: broadcast::Receiver<ProcessEvent>,
    last_sequence: u64,
    closed: bool,
}

fn invalid_request(
    source: execution_contracts::ValidationError,
) -> execution_contracts::ExecutionError {
    error(
        ExecutionErrorCode::InvalidRequest,
        format!("invalid process request: {source}"),
    )
}

fn now() -> TimestampMs {
    let milliseconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    TimestampMs(u64::try_from(milliseconds).unwrap_or(u64::MAX))
}
