//! A harness consumer that has no dependency on the worker application.
use futures_util::future::BoxFuture;
use harness_runtime::{Execution, Harness, Signals};
use platform_runtime_client::{ClientConfig, PlatformClient, Result, types::Lease};
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;
use uuid::Uuid;

struct ObserveSignals {
    lease: Lease,
}

impl Harness for ObserveSignals {
    fn run(&self, mut execution: Execution) -> BoxFuture<'static, Result<()>> {
        let lease = self.lease;
        Box::pin(async move {
            assert_eq!(execution.client.lease().run_id, lease.run_id);
            assert_eq!(execution.client.lease().lease_epoch, lease.lease_epoch);
            execution.signals.changed().await.unwrap();
            let signals = *execution.signals.borrow_and_update();
            assert!(signals.abort_requested);
            assert!(signals.draining);
            assert!(signals.ownership_lost);
            assert_eq!(signals.input_generation, 1);
            // Test-only activation: no requests or durable effects are issued.
            Ok(())
        })
    }
}

#[tokio::test]
async fn harness_library_runs_without_a_worker_dependency() {
    let lease = Lease {
        run_id: Uuid::new_v4(),
        lease_epoch: 1,
    };
    let client = PlatformClient::new(
        "http://127.0.0.1:1",
        Uuid::new_v4(),
        "harness-interface-test-token-0123456789",
        ClientConfig::default(),
    )
    .unwrap()
    .run(lease)
    .unwrap();
    let (sender, signals) = watch::channel(Signals::default());
    let initial = *signals.borrow();
    assert!(!initial.abort_requested);
    assert!(!initial.draining);
    assert!(!initial.ownership_lost);
    assert_eq!(initial.input_generation, 0);

    let harness: Arc<dyn Harness> = Arc::new(ObserveSignals { lease });
    let task = tokio::spawn(harness.run(Execution { client, signals }));
    // The activation must own its dependencies, not borrow the harness instance.
    drop(harness);
    sender.send_modify(|signals| {
        signals.abort_requested = true;
        signals.draining = true;
        signals.ownership_lost = true;
        signals.input_generation = 1;
    });
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
