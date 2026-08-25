use execution_contracts::{ProcessSignal, ShellDescriptor};
use tokio::process::{Child, Command};

use crate::error::{io_error, unsupported};

pub(crate) fn default_shell() -> ShellDescriptor {
    let executable = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_owned());
    ShellDescriptor {
        name: "cmd".to_owned(),
        executable,
    }
}

pub(crate) fn configure_process(_command: &mut Command) {}

pub(crate) fn terminate_process(
    child: &mut Child,
    signal: ProcessSignal,
) -> Result<(), execution_contracts::ExecutionError> {
    match signal {
        ProcessSignal::Kill | ProcessSignal::Terminate | ProcessSignal::Interrupt => child
            .start_kill()
            .map_err(|source| io_error(std::path::Path::new("process"), source)),
        ProcessSignal::Hangup | ProcessSignal::User1 | ProcessSignal::User2 => {
            Err(unsupported("signal is not supported on Windows"))
        }
    }
}
