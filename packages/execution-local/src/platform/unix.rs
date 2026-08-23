use std::process::Stdio;

use execution_contracts::{ProcessSignal, ShellDescriptor};
use tokio::process::Child;

use crate::error::{error, io_error, unsupported};

pub(crate) fn default_shell() -> ShellDescriptor {
    let executable = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
    let name = std::path::Path::new(&executable)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sh")
        .to_owned();
    ShellDescriptor { name, executable }
}

pub(crate) fn terminate_process(
    child: &mut Child,
    signal: ProcessSignal,
) -> Result<(), execution_contracts::ExecutionError> {
    match signal {
        ProcessSignal::Kill | ProcessSignal::Terminate => child
            .start_kill()
            .map_err(|source| io_error(std::path::Path::new("process"), source)),
        _ => {
            let Some(pid) = child.id() else {
                return Err(error(
                    execution_contracts::ExecutionErrorCode::UnknownExecution,
                    "process has already exited",
                ));
            };
            let signal_name = match signal {
                ProcessSignal::Interrupt => "INT",
                ProcessSignal::Hangup => "HUP",
                ProcessSignal::User1 => "USR1",
                ProcessSignal::User2 => "USR2",
                ProcessSignal::Terminate | ProcessSignal::Kill => unreachable!(),
            };
            std::process::Command::new("kill")
                .args([format!("-{signal_name}"), pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|source| io_error(std::path::Path::new("kill"), source))
                .and_then(|status| {
                    status
                        .success()
                        .then_some(())
                        .ok_or_else(|| unsupported(format!("failed to deliver SIG{signal_name}")))
                })
        }
    }
}
