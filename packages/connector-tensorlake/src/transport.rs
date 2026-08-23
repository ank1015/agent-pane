use std::{collections::BTreeMap, pin::Pin};

use async_trait::async_trait;
use futures_util::Stream;

use crate::TensorlakeTransportError;

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

pub type RemoteProcessStream = Pin<
    Box<dyn Stream<Item = Result<RemoteProcessEvent, TensorlakeTransportError>> + Send + 'static>,
>;

#[derive(Clone, Debug)]
pub struct RemoteProcessRequest {
    pub command: String,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub cwd: Option<String>,
    /// Connector-local diagnostic tag. Tensorlake's v1 process API does not
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
    pub pid: u32,
    pub command: String,
    pub arguments: Vec<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub tag: Option<String>,
}

#[async_trait]
pub trait TensorlakeTransport: Send + Sync {
    async fn start_process(
        &self,
        request: RemoteProcessRequest,
    ) -> Result<RemoteProcessStream, TensorlakeTransportError>;

    async fn connect_process(
        &self,
        pid: u32,
        timeout_ms: Option<u64>,
    ) -> Result<RemoteProcessStream, TensorlakeTransportError>;

    async fn list_processes(&self) -> Result<Vec<RemoteProcessSummary>, TensorlakeTransportError>;

    async fn send_input(
        &self,
        pid: u32,
        pty: bool,
        data: &[u8],
    ) -> Result<(), TensorlakeTransportError>;

    async fn close_stdin(&self, pid: u32) -> Result<(), TensorlakeTransportError>;

    async fn resize_pty(
        &self,
        pid: u32,
        columns: u16,
        rows: u16,
    ) -> Result<(), TensorlakeTransportError>;

    async fn signal_process(&self, pid: u32, kill: bool) -> Result<(), TensorlakeTransportError>;
}
