use std::{collections::BTreeMap, pin::Pin};

use async_trait::async_trait;
use futures_util::Stream;

use crate::DaytonaTransportError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteStreamKind {
    Stdout,
    Stderr,
    Pty,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RemoteProcessId {
    pub session_id: String,
    pub command_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RemoteProcessEvent {
    Started {
        process_id: RemoteProcessId,
    },
    Output {
        stream: RemoteStreamKind,
        data: Vec<u8>,
    },
    Exited {
        exit_code: i32,
        exited: bool,
        status: String,
        error: Option<String>,
    },
    KeepAlive,
}

pub type RemoteProcessStream =
    Pin<Box<dyn Stream<Item = Result<RemoteProcessEvent, DaytonaTransportError>> + Send + 'static>>;

#[derive(Clone, Debug)]
pub struct RemoteProcessRequest {
    /// Stable provider-side session ID. Inline helper calls may leave this
    /// unset and receive a generated one.
    pub session_id: Option<String>,
    pub command: String,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub cwd: Option<String>,
    /// Connector-local diagnostic tag. Daytona's v1 process API does not
    /// store tags; recoverability is provided by the target-side journal.
    pub tag: Option<String>,
    pub stdin: bool,
    /// Reserved for a future native PTY adapter. The recoverable process
    /// wrapper owns a nested PTY and carries resize commands over stdin.
    pub pty: Option<(u16, u16)>,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RemoteProcessSummary {
    pub process_id: RemoteProcessId,
    pub command: String,
    pub arguments: Vec<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub tag: Option<String>,
}

#[async_trait]
pub trait DaytonaTransport: Send + Sync {
    async fn start_process(
        &self,
        request: RemoteProcessRequest,
    ) -> Result<RemoteProcessStream, DaytonaTransportError>;

    async fn connect_process(
        &self,
        process_id: &RemoteProcessId,
        timeout_ms: Option<u64>,
    ) -> Result<RemoteProcessStream, DaytonaTransportError>;

    async fn list_processes(&self) -> Result<Vec<RemoteProcessSummary>, DaytonaTransportError>;

    async fn send_input(
        &self,
        process_id: &RemoteProcessId,
        pty: bool,
        data: &[u8],
    ) -> Result<(), DaytonaTransportError>;

    async fn close_stdin(&self, process_id: &RemoteProcessId) -> Result<(), DaytonaTransportError>;

    async fn resize_pty(
        &self,
        process_id: &RemoteProcessId,
        columns: u16,
        rows: u16,
    ) -> Result<(), DaytonaTransportError>;

    async fn signal_process(
        &self,
        process_id: &RemoteProcessId,
        kill: bool,
    ) -> Result<(), DaytonaTransportError>;
}
