use std::{collections::BTreeMap, pin::Pin};

use async_trait::async_trait;
use futures_util::Stream;

use crate::BlaxelTransportError;

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
    Pin<Box<dyn Stream<Item = Result<RemoteProcessEvent, BlaxelTransportError>> + Send + 'static>>;

#[derive(Clone, Debug)]
pub struct RemoteProcessRequest {
    pub command: String,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub cwd: Option<String>,
    /// Connector process identity, stored as Blaxel's process `name`.
    pub tag: Option<String>,
    /// Whether the provider process would need stdin. Blaxel's REST API does
    /// not expose it; the execution wrapper instead consumes control files.
    pub stdin: bool,
    /// Reserved for a future native terminal-WebSocket adapter. Recoverable
    /// tool PTYs are owned by the target-side Python wrapper.
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
pub trait BlaxelTransport: Send + Sync {
    async fn start_process(
        &self,
        request: RemoteProcessRequest,
    ) -> Result<RemoteProcessStream, BlaxelTransportError>;

    async fn connect_process(
        &self,
        pid: u32,
        timeout_ms: Option<u64>,
    ) -> Result<RemoteProcessStream, BlaxelTransportError>;

    async fn list_processes(&self) -> Result<Vec<RemoteProcessSummary>, BlaxelTransportError>;

    async fn write_control_file(
        &self,
        path: &str,
        content: &[u8],
    ) -> Result<(), BlaxelTransportError>;

    async fn signal_process(&self, pid: u32, kill: bool) -> Result<(), BlaxelTransportError>;
}
