use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex as StdMutex, Weak,
        atomic::{AtomicU8, AtomicU32, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use execution_core::{
    CommandSpec, EnvironmentMode, EnvironmentVariables, ExecutionError, ExecutionErrorCode,
    ExecutionHandle, ExecutionId, ExecutionResult, ExecutionState, OperationContext, OperationId,
    ProcessInput, ProcessInputStatus, ProcessOutputStream, ProcessRuntime, ProcessSignal,
    ReadExecutionRequest, ReadExecutionResult, ResizePtyRequest, SignalExecutionRequest,
    StartExecutionRequest, StdinMode, SupervisorGenerationId, TerminateExecutionRequest,
    TerminateExecutionResult, Validate, WriteId, WriteProcessInputRequest, WriteProcessInputResult,
};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    sync::{Mutex, RwLock, mpsc},
};
use tokio_util::sync::CancellationToken;

use super::journal::{EventJournal, remove_execution_directory};
use crate::{
    config::SupervisorLimits,
    error::{error, io_error, join_error, operation_conflict},
    path_resolver::PathResolver,
    platform,
};

const COMPLETION_NORMAL: u8 = 0;
const COMPLETION_TERMINATED: u8 = 1;
const COMPLETION_TIMED_OUT: u8 = 2;

#[derive(Clone)]
pub(crate) struct SupervisorProcessRuntime {
    inner: Arc<ManagerInner>,
}

struct ManagerInner {
    resolver: Arc<PathResolver>,
    generation_id: SupervisorGenerationId,
    executions_directory: PathBuf,
    limits: SupervisorLimits,
    sessions: RwLock<HashMap<ExecutionId, Arc<ProcessSession>>>,
    retired_execution_ids: RwLock<HashSet<ExecutionId>>,
    start_gate: Mutex<()>,
    active_processes: AtomicU32,
    shutdown: CancellationToken,
}

struct ProcessSession {
    request: StartExecutionRequest,
    directory: PathBuf,
    journal: EventJournal,
    control: Arc<ProcessControl>,
    input_records: Mutex<HashMap<WriteId, InputRecord>>,
    control_records: Mutex<HashMap<OperationId, ControlRecord>>,
    completion: AtomicU8,
}

struct InputRecord {
    fingerprint: String,
    status: ProcessInputStatus,
}

struct ControlRecord {
    fingerprint: String,
    result: ControlResult,
}

#[derive(Clone)]
enum ControlResult {
    Unit,
    Terminate(TerminateExecutionResult),
}

enum ProcessControl {
    Piped {
        pid: u32,
        child: Box<Mutex<Child>>,
        stdin: Mutex<Option<ChildStdin>>,
    },
    Pty {
        pid: u32,
        master: StdMutex<Option<Box<dyn MasterPty + Send>>>,
        writer: StdMutex<Option<Box<dyn Write + Send>>>,
        #[cfg(windows)]
        input_normalizer: StdMutex<WindowsPtyInputNormalizer>,
        killer: StdMutex<Box<dyn ChildKiller + Send + Sync>>,
    },
}

impl SupervisorProcessRuntime {
    pub async fn new(
        resolver: Arc<PathResolver>,
        generation_id: SupervisorGenerationId,
        state_directory: &Path,
        limits: SupervisorLimits,
    ) -> Result<Self, std::io::Error> {
        let executions_directory = state_directory.join("executions");
        tokio::fs::create_dir_all(&executions_directory).await?;
        let inner = Arc::new(ManagerInner {
            resolver,
            generation_id,
            executions_directory,
            limits,
            sessions: RwLock::new(HashMap::new()),
            retired_execution_ids: RwLock::new(HashSet::new()),
            start_gate: Mutex::new(()),
            active_processes: AtomicU32::new(0),
            shutdown: CancellationToken::new(),
        });
        start_cleanup_task(&inner);
        Ok(Self { inner })
    }

    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        let sessions: Vec<_> = self.inner.sessions.read().await.values().cloned().collect();
        for session in sessions {
            if session.journal.is_running().await {
                let _ = session.control.signal(ProcessSignal::Kill).await;
            }
        }
    }

    async fn entry(&self, execution_id: &ExecutionId) -> ExecutionResult<Arc<ProcessSession>> {
        self.inner
            .sessions
            .read()
            .await
            .get(execution_id)
            .cloned()
            .ok_or_else(|| {
                error(
                    ExecutionErrorCode::ExecutionNotFound,
                    format!("execution `{execution_id}` was not found"),
                )
            })
    }

    fn validate_generation(&self, generation_id: &SupervisorGenerationId) -> ExecutionResult<()> {
        if generation_id == &self.inner.generation_id {
            Ok(())
        } else {
            Err(error(
                ExecutionErrorCode::ExecutionLost,
                format!(
                    "execution belongs to supervisor generation `{generation_id}`, but the current generation is `{}`",
                    self.inner.generation_id
                ),
            ))
        }
    }

    async fn start_piped(
        &self,
        request: StartExecutionRequest,
        cwd: &Path,
        directory: PathBuf,
    ) -> ExecutionResult<ExecutionHandle> {
        let journal = EventJournal::create(directory.join("events.log")).await?;
        persist_start_request(&directory, &request).await?;

        let mut command = build_command(&request.command);
        command.current_dir(cwd);
        configure_environment(&mut command, &request.environment);
        platform::configure_process(&mut command);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        match request.stdin {
            StdinMode::Closed => {
                command.stdin(Stdio::null());
            }
            StdinMode::Pipe => {
                command.stdin(Stdio::piped());
            }
            StdinMode::Pty { .. } => unreachable!("PTY requests use start_pty"),
        }

        let mut child = command.spawn().map_err(|source| {
            error(
                ExecutionErrorCode::Io,
                format!("failed to start process: {source}"),
            )
        })?;
        let pid = child.id().ok_or_else(|| {
            error(
                ExecutionErrorCode::Internal,
                "spawned process did not report a process identifier",
            )
        })?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdin = child.stdin.take();
        let control = Arc::new(ProcessControl::Piped {
            pid,
            child: Box::new(Mutex::new(child)),
            stdin: Mutex::new(stdin),
        });
        let session = Arc::new(ProcessSession {
            request: request.clone(),
            directory,
            journal,
            control,
            input_records: Mutex::new(HashMap::new()),
            control_records: Mutex::new(HashMap::new()),
            completion: AtomicU8::new(COMPLETION_NORMAL),
        });
        self.install_session(&session).await?;

        let stdout_task = stdout.map(|reader| {
            tokio::spawn(read_pipe_output(
                Arc::clone(&session),
                reader,
                ProcessOutputStream::Stdout,
                self.inner.limits.process_output_chunk_bytes,
            ))
        });
        let stderr_task = stderr.map(|reader| {
            tokio::spawn(read_pipe_output(
                Arc::clone(&session),
                reader,
                ProcessOutputStream::Stderr,
                self.inner.limits.process_output_chunk_bytes,
            ))
        });
        spawn_piped_completion(
            Arc::clone(&self.inner),
            Arc::clone(&session),
            stdout_task,
            stderr_task,
        );
        self.spawn_timeout(&session);
        session.handle(&self.inner.generation_id).await
    }

    async fn start_pty(
        &self,
        request: StartExecutionRequest,
        cwd: &Path,
        directory: PathBuf,
        columns: u16,
        rows: u16,
    ) -> ExecutionResult<ExecutionHandle> {
        let journal = EventJournal::create(directory.join("events.log")).await?;
        persist_start_request(&directory, &request).await?;
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols: columns,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|source| {
                error(
                    ExecutionErrorCode::Io,
                    format!("failed to allocate PTY: {source}"),
                )
            })?;
        let reader = pair.master.try_clone_reader().map_err(|source| {
            error(
                ExecutionErrorCode::Io,
                format!("failed to open PTY output: {source}"),
            )
        })?;
        let writer = pair.master.take_writer().map_err(|source| {
            error(
                ExecutionErrorCode::Io,
                format!("failed to open PTY input: {source}"),
            )
        })?;
        let mut command = build_pty_command(&request.command);
        command.cwd(cwd);
        configure_pty_environment(&mut command, &request.environment);
        let mut child = pair.slave.spawn_command(command).map_err(|source| {
            error(
                ExecutionErrorCode::Io,
                format!("failed to start PTY process: {source}"),
            )
        })?;
        let pid = child.process_id().ok_or_else(|| {
            error(
                ExecutionErrorCode::Internal,
                "spawned PTY process did not report a process identifier",
            )
        })?;
        let killer = child.clone_killer();
        drop(pair.slave);

        let control = Arc::new(ProcessControl::Pty {
            pid,
            master: StdMutex::new(Some(pair.master)),
            writer: StdMutex::new(Some(writer)),
            #[cfg(windows)]
            input_normalizer: StdMutex::new(WindowsPtyInputNormalizer::default()),
            killer: StdMutex::new(killer),
        });
        let session = Arc::new(ProcessSession {
            request: request.clone(),
            directory,
            journal,
            control,
            input_records: Mutex::new(HashMap::new()),
            control_records: Mutex::new(HashMap::new()),
            completion: AtomicU8::new(COMPLETION_NORMAL),
        });
        self.install_session(&session).await?;

        let output_task = spawn_pty_output(
            Arc::clone(&session),
            reader,
            self.inner.limits.process_output_chunk_bytes,
        );
        let wait_task = tokio::task::spawn_blocking(move || child.wait());
        spawn_pty_completion(
            Arc::clone(&self.inner),
            Arc::clone(&session),
            output_task,
            wait_task,
        );
        self.spawn_timeout(&session);
        session.handle(&self.inner.generation_id).await
    }

    async fn install_session(&self, session: &Arc<ProcessSession>) -> ExecutionResult<()> {
        self.inner
            .sessions
            .write()
            .await
            .insert(session.request.execution_id.clone(), Arc::clone(session));
        self.inner.active_processes.fetch_add(1, Ordering::SeqCst);
        if let Err(source) = session.journal.mark_started().await {
            self.inner
                .sessions
                .write()
                .await
                .remove(&session.request.execution_id);
            self.inner.active_processes.fetch_sub(1, Ordering::SeqCst);
            let _ = session.control.signal(ProcessSignal::Kill).await;
            return Err(source);
        }
        Ok(())
    }

    fn spawn_timeout(&self, session: &Arc<ProcessSession>) {
        let Some(timeout_ms) = session.request.timeout_ms else {
            return;
        };
        let session = Arc::clone(session);
        let grace = self.inner.limits.termination_grace_period;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
            if !session.journal.is_running().await {
                return;
            }
            if session
                .completion
                .compare_exchange(
                    COMPLETION_NORMAL,
                    COMPLETION_TIMED_OUT,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_err()
            {
                return;
            }
            let _ = session.control.signal(ProcessSignal::Terminate).await;
            if !session.journal.wait_until_finished(grace).await {
                let _ = session.control.signal(ProcessSignal::Kill).await;
            }
        });
    }
}

#[async_trait]
impl ProcessRuntime for SupervisorProcessRuntime {
    async fn start(
        &self,
        context: &OperationContext,
        request: StartExecutionRequest,
    ) -> ExecutionResult<ExecutionHandle> {
        context.checkpoint()?;
        request.validate()?;
        let _gate = self.inner.start_gate.lock().await;
        if let Some(existing) = self.inner.sessions.read().await.get(&request.execution_id) {
            if existing.request != request {
                return Err(operation_conflict(format!(
                    "execution ID `{}` was reused with a different start request",
                    request.execution_id
                )));
            }
            return existing.handle(&self.inner.generation_id).await;
        }
        if self
            .inner
            .retired_execution_ids
            .read()
            .await
            .contains(&request.execution_id)
        {
            return Err(operation_conflict(format!(
                "execution ID `{}` was already used by this supervisor generation",
                request.execution_id
            )));
        }
        if self
            .inner
            .sessions
            .read()
            .await
            .values()
            .any(|session| session.request.operation_id == request.operation_id)
        {
            return Err(operation_conflict(format!(
                "start operation ID `{}` was reused for a different execution",
                request.operation_id
            )));
        }
        if self.inner.active_processes.load(Ordering::SeqCst)
            >= self.inner.limits.max_concurrent_processes
        {
            return Err(error(
                ExecutionErrorCode::ResourceExhausted,
                "the execution host has reached its concurrent process limit",
            ));
        }
        context.checkpoint()?;
        let cwd = self
            .inner
            .resolver
            .resolve_existing(&request.cwd, true)
            .await?;
        let metadata = tokio::fs::metadata(&cwd.path)
            .await
            .map_err(|source| io_error(&cwd.path, source))?;
        if !metadata.is_dir() {
            return Err(error(
                ExecutionErrorCode::NotDirectory,
                "process working directory is not a directory",
            ));
        }

        let directory = execution_directory(
            &self.inner.executions_directory,
            request.execution_id.as_str(),
        );
        if directory.exists() {
            tokio::fs::remove_dir_all(&directory)
                .await
                .map_err(|source| io_error(&directory, source))?;
        }
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|source| io_error(&directory, source))?;
        match request.stdin {
            StdinMode::Pty { columns, rows } => {
                self.start_pty(request, &cwd.path, directory, columns, rows)
                    .await
            }
            StdinMode::Closed | StdinMode::Pipe => {
                self.start_piped(request, &cwd.path, directory).await
            }
        }
    }

    async fn read(
        &self,
        context: &OperationContext,
        request: ReadExecutionRequest,
    ) -> ExecutionResult<ReadExecutionResult> {
        context.checkpoint()?;
        request.validate()?;
        self.validate_generation(&request.supervisor_generation_id)?;
        if request.max_bytes > self.inner.limits.max_process_read_bytes {
            return Err(error(
                ExecutionErrorCode::ResourceExhausted,
                format!(
                    "process read requested {} bytes but the host limit is {}",
                    request.max_bytes, self.inner.limits.max_process_read_bytes
                ),
            ));
        }
        let session = self.entry(&request.execution_id).await?;
        let requested_wait = request.wait_ms.map(Duration::from_millis);
        let wait = match (requested_wait, context.remaining()) {
            (Some(requested), Some(remaining)) => Some(requested.min(remaining)),
            (Some(requested), None) => Some(requested),
            (None, _) => None,
        };
        if wait.is_some() {
            tokio::select! {
                result = session.journal.read(request.after_sequence, request.max_bytes, wait) => result,
                () = context.cancelled() => Err(ExecutionError::cancelled()),
            }
        } else {
            session
                .journal
                .read(request.after_sequence, request.max_bytes, None)
                .await
        }
    }

    async fn write(
        &self,
        context: &OperationContext,
        request: WriteProcessInputRequest,
    ) -> ExecutionResult<WriteProcessInputResult> {
        context.checkpoint()?;
        self.validate_generation(&request.supervisor_generation_id)?;
        let input_size = match &request.input {
            ProcessInput::Data { data } => u64::try_from(data.len()).unwrap_or(u64::MAX),
            ProcessInput::Close => 0,
        };
        if input_size > self.inner.limits.max_process_input_bytes {
            return Err(error(
                ExecutionErrorCode::ResourceExhausted,
                format!(
                    "process input contains {input_size} bytes but the host limit is {}",
                    self.inner.limits.max_process_input_bytes
                ),
            ));
        }
        let session = self.entry(&request.execution_id).await?;
        let fingerprint = fingerprint(&request)?;
        let mut records = session.input_records.lock().await;
        if let Some(record) = records.get(&request.write_id) {
            if record.fingerprint != fingerprint {
                return Err(operation_conflict(format!(
                    "write ID `{}` was reused with different process input",
                    request.write_id
                )));
            }
            return Ok(WriteProcessInputResult {
                status: if record.status == ProcessInputStatus::Accepted {
                    ProcessInputStatus::AlreadyAccepted
                } else {
                    record.status
                },
            });
        }
        if !session.journal.is_running().await {
            return Ok(WriteProcessInputResult {
                status: ProcessInputStatus::StdinClosed,
            });
        }
        let result = session.control.write(request.input).await?;
        records.insert(
            request.write_id,
            InputRecord {
                fingerprint,
                status: result.status,
            },
        );
        Ok(result)
    }

    async fn resize(
        &self,
        context: &OperationContext,
        request: ResizePtyRequest,
    ) -> ExecutionResult<()> {
        context.checkpoint()?;
        request.validate()?;
        self.validate_generation(&request.supervisor_generation_id)?;
        let session = self.entry(&request.execution_id).await?;
        let fingerprint = fingerprint(&request)?;
        let mut records = session.control_records.lock().await;
        if let Some(record) = records.get(&request.operation_id) {
            ensure_control_record(record, &fingerprint, &request.operation_id)?;
            return match &record.result {
                ControlResult::Unit => Ok(()),
                ControlResult::Terminate(_) => Err(operation_conflict(
                    "operation ID was already used for termination",
                )),
            };
        }
        if !session.journal.is_running().await {
            return Err(error(
                ExecutionErrorCode::ExecutionNotFound,
                "cannot resize a completed execution",
            ));
        }
        session
            .control
            .resize(request.columns, request.rows)
            .await?;
        records.insert(
            request.operation_id,
            ControlRecord {
                fingerprint,
                result: ControlResult::Unit,
            },
        );
        Ok(())
    }

    async fn signal(
        &self,
        context: &OperationContext,
        request: SignalExecutionRequest,
    ) -> ExecutionResult<()> {
        context.checkpoint()?;
        self.validate_generation(&request.supervisor_generation_id)?;
        let session = self.entry(&request.execution_id).await?;
        let fingerprint = fingerprint(&request)?;
        let mut records = session.control_records.lock().await;
        if let Some(record) = records.get(&request.operation_id) {
            ensure_control_record(record, &fingerprint, &request.operation_id)?;
            return match &record.result {
                ControlResult::Unit => Ok(()),
                ControlResult::Terminate(_) => Err(operation_conflict(
                    "operation ID was already used for termination",
                )),
            };
        }
        if !session.journal.is_running().await {
            return Err(error(
                ExecutionErrorCode::ExecutionNotFound,
                "cannot signal a completed execution",
            ));
        }
        session.control.signal(request.signal).await?;
        records.insert(
            request.operation_id,
            ControlRecord {
                fingerprint,
                result: ControlResult::Unit,
            },
        );
        Ok(())
    }

    async fn terminate(
        &self,
        context: &OperationContext,
        request: TerminateExecutionRequest,
    ) -> ExecutionResult<TerminateExecutionResult> {
        context.checkpoint()?;
        self.validate_generation(&request.supervisor_generation_id)?;
        let session = self.entry(&request.execution_id).await?;
        let fingerprint = fingerprint(&request)?;
        let mut records = session.control_records.lock().await;
        if let Some(record) = records.get(&request.operation_id) {
            ensure_control_record(record, &fingerprint, &request.operation_id)?;
            return match &record.result {
                ControlResult::Terminate(result) => Ok(result.clone()),
                ControlResult::Unit => Err(operation_conflict(
                    "operation ID was already used for another process-control operation",
                )),
            };
        }
        let was_running = session.journal.is_running().await;
        if was_running {
            let _ = session.completion.compare_exchange(
                COMPLETION_NORMAL,
                COMPLETION_TERMINATED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
            let terminate_error = session.control.signal(ProcessSignal::Terminate).await.err();
            let grace = self.inner.limits.termination_grace_period;
            if !session.journal.wait_until_finished(grace).await {
                if let Err(kill_error) = session.control.signal(ProcessSignal::Kill).await {
                    return Err(terminate_error.unwrap_or(kill_error));
                }
            }
        }
        let result = TerminateExecutionResult { was_running };
        records.insert(
            request.operation_id,
            ControlRecord {
                fingerprint,
                result: ControlResult::Terminate(result.clone()),
            },
        );
        Ok(result)
    }
}

impl ProcessSession {
    async fn handle(
        &self,
        generation_id: &SupervisorGenerationId,
    ) -> ExecutionResult<ExecutionHandle> {
        let (state, started_at) = self.journal.handle().await;
        Ok(ExecutionHandle {
            execution_id: self.request.execution_id.clone(),
            supervisor_generation_id: generation_id.clone(),
            state,
            started_at,
        })
    }
}

impl ProcessControl {
    fn pid(&self) -> u32 {
        match self {
            Self::Piped { pid, .. } | Self::Pty { pid, .. } => *pid,
        }
    }

    async fn write(
        self: &Arc<Self>,
        input: ProcessInput,
    ) -> ExecutionResult<WriteProcessInputResult> {
        match self.as_ref() {
            Self::Piped { stdin, .. } => {
                let mut stdin = stdin.lock().await;
                match input {
                    ProcessInput::Data { data } => {
                        let Some(writer) = stdin.as_mut() else {
                            return Ok(WriteProcessInputResult {
                                status: ProcessInputStatus::StdinClosed,
                            });
                        };
                        writer.write_all(data.as_slice()).await.map_err(|source| {
                            error(
                                ExecutionErrorCode::Io,
                                format!("failed to write process input: {source}"),
                            )
                        })?;
                        writer.flush().await.map_err(|source| {
                            error(
                                ExecutionErrorCode::Io,
                                format!("failed to flush process input: {source}"),
                            )
                        })?;
                    }
                    ProcessInput::Close => {
                        stdin.take();
                    }
                }
            }
            Self::Pty { .. } => {
                let control = Arc::clone(self);
                return tokio::task::spawn_blocking(move || control.write_pty(input))
                    .await
                    .map_err(|source| join_error("PTY input task failed", source))?;
            }
        }
        Ok(WriteProcessInputResult {
            status: ProcessInputStatus::Accepted,
        })
    }

    fn write_pty(&self, input: ProcessInput) -> ExecutionResult<WriteProcessInputResult> {
        let Self::Pty {
            writer,
            #[cfg(windows)]
            input_normalizer,
            ..
        } = self
        else {
            return Err(error(
                ExecutionErrorCode::Internal,
                "PTY write used with a piped process",
            ));
        };
        let mut writer = writer
            .lock()
            .map_err(|_| error(ExecutionErrorCode::Internal, "PTY input lock is poisoned"))?;
        match input {
            ProcessInput::Data { data } => {
                let Some(writer) = writer.as_mut() else {
                    return Ok(WriteProcessInputResult {
                        status: ProcessInputStatus::StdinClosed,
                    });
                };
                #[cfg(windows)]
                let data = input_normalizer
                    .lock()
                    .map_err(|_| {
                        error(
                            ExecutionErrorCode::Internal,
                            "PTY input normalizer lock is poisoned",
                        )
                    })?
                    .normalize(data.as_slice());
                writer.write_all(data.as_slice()).map_err(|source| {
                    error(
                        ExecutionErrorCode::Io,
                        format!("failed to write PTY input: {source}"),
                    )
                })?;
                writer.flush().map_err(|source| {
                    error(
                        ExecutionErrorCode::Io,
                        format!("failed to flush PTY input: {source}"),
                    )
                })?;
            }
            ProcessInput::Close => {
                writer.take();
            }
        }
        Ok(WriteProcessInputResult {
            status: ProcessInputStatus::Accepted,
        })
    }

    async fn resize(self: &Arc<Self>, columns: u16, rows: u16) -> ExecutionResult<()> {
        let control = Arc::clone(self);
        tokio::task::spawn_blocking(move || match control.as_ref() {
            Self::Pty { master, .. } => master
                .lock()
                .map_err(|_| error(ExecutionErrorCode::Internal, "PTY resize lock is poisoned"))?
                .as_ref()
                .ok_or_else(|| {
                    error(
                        ExecutionErrorCode::ExecutionNotFound,
                        "cannot resize a completed PTY execution",
                    )
                })?
                .resize(PtySize {
                    rows,
                    cols: columns,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|source| {
                    error(
                        ExecutionErrorCode::Io,
                        format!("failed to resize PTY: {source}"),
                    )
                }),
            Self::Piped { .. } => Err(error(
                ExecutionErrorCode::Unsupported,
                "cannot resize a process without a PTY",
            )),
        })
        .await
        .map_err(|source| join_error("PTY resize task failed", source))?
    }

    async fn signal(&self, signal: ProcessSignal) -> ExecutionResult<()> {
        match self {
            Self::Piped { pid, child, .. } => {
                platform::signal_piped_process(*pid, &mut *child.lock().await, signal)
            }
            Self::Pty { .. } => match platform::signal_process_group(self.pid(), signal) {
                Ok(()) => Ok(()),
                Err(source) if platform::pty_signal_uses_kill_fallback(signal) => {
                    self.kill_fallback(source)
                }
                Err(source) => Err(source),
            },
        }
    }

    fn kill_fallback(&self, original: ExecutionError) -> ExecutionResult<()> {
        match self {
            Self::Pty { killer, .. } => killer
                .lock()
                .map_err(|_| error(ExecutionErrorCode::Internal, "PTY killer lock is poisoned"))?
                .kill()
                .map_err(|source| {
                    error(
                        ExecutionErrorCode::Io,
                        format!("failed to kill PTY process after {original}: {source}"),
                    )
                }),
            Self::Piped { .. } => Err(original),
        }
    }

    fn close_pty_after_child_exit(&self) -> ExecutionResult<()> {
        let Self::Pty { master, writer, .. } = self else {
            return Ok(());
        };
        let writer = writer
            .lock()
            .map_err(|_| error(ExecutionErrorCode::Internal, "PTY input lock is poisoned"))?
            .take();
        drop(writer);
        let master = master
            .lock()
            .map_err(|_| error(ExecutionErrorCode::Internal, "PTY master lock is poisoned"))?
            .take();
        drop(master);
        Ok(())
    }
}

fn spawn_piped_completion(
    manager: Arc<ManagerInner>,
    session: Arc<ProcessSession>,
    stdout_task: Option<tokio::task::JoinHandle<ExecutionResult<()>>>,
    stderr_task: Option<tokio::task::JoinHandle<ExecutionResult<()>>>,
) {
    tokio::spawn(async move {
        let exit = wait_for_piped_child(&session).await;
        let stdout = join_output_task(stdout_task, "stdout reader task failed").await;
        let stderr = join_output_task(stderr_task, "stderr reader task failed").await;
        complete_session(&manager, &session, exit, [stdout, stderr]).await;
    });
}

fn spawn_pty_completion(
    manager: Arc<ManagerInner>,
    session: Arc<ProcessSession>,
    output_task: tokio::task::JoinHandle<ExecutionResult<()>>,
    wait_task: tokio::task::JoinHandle<std::io::Result<portable_pty::ExitStatus>>,
) {
    tokio::spawn(async move {
        let exit = match wait_task.await {
            Ok(Ok(status)) => Ok(i32::try_from(status.exit_code()).unwrap_or(-1)),
            Ok(Err(source)) => Err(error(
                ExecutionErrorCode::Io,
                format!("failed to wait for PTY process: {source}"),
            )),
            Err(source) => Err(join_error("PTY wait task failed", source)),
        };
        let close = session.control.close_pty_after_child_exit();
        let output = output_task
            .await
            .map_err(|source| join_error("PTY output task failed", source))
            .and_then(|result| result);
        complete_session(&manager, &session, exit, [close, output]).await;
    });
}

async fn complete_session<const N: usize>(
    manager: &ManagerInner,
    session: &ProcessSession,
    exit: ExecutionResult<i32>,
    outputs: [ExecutionResult<()>; N],
) {
    let failure = outputs.into_iter().find_map(Result::err);
    let result = match (exit, failure) {
        (Err(source), _) | (_, Some(source)) => {
            session.journal.finish_failed(source.to_string()).await
        }
        (Ok(exit_code), None) => {
            let final_state = match session.completion.load(Ordering::SeqCst) {
                COMPLETION_TERMINATED => ExecutionState::Cancelled,
                COMPLETION_TIMED_OUT => ExecutionState::Failed,
                _ => ExecutionState::Exited,
            };
            session.journal.finish_exited(exit_code, final_state).await
        }
    };
    if result.is_err() {
        let _ = session
            .journal
            .finish_failed("failed to finalize process journal".to_string())
            .await;
    }
    manager.active_processes.fetch_sub(1, Ordering::SeqCst);
}

async fn wait_for_piped_child(session: &ProcessSession) -> ExecutionResult<i32> {
    let ProcessControl::Piped { child, .. } = session.control.as_ref() else {
        return Err(error(
            ExecutionErrorCode::Internal,
            "piped wait used with a PTY process",
        ));
    };
    loop {
        let status = child.lock().await.try_wait().map_err(|source| {
            error(
                ExecutionErrorCode::Io,
                format!("failed to inspect process: {source}"),
            )
        })?;
        if let Some(status) = status {
            return Ok(status.code().unwrap_or(-1));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn join_output_task(
    task: Option<tokio::task::JoinHandle<ExecutionResult<()>>>,
    message: &str,
) -> ExecutionResult<()> {
    match task {
        Some(task) => task
            .await
            .map_err(|source| join_error(message, source))
            .and_then(|result| result),
        None => Ok(()),
    }
}

async fn read_pipe_output<R>(
    session: Arc<ProcessSession>,
    mut reader: R,
    stream: ProcessOutputStream,
    chunk_size: usize,
) -> ExecutionResult<()>
where
    R: AsyncRead + Unpin,
{
    let mut buffer = vec![0_u8; chunk_size];
    loop {
        let read = reader.read(&mut buffer).await.map_err(|source| {
            error(
                ExecutionErrorCode::Io,
                format!("failed to read process output: {source}"),
            )
        })?;
        if read == 0 {
            return Ok(());
        }
        session
            .journal
            .append_output(stream, buffer[..read].to_vec())
            .await?;
    }
}

fn spawn_pty_output(
    session: Arc<ProcessSession>,
    mut reader: Box<dyn Read + Send>,
    chunk_size: usize,
) -> tokio::task::JoinHandle<ExecutionResult<()>> {
    let (sender, mut receiver) = mpsc::channel::<Result<Vec<u8>, std::io::Error>>(16);
    #[cfg(windows)]
    let control = Arc::clone(&session.control);
    let reader_task = tokio::task::spawn_blocking(move || {
        let mut buffer = vec![0_u8; chunk_size];
        #[cfg(windows)]
        let mut terminal_responder = WindowsTerminalResponder::default();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    #[cfg(windows)]
                    for response in terminal_responder.observe(&buffer[..read]) {
                        if let Err(source) = control.write_pty_terminal_response(response) {
                            let _ = sender.blocking_send(Err(source));
                            return;
                        }
                    }
                    if sender.blocking_send(Ok(buffer[..read].to_vec())).is_err() {
                        break;
                    }
                }
                Err(source) if source.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(source) => {
                    let _ = sender.blocking_send(Err(source));
                    break;
                }
            }
        }
    });
    tokio::spawn(async move {
        while let Some(chunk) = receiver.recv().await {
            let chunk = chunk.map_err(|source| {
                error(
                    ExecutionErrorCode::Io,
                    format!("failed to read PTY output: {source}"),
                )
            })?;
            session
                .journal
                .append_output(ProcessOutputStream::Pty, chunk)
                .await?;
        }
        reader_task
            .await
            .map_err(|source| join_error("PTY reader task failed", source))?;
        Ok(())
    })
}

#[cfg(windows)]
impl ProcessControl {
    fn write_pty_terminal_response(&self, response: &[u8]) -> std::io::Result<()> {
        let Self::Pty { writer, .. } = self else {
            return Err(std::io::Error::other(
                "terminal response used with a piped process",
            ));
        };
        let mut writer = writer
            .lock()
            .map_err(|_| std::io::Error::other("PTY input lock is poisoned"))?;
        let writer = writer.as_mut().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "PTY input is closed")
        })?;
        writer.write_all(response)?;
        writer.flush()
    }
}

#[cfg(any(windows, test))]
#[derive(Default)]
struct WindowsTerminalResponder {
    matched_cursor_query_bytes: usize,
}

#[cfg(any(windows, test))]
impl WindowsTerminalResponder {
    const CURSOR_POSITION_QUERY: &'static [u8] = b"\x1b[6n";
    const CURSOR_POSITION_RESPONSE: &'static [u8] = b"\x1b[1;1R";

    fn observe(&mut self, bytes: &[u8]) -> Vec<&'static [u8]> {
        let mut responses = Vec::new();
        for &byte in bytes {
            if byte == Self::CURSOR_POSITION_QUERY[self.matched_cursor_query_bytes] {
                self.matched_cursor_query_bytes += 1;
                if self.matched_cursor_query_bytes == Self::CURSOR_POSITION_QUERY.len() {
                    responses.push(Self::CURSOR_POSITION_RESPONSE);
                    self.matched_cursor_query_bytes = 0;
                }
            } else {
                self.matched_cursor_query_bytes =
                    usize::from(byte == Self::CURSOR_POSITION_QUERY[0]);
            }
        }
        responses
    }
}

/// Converts terminal keystroke bytes into the form expected by ConPTY.
///
/// Enter is a carriage return on Windows. Existing CRLF sequences are
/// collapsed even when split between API writes, and Backspace is represented
/// by DEL so ConPTY translates it to `VK_BACK`. Other bytes pass through.
#[cfg(any(windows, test))]
#[derive(Default)]
struct WindowsPtyInputNormalizer {
    previous_was_cr: bool,
}

#[cfg(any(windows, test))]
impl WindowsPtyInputNormalizer {
    fn normalize(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut normalized = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            match byte {
                b'\x08' => normalized.push(b'\x7f'),
                b'\n' => {
                    if !self.previous_was_cr {
                        normalized.push(b'\r');
                    }
                }
                _ => normalized.push(byte),
            }
            self.previous_was_cr = byte == b'\r';
        }
        normalized
    }
}

fn build_command(spec: &CommandSpec) -> Command {
    match spec {
        CommandSpec::Argv { program, arguments } => {
            let mut command = Command::new(program);
            command.args(arguments);
            command
        }
        CommandSpec::Shell {
            command,
            shell,
            login,
        } => {
            let mut process = Command::new(shell.clone().unwrap_or_else(platform::default_shell));
            if cfg!(windows) {
                process.arg("/C").arg(command);
            } else {
                process.arg(if *login { "-lc" } else { "-c" }).arg(command);
            }
            process
        }
    }
}

fn build_pty_command(spec: &CommandSpec) -> CommandBuilder {
    match spec {
        CommandSpec::Argv { program, arguments } => {
            let mut command = CommandBuilder::new(program);
            command.args(arguments);
            command
        }
        CommandSpec::Shell {
            command,
            shell,
            login,
        } => {
            let mut process =
                CommandBuilder::new(shell.clone().unwrap_or_else(platform::default_shell));
            if cfg!(windows) {
                process.arg("/C");
                process.arg(command);
            } else {
                process.arg(if *login { "-lc" } else { "-c" });
                process.arg(command);
            }
            process
        }
    }
}

fn configure_environment(command: &mut Command, environment: &EnvironmentVariables) {
    if environment.mode == EnvironmentMode::Empty {
        command.env_clear();
    }
    for name in &environment.remove {
        command.env_remove(name);
    }
    for (name, value) in &environment.set {
        command.env(name, value);
    }
}

fn configure_pty_environment(command: &mut CommandBuilder, environment: &EnvironmentVariables) {
    if environment.mode == EnvironmentMode::Empty {
        command.env_clear();
    }
    for name in &environment.remove {
        command.env_remove(name);
    }
    for (name, value) in &environment.set {
        command.env(name, value);
    }
}

async fn persist_start_request(
    directory: &Path,
    request: &StartExecutionRequest,
) -> ExecutionResult<()> {
    let path = directory.join("metadata.json");
    let encoded = serde_json::to_vec_pretty(request).map_err(|source| {
        error(
            ExecutionErrorCode::Internal,
            format!("failed to encode process metadata: {source}"),
        )
    })?;
    tokio::fs::write(&path, encoded)
        .await
        .map_err(|source| io_error(&path, source))
}

fn fingerprint(value: &impl Serialize) -> ExecutionResult<String> {
    let encoded = serde_json::to_vec(value).map_err(|source| {
        error(
            ExecutionErrorCode::Internal,
            format!("failed to fingerprint process operation: {source}"),
        )
    })?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn ensure_control_record(
    record: &ControlRecord,
    fingerprint: &str,
    operation_id: &OperationId,
) -> ExecutionResult<()> {
    if record.fingerprint == fingerprint {
        Ok(())
    } else {
        Err(operation_conflict(format!(
            "operation ID `{operation_id}` was reused with a different process-control request"
        )))
    }
}

fn execution_directory(parent: &Path, execution_id: &str) -> PathBuf {
    parent.join(format!("{:x}", Sha256::digest(execution_id.as_bytes())))
}

fn start_cleanup_task(manager: &Arc<ManagerInner>) {
    let weak = Arc::downgrade(manager);
    let retention = manager.limits.completed_execution_retention;
    let interval = retention
        .min(Duration::from_secs(60))
        .max(Duration::from_millis(100));
    tokio::spawn(async move {
        loop {
            let Some(manager) = Weak::upgrade(&weak) else {
                break;
            };
            tokio::select! {
                () = manager.shutdown.cancelled() => break,
                () = tokio::time::sleep(interval) => {}
            }
            cleanup_completed(&manager, retention).await;
        }
    });
}

async fn cleanup_completed(manager: &ManagerInner, retention: Duration) {
    let sessions: Vec<_> = manager.sessions.read().await.values().cloned().collect();
    for session in sessions {
        if session
            .journal
            .finished_for()
            .await
            .is_some_and(|elapsed| elapsed >= retention)
        {
            manager
                .retired_execution_ids
                .write()
                .await
                .insert(session.request.execution_id.clone());
            let removed = manager
                .sessions
                .write()
                .await
                .remove(&session.request.execution_id)
                .is_some();
            if removed {
                remove_execution_directory(&session.directory).await;
            }
        }
    }
}

#[cfg(test)]
mod terminal_responder_tests {
    use super::{WindowsPtyInputNormalizer, WindowsTerminalResponder};

    #[test]
    fn recognizes_cursor_position_query() {
        let mut responder = WindowsTerminalResponder::default();
        assert_eq!(
            responder.observe(b"before\x1b[6nafter"),
            vec![b"\x1b[1;1R".as_slice()]
        );
    }

    #[test]
    fn recognizes_query_split_across_output_chunks() {
        let mut responder = WindowsTerminalResponder::default();
        assert!(responder.observe(b"\x1b[").is_empty());
        assert_eq!(responder.observe(b"6n"), vec![b"\x1b[1;1R".as_slice()]);
    }

    #[test]
    fn recognizes_multiple_queries_and_ignores_near_matches() {
        let mut responder = WindowsTerminalResponder::default();
        assert_eq!(
            responder.observe(b"\x1b[5n\x1b[6ntext\x1b[6n"),
            vec![b"\x1b[1;1R".as_slice(), b"\x1b[1;1R".as_slice()]
        );
    }

    #[test]
    fn normalizes_windows_terminal_input_across_writes() {
        let mut normalizer = WindowsPtyInputNormalizer::default();
        assert_eq!(normalizer.normalize(b"first\n"), b"first\r");
        assert_eq!(normalizer.normalize(b"second\r"), b"second\r");
        assert_eq!(normalizer.normalize(b"\nthird\r\n"), b"third\r");

        let mut input = "cafeé 漢字".as_bytes().to_vec();
        input.extend_from_slice(b"\x08\x03");
        let mut expected = "cafeé 漢字".as_bytes().to_vec();
        expected.extend_from_slice(b"\x7f\x03");
        assert_eq!(normalizer.normalize(&input), expected);
    }
}
