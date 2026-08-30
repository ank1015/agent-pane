use std::sync::Arc;

use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

/// One Codex-style reader/writer admission gate for a tool-call runtime.
///
/// Callers create separate gates for top-level calls and code-mode nested calls
/// in each sampling batch. Clones share admission only within that batch.
#[derive(Clone, Default)]
pub(super) struct ToolAdmissionGate {
    inner: Arc<RwLock<()>>,
}

impl ToolAdmissionGate {
    pub(super) async fn acquire(&self, tool_name: &str) -> ToolAdmissionPermit<'_> {
        if supports_parallel(tool_name) {
            ToolAdmissionPermit::Parallel {
                _guard: self.inner.read().await,
            }
        } else {
            self.acquire_exclusive().await
        }
    }

    pub(super) async fn acquire_exclusive(&self) -> ToolAdmissionPermit<'_> {
        ToolAdmissionPermit::Exclusive {
            _guard: self.inner.write().await,
        }
    }
}

/// Holding either variant keeps the corresponding gate admission alive.
pub(super) enum ToolAdmissionPermit<'a> {
    Parallel { _guard: RwLockReadGuard<'a, ()> },
    Exclusive { _guard: RwLockWriteGuard<'a, ()> },
}

fn supports_parallel(tool_name: &str) -> bool {
    matches!(
        tool_name,
        tool_codex_unified_exec::EXEC_COMMAND_TOOL_NAME
            | tool_codex_unified_exec::WRITE_STDIN_TOOL_NAME
            | tool_codex_view_image::TOOL_NAME
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::ToolAdmissionGate;

    #[tokio::test]
    async fn parallel_safe_tools_share_read_admission() {
        let gate = ToolAdmissionGate::default();
        let first = gate.acquire("exec_command").await;

        tokio::time::timeout(Duration::from_millis(100), gate.acquire("write_stdin"))
            .await
            .expect("write_stdin should overlap another reader");
        tokio::time::timeout(Duration::from_millis(100), gate.acquire("view_image"))
            .await
            .expect("view_image should overlap another reader");
        drop(first);
    }

    #[tokio::test]
    async fn exclusive_default_blocks_readers_and_other_writers() {
        for tool_name in ["exec", "wait", "apply_patch", "unknown_tool"] {
            let gate = ToolAdmissionGate::default();
            let exclusive = gate.acquire(tool_name).await;

            assert!(
                tokio::time::timeout(Duration::from_millis(20), gate.acquire("write_stdin"))
                    .await
                    .is_err(),
                "{tool_name} should block a reader"
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(20), gate.acquire("unknown_tool"))
                    .await
                    .is_err(),
                "{tool_name} should block another writer"
            );

            drop(exclusive);
            tokio::time::timeout(Duration::from_millis(100), gate.acquire("write_stdin"))
                .await
                .expect("reader should proceed after exclusive admission ends");
        }
    }

    #[tokio::test]
    async fn separate_batches_do_not_share_admission() {
        let first_batch = ToolAdmissionGate::default();
        let second_batch = ToolAdmissionGate::default();
        let _exclusive = first_batch.acquire("apply_patch").await;

        tokio::time::timeout(
            Duration::from_millis(100),
            second_batch.acquire("exec_command"),
        )
        .await
        .expect("a different batch must not share the first batch's gate");
    }
}
