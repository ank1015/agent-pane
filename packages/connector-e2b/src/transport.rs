use std::{collections::BTreeMap, pin::Pin};

use async_trait::async_trait;
use futures_util::Stream;

use crate::E2bTransportError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteStreamKind {
    Stdout,
    Stderr,
    Pty,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RemoteProcessEvent {
    Started {
        pid: u32,
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
    Pin<Box<dyn Stream<Item = Result<RemoteProcessEvent, E2bTransportError>> + Send + 'static>>;

#[derive(Clone, Debug)]
pub struct RemoteProcessRequest {
    pub command: String,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub cwd: Option<String>,
    pub tag: Option<String>,
    pub stdin: bool,
    /// E2B PTY allocation. The recoverable agent-pane process wrapper does not
    /// use this: it owns a nested PTY and carries resize commands over stdin.
    pub pty: Option<(u16, u16)>,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RemoteProcessSummary {
    pub pid: u32,
    pub command: String,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub cwd: Option<String>,
    pub tag: Option<String>,
}

#[async_trait]
pub trait E2bTransport: Send + Sync {
    async fn start_process(
        &self,
        request: RemoteProcessRequest,
    ) -> Result<RemoteProcessStream, E2bTransportError>;

    async fn connect_process(
        &self,
        pid: u32,
        timeout_ms: Option<u64>,
    ) -> Result<RemoteProcessStream, E2bTransportError>;

    async fn list_processes(&self) -> Result<Vec<RemoteProcessSummary>, E2bTransportError>;

    async fn send_input(&self, pid: u32, pty: bool, data: &[u8]) -> Result<(), E2bTransportError>;

    async fn close_stdin(&self, pid: u32) -> Result<(), E2bTransportError>;

    async fn resize_pty(&self, pid: u32, columns: u16, rows: u16) -> Result<(), E2bTransportError>;

    async fn signal_process(&self, pid: u32, kill: bool) -> Result<(), E2bTransportError>;
}
