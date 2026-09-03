//! Versioned wire protocol and shared dispatch for execution runtimes.
//!
//! Domain request and result values come directly from `execution-core`; this
//! crate only adds correlation, versioning, framing, and operation dispatch.

mod codec;
mod dispatch;
mod protocol;

pub use codec::{DEFAULT_MAX_FRAME_BYTES, NdjsonCodec, WireCodecError};
pub use dispatch::{dispatch_operation, dispatch_request};
pub use protocol::{
    Operation, OperationResult, PROTOCOL_NAME, PROTOCOL_VERSION, RequestEnvelope, RequestId,
    ResponseEnvelope,
};
