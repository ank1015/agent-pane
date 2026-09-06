use execution_core::*;
use execution_supervisor_core::{
    SupervisorConfig, SupervisorLimits, SupervisorRoot, SupervisorRuntime,
};

struct Fixture {
    temp: tempfile::TempDir,
    config: SupervisorConfig,
    runtime: SupervisorRuntime,
}
impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("work")).unwrap();
        let config = SupervisorConfig {
            host_id: ExecutionHostId::generate(),
            state_directory: temp.path().join("state"),
            roots: vec![SupervisorRoot {
                id: RootId::new("work").unwrap(),
                name: "Work".into(),
                path: temp.path().join("work"),
                read_only: false,
            }],
            limits: SupervisorLimits::default(),
        };
        let runtime = SupervisorRuntime::new(config.clone()).await.unwrap();
        Self {
            temp,
            config,
            runtime,
        }
    }
    fn write(&self) -> WriteFileRequest {
        WriteFileRequest {
            operation_id: OperationId::generate(),
            path: ExecutionPath::new(RootId::new("work").unwrap(), "file").unwrap(),
            data: BinaryData::new(b"patched\n".to_vec()),
            condition: WriteCondition::Any,
            create_parents: true,
            follow_symlinks: true,
            strategy: WriteStrategy::InPlace,
            expected_generation: Some(self.runtime.descriptor().supervisor_generation_id.clone()),
        }
    }
    fn remove(&self) -> RemovePathRequest {
        RemovePathRequest {
            operation_id: OperationId::generate(),
            path: self.write().path,
            recursive: false,
            ignore_missing: false,
            expected_revision: None,
            expected_generation: Some(self.runtime.descriptor().supervisor_generation_id.clone()),
            target_kind: RemoveTargetKind::File,
        }
    }
    fn path(&self) -> std::path::PathBuf {
        self.temp.path().join("work/file")
    }
}

#[tokio::test]
async fn old_generation_is_rejected_by_host_even_with_cached_client_descriptor() {
    let fixture = Fixture::new().await;
    let write = fixture.write();
    let remove = fixture.remove();
    let restarted = SupervisorRuntime::new(fixture.config.clone())
        .await
        .unwrap();
    std::fs::write(fixture.path(), "recreated\n").unwrap();
    let context = OperationContext::new();
    assert_eq!(
        restarted
            .filesystem()
            .write(&context, write)
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::ExecutionLost
    );
    assert_eq!(
        restarted
            .filesystem()
            .remove(&context, remove)
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::ExecutionLost
    );
    assert_eq!(std::fs::read(fixture.path()).unwrap(), b"recreated\n");
}

#[tokio::test]
async fn failed_fenced_mutations_are_replayed_without_retrying_io() {
    let fixture = Fixture::new().await;
    let context = OperationContext::new();
    std::fs::create_dir(fixture.path()).unwrap();
    let write = fixture.write();
    let failure = fixture
        .runtime
        .filesystem()
        .write(&context, write.clone())
        .await
        .unwrap_err();
    std::fs::remove_dir(fixture.path()).unwrap();
    std::fs::write(fixture.path(), "external\n").unwrap();
    assert_eq!(
        fixture
            .runtime
            .filesystem()
            .write(&context, write)
            .await
            .unwrap_err(),
        failure
    );
    assert_eq!(std::fs::read(fixture.path()).unwrap(), b"external\n");
    std::fs::remove_file(fixture.path()).unwrap();
    let remove = fixture.remove();
    let failure = fixture
        .runtime
        .filesystem()
        .remove(&context, remove.clone())
        .await
        .unwrap_err();
    std::fs::write(fixture.path(), "recreated\n").unwrap();
    assert_eq!(
        fixture
            .runtime
            .filesystem()
            .remove(&context, remove)
            .await
            .unwrap_err(),
        failure
    );
    assert!(fixture.path().exists());
}

#[tokio::test]
async fn fenced_receipts_reject_changed_payloads() {
    let fixture = Fixture::new().await;
    let context = OperationContext::new();
    let mut write = fixture.write();
    fixture
        .runtime
        .filesystem()
        .write(&context, write.clone())
        .await
        .unwrap();
    write.data = BinaryData::new(b"changed".to_vec());
    assert_eq!(
        fixture
            .runtime
            .filesystem()
            .write(&context, write)
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::OperationConflict
    );
    assert_eq!(std::fs::read(fixture.path()).unwrap(), b"patched\n");
}

#[cfg(unix)]
#[tokio::test]
async fn file_only_removal_refuses_directory_and_dangling_symlinks() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new().await;
    let context = OperationContext::new();
    std::fs::create_dir(fixture.temp.path().join("work/dir")).unwrap();
    symlink("dir", fixture.path()).unwrap();
    assert_eq!(
        fixture
            .runtime
            .filesystem()
            .remove(&context, fixture.remove())
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::IsDirectory
    );
    std::fs::remove_file(fixture.path()).unwrap();
    symlink("missing", fixture.path()).unwrap();
    assert_eq!(
        fixture
            .runtime
            .filesystem()
            .remove(&context, fixture.remove())
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::NotFound
    );
    assert!(
        std::fs::symlink_metadata(fixture.path())
            .unwrap()
            .is_symlink()
    );
    std::fs::remove_file(fixture.path()).unwrap();
    let outside = fixture.temp.path().join("outside");
    std::fs::write(&outside, "outside").unwrap();
    symlink(&outside, fixture.path()).unwrap();
    assert_eq!(
        fixture
            .runtime
            .filesystem()
            .remove(&context, fixture.remove())
            .await
            .unwrap_err()
            .code,
        ExecutionErrorCode::PathOutsideRoot
    );
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
    let mut ordinary = fixture.remove();
    ordinary.target_kind = RemoveTargetKind::Any;
    ordinary.expected_generation = None;
    fixture
        .runtime
        .filesystem()
        .remove(&context, ordinary)
        .await
        .unwrap();
    assert!(std::fs::symlink_metadata(fixture.path()).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn atomic_write_default_keeps_existing_replacement_behavior() {
    let fixture = Fixture::new().await;
    std::fs::write(fixture.path(), "old\n").unwrap();
    let alias = fixture.temp.path().join("work/alias");
    std::fs::hard_link(fixture.path(), &alias).unwrap();
    let mut request = fixture.write();
    request.strategy = WriteStrategy::AtomicReplace;
    request.expected_generation = None;
    fixture
        .runtime
        .filesystem()
        .write(&OperationContext::new(), request)
        .await
        .unwrap();
    assert_eq!(std::fs::read(alias).unwrap(), b"old\n");
    assert_eq!(std::fs::read(fixture.path()).unwrap(), b"patched\n");
}

#[test]
fn dropped_mutation_future_cannot_be_blindly_retried() {
    use std::{future::poll_fn, task::Poll};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let fixture = Fixture::new().await;
        let (release, blocked) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        // Hold the only filesystem worker so cancellation reliably happens
        // after receipt insertion and before the first IO completes.
        let blocker = tokio::task::spawn_blocking(move || {
            started.send(()).unwrap();
            blocked.recv().unwrap();
        });
        ready.await.unwrap();
        let request = fixture.write();
        let context = OperationContext::new();
        let mut future = fixture
            .runtime
            .filesystem()
            .write(&context, request.clone());
        poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(future);
        release.send(()).unwrap();
        blocker.await.unwrap();
        let failure = fixture
            .runtime
            .filesystem()
            .write(&context, request)
            .await
            .unwrap_err();
        assert_eq!(failure.code, ExecutionErrorCode::ExecutionLost);
        assert_eq!(failure.details["mutation_outcome"], "unknown");
        assert!(!fixture.path().exists());
    });
}
