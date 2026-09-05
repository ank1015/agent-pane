use platform_runtime_contracts::{ConflictCode, ErrorInfo};

pub type Result<T> = std::result::Result<T, Error>;

/// HTTP errors retain unknown future codes. Debug/Display do not log server
/// messages, request bodies, URLs, or credentials.
pub struct ServerError {
    pub status: u16,
    pub error: Option<ErrorInfo>,
}
impl ServerError {
    pub fn code(&self) -> Option<&str> {
        self.error.as_ref().map(|e| e.code.as_str())
    }
    pub fn conflict(&self) -> Option<ConflictCode> {
        (self.status == 409)
            .then(|| self.error.as_ref()?.conflict())
            .flatten()
    }
}
impl std::fmt::Debug for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerError")
            .field("status", &self.status)
            .field("code", &self.code())
            .finish()
    }
}
impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Platform returned HTTP {} ({})",
            self.status,
            self.code().unwrap_or("unknown code")
        )
    }
}
impl std::error::Error for ServerError {}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The supervisor observed a permanently offline process identity. Restart
    /// with a fresh UUID/token; retrying or reviving this identity cannot recover.
    #[error("worker identity is offline; restart the process with a fresh identity")]
    WorkerOffline,
    #[error("invalid client request/configuration: {0}")]
    Invalid(&'static str),
    #[error("Platform transport failed (timeout: {timed_out}); mutation outcome may be unknown")]
    Transport { timed_out: bool },
    #[error("invalid Platform response: {0}; mutation outcome may be unknown")]
    Protocol(&'static str),
    #[error("{0}")]
    Server(#[from] ServerError),
}
impl Error {
    pub fn conflict(&self) -> Option<ConflictCode> {
        match self {
            Self::Server(e) => e.conflict(),
            _ => None,
        }
    }
}
