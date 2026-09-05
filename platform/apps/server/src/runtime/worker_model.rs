pub(super) use platform_runtime_contracts::{
    AbortRequest, Child, Claim, Commit, ContextQuery, Dependency, Disposition, Events, FollowUp,
    HarnessEvent, Heartbeat, InputStatus, PatchWorker, RegisterWorker, RunMessage, Wait, WaitMode,
    WorkerStatus,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct Owner {
    pub worker_id: Uuid,
    pub lease_epoch: i64,
}
