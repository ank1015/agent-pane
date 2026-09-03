//! Reusable black-box behavioral checks for execution runtimes.
//!
//! The suite only uses [`execution_core::ExecutionRuntime`]. Provider and
//! transport implementations can therefore run exactly the same checks as the
//! in-process supervisor implementation.

use std::{collections::BTreeMap, fmt, time::Duration};

use execution_core::{
    BinaryData, CommandSpec, EnvironmentMode, EnvironmentVariables, ExecutionError,
    ExecutionErrorCode, ExecutionHandle, ExecutionId, ExecutionPath, ExecutionRuntime,
    ExecutionState, FileKind, FileRevision, ListDirectoryRequest, OperationContext, OperationId,
    PathConvention, ProcessEvent, ProcessEventKind, ProcessInput, ProcessInputStatus,
    ProcessOutputStream, ProcessSignal, ReadExecutionRequest, ReadExecutionResult, ReadFileRequest,
    RemovePathRequest, ResizePtyRequest, RootId, SignalExecutionRequest, StartExecutionRequest,
    StatRequest, StdinMode, SupervisorGenerationId, TerminateExecutionRequest, WriteCondition,
    WriteFileRequest, WriteId, WriteProcessInputRequest,
};
use thiserror::Error;

const DEFAULT_OPERATION_TIMEOUT: Duration = Duration::from_secs(20);
const PROCESS_READ_BYTES: u64 = 1024 * 1024;
const PROCESS_WAIT_MS: u64 = 1_000;

/// Runtime-specific inputs required by the black-box suite.
#[derive(Clone, Debug)]
pub struct ConformanceConfig {
    pub root_id: RootId,
    pub shell_program: String,
    pub shell_arguments: Vec<String>,
    pub operation_timeout: Duration,
    pub command_profile: ConformanceCommandProfile,
}

/// Native command syntax used by process conformance checks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConformanceCommandProfile {
    Posix,
    WindowsPowerShell,
}

impl ConformanceConfig {
    /// Builds the native configuration from the first writable root.
    pub fn for_runtime(runtime: &dyn ExecutionRuntime) -> Result<Self, ConformanceError> {
        let descriptor = runtime.descriptor();
        let root = descriptor
            .roots
            .iter()
            .find(|root| !root.read_only)
            .ok_or_else(|| {
                ConformanceError::configuration("runtime does not expose a writable root")
            })?;
        match descriptor.path_convention {
            PathConvention::Unix => Ok(Self::unix(root.id.clone())),
            PathConvention::Windows => Ok(Self::windows(root.id.clone())),
        }
    }

    /// Constructs a configuration for a known root and the default Unix shell.
    #[must_use]
    pub fn unix(root_id: RootId) -> Self {
        Self {
            root_id,
            shell_program: "/bin/sh".to_string(),
            shell_arguments: vec!["-c".to_string()],
            operation_timeout: DEFAULT_OPERATION_TIMEOUT,
            command_profile: ConformanceCommandProfile::Posix,
        }
    }

    /// Constructs a configuration for a Windows host using Windows PowerShell.
    #[must_use]
    pub fn windows(root_id: RootId) -> Self {
        Self {
            root_id,
            shell_program: "powershell.exe".to_string(),
            shell_arguments: vec![
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
            ],
            operation_timeout: DEFAULT_OPERATION_TIMEOUT,
            command_profile: ConformanceCommandProfile::WindowsPowerShell,
        }
    }

    /// Overrides the command used to execute POSIX-compatible test scripts.
    #[must_use]
    pub fn with_shell(
        mut self,
        program: impl Into<String>,
        arguments_before_script: Vec<String>,
    ) -> Self {
        self.shell_program = program.into();
        self.shell_arguments = arguments_before_script;
        self
    }

    /// Overrides the deadline applied to each conformance operation group.
    #[must_use]
    pub fn with_operation_timeout(mut self, timeout: Duration) -> Self {
        self.operation_timeout = timeout;
        self
    }

    fn command(&self, program: ConformanceProgram) -> CommandSpec {
        let mut arguments = self.shell_arguments.clone();
        arguments.push(program.script(self.command_profile).to_string());
        CommandSpec::Argv {
            program: self.shell_program.clone(),
            arguments,
        }
    }
}

#[derive(Clone, Copy)]
enum ConformanceProgram {
    OutputAndEnvironment,
    DifferentOutput,
    ReadInput,
    BoundedOutput,
    PtyInputAndSize,
    Sleep,
}

impl ConformanceProgram {
    fn script(self, profile: ConformanceCommandProfile) -> &'static str {
        match (profile, self) {
            (ConformanceCommandProfile::Posix, Self::OutputAndEnvironment) => {
                "printf '%s' \"$EXECUTION_CONFORMANCE_VALUE\"; printf 'stderr' >&2"
            }
            (ConformanceCommandProfile::Posix, Self::DifferentOutput) => "printf 'different'",
            (ConformanceCommandProfile::Posix, Self::ReadInput) => {
                "IFS= read -r line; printf '<%s>' \"$line\""
            }
            (ConformanceCommandProfile::Posix, Self::BoundedOutput) => "printf 'abcd'",
            (ConformanceCommandProfile::Posix, Self::PtyInputAndSize) => {
                "IFS= read -r line; stty size; printf '<%s>' \"$line\""
            }
            (ConformanceCommandProfile::Posix, Self::Sleep) => "sleep 10",
            (ConformanceCommandProfile::WindowsPowerShell, Self::OutputAndEnvironment) => {
                "[Console]::Out.Write($env:EXECUTION_CONFORMANCE_VALUE); [Console]::Error.Write('stderr')"
            }
            (ConformanceCommandProfile::WindowsPowerShell, Self::DifferentOutput) => {
                "[Console]::Out.Write('different')"
            }
            (ConformanceCommandProfile::WindowsPowerShell, Self::ReadInput) => {
                "$line = [Console]::In.ReadLine(); [Console]::Out.Write('<' + $line + '>')"
            }
            (ConformanceCommandProfile::WindowsPowerShell, Self::BoundedOutput) => {
                "[Console]::Out.Write('abcd')"
            }
            (ConformanceCommandProfile::WindowsPowerShell, Self::PtyInputAndSize) => {
                "$line = [Console]::In.ReadLine(); $size = $Host.UI.RawUI.WindowSize; [Console]::Out.Write(('{0} {1} <{2}>' -f $size.Height, $size.Width, $line))"
            }
            (ConformanceCommandProfile::WindowsPowerShell, Self::Sleep) => {
                "Start-Sleep -Seconds 10"
            }
        }
    }
}

/// Successful checks and feature-dependent checks skipped by a runtime.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConformanceReport {
    passed: Vec<&'static str>,
    skipped: Vec<SkippedCheck>,
}

impl ConformanceReport {
    #[must_use]
    pub fn passed_checks(&self) -> &[&'static str] {
        &self.passed
    }

    #[must_use]
    pub fn skipped_checks(&self) -> &[SkippedCheck] {
        &self.skipped
    }

    fn pass(&mut self, name: &'static str) {
        self.passed.push(name);
    }

    fn skip(&mut self, name: &'static str, reason: &'static str) {
        self.skipped.push(SkippedCheck { name, reason });
    }
}

/// A check skipped because the runtime does not advertise the required feature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkippedCheck {
    pub name: &'static str,
    pub reason: &'static str,
}

/// Failure of a named conformance check.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("execution conformance check `{check}` failed: {message}")]
pub struct ConformanceError {
    pub check: &'static str,
    pub message: String,
}

impl ConformanceError {
    fn configuration(message: impl Into<String>) -> Self {
        Self {
            check: "configuration",
            message: message.into(),
        }
    }

    fn check(check: &'static str, message: impl Into<String>) -> Self {
        Self {
            check,
            message: message.into(),
        }
    }

    fn runtime(check: &'static str, error: ExecutionError) -> Self {
        Self::check(
            check,
            format!(
                "runtime returned {:?} (retryable={}): {}",
                error.code, error.retryable, error.message
            ),
        )
    }
}

/// Runs the complete provider-neutral suite.
pub async fn run_all(
    runtime: &dyn ExecutionRuntime,
    config: &ConformanceConfig,
) -> Result<ConformanceReport, ConformanceError> {
    let mut report = ConformanceReport::default();
    check_descriptor(runtime, config, &mut report)?;
    run_filesystem(runtime, config, &mut report).await?;
    run_processes(runtime, config, &mut report).await?;
    Ok(report)
}

fn check_descriptor(
    runtime: &dyn ExecutionRuntime,
    config: &ConformanceConfig,
    report: &mut ConformanceReport,
) -> Result<(), ConformanceError> {
    const CHECK: &str = "descriptor";
    let descriptor = runtime.descriptor();
    if descriptor.supervisor_generation_id.as_str().is_empty() {
        return Err(ConformanceError::check(
            CHECK,
            "supervisor generation ID is empty",
        ));
    }
    let Some(root) = descriptor
        .roots
        .iter()
        .find(|root| root.id == config.root_id)
    else {
        return Err(ConformanceError::check(
            CHECK,
            format!("configured root `{}` is not advertised", config.root_id),
        ));
    };
    if root.read_only {
        return Err(ConformanceError::check(
            CHECK,
            format!("configured root `{}` is read-only", config.root_id),
        ));
    }
    if !descriptor.features.file_revisions {
        return Err(ConformanceError::check(
            CHECK,
            "runtime does not advertise required file revisions",
        ));
    }
    report.pass(CHECK);
    Ok(())
}

async fn run_filesystem(
    runtime: &dyn ExecutionRuntime,
    config: &ConformanceConfig,
    report: &mut ConformanceReport,
) -> Result<(), ConformanceError> {
    let namespace = format!(".execution-conformance-{}", ExecutionId::generate());
    let result = run_filesystem_inner(runtime, config, report, &namespace).await;
    let cleanup = runtime
        .filesystem()
        .remove(
            &OperationContext::with_timeout(config.operation_timeout),
            RemovePathRequest {
                operation_id: OperationId::generate(),
                path: path(config, &namespace)?,
                recursive: true,
                ignore_missing: true,
                expected_revision: None,
            },
        )
        .await;

    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(ConformanceError::runtime("filesystem_cleanup", error)),
        (Ok(()), Ok(_)) => {
            report.pass("filesystem_cleanup");
            Ok(())
        }
    }
}

async fn run_filesystem_inner(
    runtime: &dyn ExecutionRuntime,
    config: &ConformanceConfig,
    report: &mut ConformanceReport,
    namespace: &str,
) -> Result<(), ConformanceError> {
    const ROUNDTRIP: &str = "filesystem_roundtrip";
    const CONDITIONS: &str = "filesystem_conditions_and_idempotency";
    const LISTING: &str = "filesystem_listing_and_remove";

    let fs = runtime.filesystem();
    let context = || OperationContext::with_timeout(config.operation_timeout);
    let directory = path(config, namespace)?;
    fs.create_directory(
        &context(),
        execution_core::CreateDirectoryRequest {
            operation_id: OperationId::generate(),
            path: directory.clone(),
            recursive: true,
        },
    )
    .await
    .map_err(|error| ConformanceError::runtime(ROUNDTRIP, error))?;

    let file = path(config, &format!("{namespace}/data.bin"))?;
    let original = b"abc\0defghi".to_vec();
    let operation_id = OperationId::generate();
    let write = WriteFileRequest {
        operation_id: operation_id.clone(),
        path: file.clone(),
        data: BinaryData::new(original.clone()),
        condition: WriteCondition::MustNotExist,
        create_parents: true,
        follow_symlinks: true,
    };
    let written = fs
        .write(&context(), write.clone())
        .await
        .map_err(|error| ConformanceError::runtime(ROUNDTRIP, error))?;
    ensure(
        ROUNDTRIP,
        !written.existed && written.bytes_written == original.len() as u64,
        "initial write returned incorrect existence or byte count",
    )?;

    let retried = fs
        .write(&context(), write.clone())
        .await
        .map_err(|error| ConformanceError::runtime(CONDITIONS, error))?;
    ensure(
        CONDITIONS,
        retried == written,
        "retrying an identical write operation returned a different result",
    )?;

    let metadata = fs
        .stat(
            &context(),
            StatRequest {
                path: file.clone(),
                follow_symlinks: true,
            },
        )
        .await
        .map_err(|error| ConformanceError::runtime(ROUNDTRIP, error))?;
    ensure(
        ROUNDTRIP,
        metadata.kind == FileKind::File,
        "stat did not report a file",
    )?;
    ensure(
        ROUNDTRIP,
        metadata.size == original.len() as u64,
        "stat returned an incorrect file size",
    )?;
    ensure(
        ROUNDTRIP,
        metadata.revision.as_ref() == Some(&written.revision),
        "stat revision did not match the write result",
    )?;

    let first = fs
        .read(
            &context(),
            ReadFileRequest {
                path: file.clone(),
                offset: 0,
                max_bytes: 4,
                follow_symlinks: true,
            },
        )
        .await
        .map_err(|error| ConformanceError::runtime(ROUNDTRIP, error))?;
    let second = fs
        .read(
            &context(),
            ReadFileRequest {
                path: file.clone(),
                offset: 4,
                max_bytes: 64,
                follow_symlinks: true,
            },
        )
        .await
        .map_err(|error| ConformanceError::runtime(ROUNDTRIP, error))?;
    let mut combined = first.data.into_inner();
    combined.extend(second.data.into_inner());
    ensure(
        ROUNDTRIP,
        combined == original,
        "offset reads did not reconstruct the file",
    )?;
    ensure(
        ROUNDTRIP,
        !first.eof && second.eof,
        "read EOF markers were incorrect",
    )?;
    report.pass(ROUNDTRIP);

    expect_error(
        CONDITIONS,
        fs.write(
            &context(),
            WriteFileRequest {
                operation_id: OperationId::generate(),
                path: file.clone(),
                data: BinaryData::new(b"duplicate".to_vec()),
                condition: WriteCondition::MustNotExist,
                create_parents: false,
                follow_symlinks: true,
            },
        )
        .await,
        ExecutionErrorCode::AlreadyExists,
    )?;

    expect_error(
        CONDITIONS,
        fs.write(
            &context(),
            WriteFileRequest {
                operation_id: OperationId::generate(),
                path: file.clone(),
                data: BinaryData::new(b"stale".to_vec()),
                condition: WriteCondition::MatchRevision {
                    revision: FileRevision::new("stale-revision")
                        .map_err(|error| ConformanceError::check(CONDITIONS, error.to_string()))?,
                },
                create_parents: false,
                follow_symlinks: true,
            },
        )
        .await,
        ExecutionErrorCode::RevisionConflict,
    )?;

    let replacement = b"replacement".to_vec();
    let replaced = fs
        .write(
            &context(),
            WriteFileRequest {
                operation_id: OperationId::generate(),
                path: file.clone(),
                data: BinaryData::new(replacement.clone()),
                condition: WriteCondition::MatchRevision {
                    revision: written.revision.clone(),
                },
                create_parents: false,
                follow_symlinks: true,
            },
        )
        .await
        .map_err(|error| ConformanceError::runtime(CONDITIONS, error))?;
    ensure(
        CONDITIONS,
        replaced.existed,
        "revision-conditional replacement did not report an existing file",
    )?;

    let conflicting = WriteFileRequest {
        data: BinaryData::new(b"different".to_vec()),
        ..write
    };
    expect_error(
        CONDITIONS,
        fs.write(&context(), conflicting).await,
        ExecutionErrorCode::OperationConflict,
    )?;
    report.pass(CONDITIONS);

    for (name, bytes) in [("a.txt", b"a".as_slice()), ("b.txt", b"b".as_slice())] {
        fs.write(
            &context(),
            WriteFileRequest {
                operation_id: OperationId::generate(),
                path: path(config, &format!("{namespace}/{name}"))?,
                data: BinaryData::new(bytes.to_vec()),
                condition: WriteCondition::MustNotExist,
                create_parents: false,
                follow_symlinks: true,
            },
        )
        .await
        .map_err(|error| ConformanceError::runtime(LISTING, error))?;
    }

    let first_page = fs
        .list(
            &context(),
            ListDirectoryRequest {
                path: directory.clone(),
                follow_symlinks: true,
                max_entries: 2,
                cursor: None,
            },
        )
        .await
        .map_err(|error| ConformanceError::runtime(LISTING, error))?;
    ensure(
        LISTING,
        first_page.entries.len() == 2 && first_page.next_cursor.is_some(),
        "bounded directory listing did not return a continuation cursor",
    )?;
    let second_page = fs
        .list(
            &context(),
            ListDirectoryRequest {
                path: directory,
                follow_symlinks: true,
                max_entries: 2,
                cursor: first_page.next_cursor,
            },
        )
        .await
        .map_err(|error| ConformanceError::runtime(LISTING, error))?;
    let mut names = first_page
        .entries
        .into_iter()
        .chain(second_page.entries)
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    names.sort();
    ensure(
        LISTING,
        names == ["a.txt", "b.txt", "data.bin"],
        format!("directory listing returned unexpected entries: {names:?}"),
    )?;

    expect_error(
        LISTING,
        fs.remove(
            &context(),
            RemovePathRequest {
                operation_id: OperationId::generate(),
                path: file.clone(),
                recursive: false,
                ignore_missing: false,
                expected_revision: Some(
                    FileRevision::new("stale-revision")
                        .map_err(|error| ConformanceError::check(LISTING, error.to_string()))?,
                ),
            },
        )
        .await,
        ExecutionErrorCode::RevisionConflict,
    )?;
    let removed = fs
        .remove(
            &context(),
            RemovePathRequest {
                operation_id: OperationId::generate(),
                path: file.clone(),
                recursive: false,
                ignore_missing: false,
                expected_revision: Some(replaced.revision),
            },
        )
        .await
        .map_err(|error| ConformanceError::runtime(LISTING, error))?;
    ensure(
        LISTING,
        removed.removed,
        "existing file was not reported as removed",
    )?;
    let missing = fs
        .remove(
            &context(),
            RemovePathRequest {
                operation_id: OperationId::generate(),
                path: file,
                recursive: false,
                ignore_missing: true,
                expected_revision: None,
            },
        )
        .await
        .map_err(|error| ConformanceError::runtime(LISTING, error))?;
    ensure(
        LISTING,
        !missing.removed,
        "missing file was reported as removed",
    )?;
    report.pass(LISTING);
    Ok(())
}

async fn run_processes(
    runtime: &dyn ExecutionRuntime,
    config: &ConformanceConfig,
    report: &mut ConformanceReport,
) -> Result<(), ConformanceError> {
    process_output_and_replay(runtime, config).await?;
    report.pass("process_output_and_replay");
    process_input_deduplication(runtime, config).await?;
    report.pass("process_input_deduplication");
    process_bounded_reads(runtime, config).await?;
    report.pass("process_bounded_reads");

    if runtime.descriptor().features.pty {
        process_pty(runtime, config).await?;
        report.pass("process_pty");
    } else {
        report.skip("process_pty", "runtime does not advertise PTY support");
    }

    process_timeout_and_termination(runtime, config).await?;
    report.pass("process_timeout_and_termination");
    if runtime.descriptor().features.process_signals {
        process_signal(runtime, config).await?;
        report.pass("process_signal");
    } else {
        report.skip(
            "process_signal",
            "runtime does not advertise process signal support",
        );
    }
    Ok(())
}

async fn process_output_and_replay(
    runtime: &dyn ExecutionRuntime,
    config: &ConformanceConfig,
) -> Result<(), ConformanceError> {
    const CHECK: &str = "process_output_and_replay";
    let mut environment = EnvironmentVariables {
        mode: EnvironmentMode::Inherit,
        set: BTreeMap::new(),
        remove: Default::default(),
    };
    environment.set.insert(
        "EXECUTION_CONFORMANCE_VALUE".to_string(),
        "stdout".to_string(),
    );
    let request = start_request(
        config,
        config.command(ConformanceProgram::OutputAndEnvironment),
        StdinMode::Closed,
        environment,
        None,
    );
    let handle = runtime
        .processes()
        .start(&context(config), request.clone())
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    let completed = read_until_terminal(runtime, config, &handle).await?;
    ensure(
        CHECK,
        completed.state == ExecutionState::Exited,
        "process did not exit normally",
    )?;
    ensure(
        CHECK,
        completed.exit_code == Some(0),
        "process did not report exit code 0",
    )?;
    ensure(
        CHECK,
        stdout_bytes(&completed.events) == b"stdout",
        "stdout bytes were incorrect",
    )?;
    ensure(
        CHECK,
        stderr_bytes(&completed.events) == b"stderr",
        "stderr bytes were incorrect",
    )?;
    ensure_ordered_and_closed(CHECK, &completed.events)?;

    let replay = runtime
        .processes()
        .read(
            &context(config),
            ReadExecutionRequest {
                execution_id: handle.execution_id.clone(),
                supervisor_generation_id: handle.supervisor_generation_id.clone(),
                after_sequence: 0,
                max_bytes: PROCESS_READ_BYTES,
                wait_ms: None,
            },
        )
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    ensure(
        CHECK,
        replay.events == completed.events,
        "replayed events differed from original events",
    )?;

    let retried = runtime
        .processes()
        .start(&context(config), request.clone())
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    ensure(
        CHECK,
        retried.execution_id == handle.execution_id,
        "idempotent start returned another execution",
    )?;

    let conflict = StartExecutionRequest {
        command: config.command(ConformanceProgram::DifferentOutput),
        ..request
    };
    expect_error(
        CHECK,
        runtime.processes().start(&context(config), conflict).await,
        ExecutionErrorCode::OperationConflict,
    )?;

    expect_error(
        CHECK,
        runtime
            .processes()
            .read(
                &context(config),
                ReadExecutionRequest {
                    execution_id: handle.execution_id,
                    supervisor_generation_id: SupervisorGenerationId::generate(),
                    after_sequence: 0,
                    max_bytes: PROCESS_READ_BYTES,
                    wait_ms: None,
                },
            )
            .await,
        ExecutionErrorCode::ExecutionLost,
    )?;
    Ok(())
}

async fn process_input_deduplication(
    runtime: &dyn ExecutionRuntime,
    config: &ConformanceConfig,
) -> Result<(), ConformanceError> {
    const CHECK: &str = "process_input_deduplication";
    let handle = runtime
        .processes()
        .start(
            &context(config),
            start_request(
                config,
                config.command(ConformanceProgram::ReadInput),
                StdinMode::Pipe,
                EnvironmentVariables::default(),
                None,
            ),
        )
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    let write = WriteProcessInputRequest {
        execution_id: handle.execution_id.clone(),
        supervisor_generation_id: handle.supervisor_generation_id.clone(),
        write_id: WriteId::generate(),
        input: ProcessInput::Data {
            data: BinaryData::new(b"hello\n".to_vec()),
        },
    };
    let accepted = runtime
        .processes()
        .write(&context(config), write.clone())
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    ensure(
        CHECK,
        accepted.status == ProcessInputStatus::Accepted,
        "first input was not accepted",
    )?;
    let duplicate = runtime
        .processes()
        .write(&context(config), write)
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    ensure(
        CHECK,
        duplicate.status == ProcessInputStatus::AlreadyAccepted,
        "duplicate input was not recognized",
    )?;
    let completed = read_until_terminal(runtime, config, &handle).await?;
    ensure(
        CHECK,
        stdout_bytes(&completed.events) == b"<hello>",
        "input was duplicated or corrupted",
    )?;
    Ok(())
}

async fn process_bounded_reads(
    runtime: &dyn ExecutionRuntime,
    config: &ConformanceConfig,
) -> Result<(), ConformanceError> {
    const CHECK: &str = "process_bounded_reads";
    let handle = runtime
        .processes()
        .start(
            &context(config),
            start_request(
                config,
                config.command(ConformanceProgram::BoundedOutput),
                StdinMode::Closed,
                EnvironmentVariables::default(),
                None,
            ),
        )
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    let complete = read_until_terminal(runtime, config, &handle).await?;
    let mut cursor = 0;
    let mut replayed = Vec::new();
    while cursor < complete.next_sequence {
        let page = runtime
            .processes()
            .read(
                &context(config),
                ReadExecutionRequest {
                    execution_id: handle.execution_id.clone(),
                    supervisor_generation_id: handle.supervisor_generation_id.clone(),
                    after_sequence: cursor,
                    max_bytes: 1,
                    wait_ms: None,
                },
            )
            .await
            .map_err(|error| ConformanceError::runtime(CHECK, error))?;
        let output_bytes = page
            .events
            .iter()
            .map(|event| match &event.event {
                ProcessEventKind::Output { data, .. } => data.len(),
                _ => 0,
            })
            .sum::<usize>();
        ensure(CHECK, output_bytes <= 1, "process read exceeded max_bytes")?;
        ensure(
            CHECK,
            page.next_sequence > cursor,
            "process read did not advance its cursor",
        )?;
        cursor = page.next_sequence;
        replayed.extend(stdout_bytes(&page.events));
    }
    ensure(
        CHECK,
        replayed == b"abcd",
        "bounded reads lost or duplicated output",
    )?;
    Ok(())
}

async fn process_pty(
    runtime: &dyn ExecutionRuntime,
    config: &ConformanceConfig,
) -> Result<(), ConformanceError> {
    const CHECK: &str = "process_pty";
    let handle = runtime
        .processes()
        .start(
            &context(config),
            start_request(
                config,
                config.command(ConformanceProgram::PtyInputAndSize),
                StdinMode::Pty {
                    columns: 80,
                    rows: 24,
                },
                EnvironmentVariables::default(),
                None,
            ),
        )
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    runtime
        .processes()
        .resize(
            &context(config),
            ResizePtyRequest {
                operation_id: OperationId::generate(),
                execution_id: handle.execution_id.clone(),
                supervisor_generation_id: handle.supervisor_generation_id.clone(),
                columns: 100,
                rows: 40,
            },
        )
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    runtime
        .processes()
        .write(
            &context(config),
            WriteProcessInputRequest {
                execution_id: handle.execution_id.clone(),
                supervisor_generation_id: handle.supervisor_generation_id.clone(),
                write_id: WriteId::generate(),
                input: ProcessInput::Data {
                    data: BinaryData::new(b"hello\n".to_vec()),
                },
            },
        )
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    let completed = read_until_terminal(runtime, config, &handle).await?;
    let output_bytes = pty_bytes(&completed.events);
    let output = String::from_utf8_lossy(&output_bytes);
    ensure(
        CHECK,
        output.contains("40 100"),
        format!("PTY resize was not observed: {output:?}"),
    )?;
    ensure(
        CHECK,
        output.contains("hello"),
        format!("PTY input was not observed: {output:?}"),
    )?;
    Ok(())
}

async fn process_timeout_and_termination(
    runtime: &dyn ExecutionRuntime,
    config: &ConformanceConfig,
) -> Result<(), ConformanceError> {
    const CHECK: &str = "process_timeout_and_termination";
    let timed = runtime
        .processes()
        .start(
            &context(config),
            start_request(
                config,
                config.command(ConformanceProgram::Sleep),
                StdinMode::Closed,
                EnvironmentVariables::default(),
                Some(50),
            ),
        )
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    let timed_result = read_until_terminal(runtime, config, &timed).await?;
    ensure(
        CHECK,
        timed_result.state == ExecutionState::Failed,
        "timed process did not fail",
    )?;

    let running = runtime
        .processes()
        .start(
            &context(config),
            start_request(
                config,
                config.command(ConformanceProgram::Sleep),
                StdinMode::Closed,
                EnvironmentVariables::default(),
                None,
            ),
        )
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    let terminate_request = TerminateExecutionRequest {
        operation_id: OperationId::generate(),
        execution_id: running.execution_id.clone(),
        supervisor_generation_id: running.supervisor_generation_id.clone(),
    };
    let terminated = runtime
        .processes()
        .terminate(&context(config), terminate_request.clone())
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    ensure(
        CHECK,
        terminated.was_running,
        "termination did not observe a running process",
    )?;
    let duplicate = runtime
        .processes()
        .terminate(&context(config), terminate_request)
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    ensure(
        CHECK,
        duplicate == terminated,
        "termination retry returned a different result",
    )?;
    let terminated_result = read_until_terminal(runtime, config, &running).await?;
    ensure(
        CHECK,
        terminated_result.state == ExecutionState::Cancelled,
        "explicitly terminated process did not become cancelled",
    )?;
    Ok(())
}

async fn process_signal(
    runtime: &dyn ExecutionRuntime,
    config: &ConformanceConfig,
) -> Result<(), ConformanceError> {
    const CHECK: &str = "process_signal";
    let handle = runtime
        .processes()
        .start(
            &context(config),
            start_request(
                config,
                config.command(ConformanceProgram::Sleep),
                StdinMode::Closed,
                EnvironmentVariables::default(),
                None,
            ),
        )
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    let request = SignalExecutionRequest {
        operation_id: OperationId::generate(),
        execution_id: handle.execution_id.clone(),
        supervisor_generation_id: handle.supervisor_generation_id.clone(),
        signal: ProcessSignal::Kill,
    };
    runtime
        .processes()
        .signal(&context(config), request.clone())
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    runtime
        .processes()
        .signal(&context(config), request)
        .await
        .map_err(|error| ConformanceError::runtime(CHECK, error))?;
    let result = read_until_terminal(runtime, config, &handle).await?;
    ensure(
        CHECK,
        !matches!(
            result.state,
            ExecutionState::Starting | ExecutionState::Running
        ),
        "signalled process did not become terminal",
    )?;
    Ok(())
}

fn start_request(
    config: &ConformanceConfig,
    command: CommandSpec,
    stdin: StdinMode,
    environment: EnvironmentVariables,
    timeout_ms: Option<u64>,
) -> StartExecutionRequest {
    StartExecutionRequest {
        operation_id: OperationId::generate(),
        execution_id: ExecutionId::generate(),
        command,
        cwd: ExecutionPath::root(config.root_id.clone()),
        environment,
        stdin,
        timeout_ms,
    }
}

async fn read_until_terminal(
    runtime: &dyn ExecutionRuntime,
    config: &ConformanceConfig,
    handle: &ExecutionHandle,
) -> Result<ReadExecutionResult, ConformanceError> {
    const CHECK: &str = "process_read_until_terminal";
    let deadline = tokio::time::Instant::now() + config.operation_timeout;
    let mut events = Vec::new();
    let mut after_sequence = 0;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(ConformanceError::check(
                CHECK,
                format!(
                    "execution `{}` did not become terminal",
                    handle.execution_id
                ),
            ));
        }
        let result = runtime
            .processes()
            .read(
                &OperationContext::with_deadline(deadline.into_std()),
                ReadExecutionRequest {
                    execution_id: handle.execution_id.clone(),
                    supervisor_generation_id: handle.supervisor_generation_id.clone(),
                    after_sequence,
                    max_bytes: PROCESS_READ_BYTES,
                    wait_ms: Some(PROCESS_WAIT_MS),
                },
            )
            .await
            .map_err(|error| ConformanceError::runtime(CHECK, error))?;
        after_sequence = result.next_sequence;
        events.extend(result.events);
        if terminal(result.state) {
            return Ok(ReadExecutionResult { events, ..result });
        }
    }
}

fn terminal(state: ExecutionState) -> bool {
    matches!(
        state,
        ExecutionState::Exited
            | ExecutionState::Failed
            | ExecutionState::Cancelled
            | ExecutionState::Lost
    )
}

fn context(config: &ConformanceConfig) -> OperationContext {
    OperationContext::with_timeout(config.operation_timeout)
}

fn path(config: &ConformanceConfig, value: &str) -> Result<ExecutionPath, ConformanceError> {
    ExecutionPath::new(config.root_id.clone(), value)
        .map_err(|error| ConformanceError::configuration(error.to_string()))
}

fn ensure(
    check: &'static str,
    condition: bool,
    message: impl Into<String>,
) -> Result<(), ConformanceError> {
    if condition {
        Ok(())
    } else {
        Err(ConformanceError::check(check, message))
    }
}

fn expect_error<T>(
    check: &'static str,
    result: Result<T, ExecutionError>,
    expected: ExecutionErrorCode,
) -> Result<(), ConformanceError> {
    match result {
        Err(error) if error.code == expected => Ok(()),
        Err(error) => Err(ConformanceError::check(
            check,
            format!(
                "expected {expected:?}, received {:?}: {}",
                error.code, error.message
            ),
        )),
        Ok(_) => Err(ConformanceError::check(
            check,
            format!("expected {expected:?}, but the operation succeeded"),
        )),
    }
}

fn ensure_ordered_and_closed(
    check: &'static str,
    events: &[ProcessEvent],
) -> Result<(), ConformanceError> {
    ensure(check, !events.is_empty(), "process returned no events")?;
    ensure(
        check,
        events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence),
        "process event sequences were not strictly increasing",
    )?;
    ensure(
        check,
        matches!(
            events.first().map(|event| &event.event),
            Some(ProcessEventKind::Started)
        ),
        "first process event was not started",
    )?;
    ensure(
        check,
        matches!(
            events.last().map(|event| &event.event),
            Some(ProcessEventKind::Closed)
        ),
        "last process event was not closed",
    )
}

fn stream_bytes(events: &[ProcessEvent], selected: ProcessOutputStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    for event in events {
        match &event.event {
            ProcessEventKind::Output { stream, data } if *stream == selected => {
                bytes.extend_from_slice(data.as_slice());
            }
            _ => {}
        }
    }
    bytes
}

fn stdout_bytes(events: &[ProcessEvent]) -> Vec<u8> {
    stream_bytes(events, ProcessOutputStream::Stdout)
}

fn stderr_bytes(events: &[ProcessEvent]) -> Vec<u8> {
    stream_bytes(events, ProcessOutputStream::Stderr)
}

fn pty_bytes(events: &[ProcessEvent]) -> Vec<u8> {
    stream_bytes(events, ProcessOutputStream::Pty)
}

impl fmt::Display for ConformanceReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} checks passed, {} skipped",
            self.passed.len(),
            self.skipped.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_profiles_use_the_expected_command_interpreters() {
        let root = RootId::new("workspace").expect("valid root ID");
        let unix = ConformanceConfig::unix(root.clone());
        let windows = ConformanceConfig::windows(root);

        let CommandSpec::Argv { program, arguments } = unix.command(ConformanceProgram::Sleep)
        else {
            panic!("Unix conformance command was not argv-based");
        };
        assert_eq!(program, "/bin/sh");
        assert_eq!(arguments, ["-c", "sleep 10"]);

        let CommandSpec::Argv { program, arguments } = windows.command(ConformanceProgram::Sleep)
        else {
            panic!("Windows conformance command was not argv-based");
        };
        assert_eq!(program, "powershell.exe");
        assert_eq!(
            arguments.last().map(String::as_str),
            Some("Start-Sleep -Seconds 10")
        );
    }
}
