use execution_core::{ExecutionError, ExecutionErrorCode, ProcessSignal};
use tokio::process::Child;

use crate::error::error;

#[cfg(unix)]
pub(crate) fn configure_process(command: &mut tokio::process::Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
pub(crate) fn configure_process(_command: &mut tokio::process::Command) {}

#[cfg(unix)]
pub(crate) fn signal_process_group(pid: u32, signal: ProcessSignal) -> Result<(), ExecutionError> {
    use nix::{
        sys::signal::{Signal, kill, killpg},
        unistd::Pid,
    };

    let pid = i32::try_from(pid).map_err(|_| {
        error(
            ExecutionErrorCode::Internal,
            "process identifier does not fit the host PID type",
        )
    })?;
    let signal = match signal {
        ProcessSignal::Interrupt => Signal::SIGINT,
        ProcessSignal::Terminate => Signal::SIGTERM,
        ProcessSignal::Kill => Signal::SIGKILL,
    };
    let pid = Pid::from_raw(pid);
    match killpg(pid, signal) {
        Ok(()) => Ok(()),
        Err(group_error) => kill(pid, signal).map_err(|process_error| {
            error(
                ExecutionErrorCode::Io,
                format!(
                    "failed to signal process group {pid} ({group_error}) and process {pid} ({process_error})"
                ),
            )
        }),
    }
}

#[cfg(unix)]
pub(crate) fn signal_piped_process(
    pid: u32,
    _child: &mut Child,
    signal: ProcessSignal,
) -> Result<(), ExecutionError> {
    signal_process_group(pid, signal)
}

#[cfg(windows)]
pub(crate) fn signal_piped_process(
    _pid: u32,
    child: &mut Child,
    _signal: ProcessSignal,
) -> Result<(), ExecutionError> {
    child
        .start_kill()
        .map_err(|source| crate::error::io_error(std::path::Path::new("process"), source))
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn signal_piped_process(
    _pid: u32,
    _child: &mut Child,
    _signal: ProcessSignal,
) -> Result<(), ExecutionError> {
    Err(error(
        ExecutionErrorCode::Unsupported,
        "process signals are not implemented on this platform",
    ))
}

#[cfg(not(unix))]
pub(crate) fn signal_process_group(
    _pid: u32,
    _signal: ProcessSignal,
) -> Result<(), ExecutionError> {
    Err(error(
        ExecutionErrorCode::Unsupported,
        "process-group signals are not implemented on this platform",
    ))
}

pub(crate) fn pty_signal_uses_kill_fallback(signal: ProcessSignal) -> bool {
    cfg!(windows) || signal == ProcessSignal::Kill
}

pub(crate) fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}
