use execution_runtime::OperationContext;
use tokio::task::{JoinError, JoinSet};
use uuid::Uuid;

use crate::{
    config::WorkerConfig,
    worker::{WorkerError, WorkerService},
};

use super::{PiRuntime, PiRuntimeBuildError, PiRuntimeError, RunOutcome};

type RunTaskResult = Result<(Uuid, Result<RunOutcome, PiRuntimeError>), JoinError>;

#[derive(Clone)]
pub struct WorkerRuntime {
    worker: WorkerService,
    pi: PiRuntime,
    max_concurrent_runs: usize,
}

impl WorkerRuntime {
    pub fn from_config(config: &WorkerConfig) -> Result<Self, WorkerRuntimeError> {
        Ok(Self::new(
            WorkerService::from_config(config)?,
            PiRuntime::from_config(config)?,
            config.max_concurrent_runs,
        ))
    }

    #[must_use]
    pub fn new(worker: WorkerService, pi: PiRuntime, max_concurrent_runs: usize) -> Self {
        assert!(
            max_concurrent_runs > 0,
            "worker concurrency must be positive"
        );
        Self {
            worker,
            pi,
            max_concurrent_runs,
        }
    }

    /// Claims runs until shutdown, executing at most the configured number at once.
    pub async fn run(&self, shutdown: &OperationContext) -> Result<(), WorkerRuntimeError> {
        let mut tasks = JoinSet::new();
        let mut claim_error = None;

        'worker: loop {
            if shutdown.is_cancelled() {
                break;
            }

            while tasks.len() >= self.max_concurrent_runs {
                tokio::select! {
                    () = shutdown.cancelled() => break 'worker,
                    joined = tasks.join_next() => observe(joined),
                }
            }

            let claim = self.worker.claim_next(shutdown);
            tokio::pin!(claim);
            let claimed = loop {
                if tasks.is_empty() {
                    break tokio::select! {
                        () = shutdown.cancelled() => break 'worker,
                        result = &mut claim => result,
                    };
                }

                tokio::select! {
                    () = shutdown.cancelled() => break 'worker,
                    joined = tasks.join_next() => observe(joined),
                    result = &mut claim => break result,
                }
            };
            match claimed {
                Ok(run) => spawn_run(&mut tasks, self.pi.clone(), run),
                Err(WorkerError::Cancelled) if shutdown.is_cancelled() => break,
                Err(error) => {
                    claim_error = Some(error);
                    break;
                }
            }
        }

        shutdown.cancel();
        while let Some(joined) = tasks.join_next().await {
            observe(Some(joined));
        }

        match claim_error {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
    }
}

fn spawn_run(
    tasks: &mut JoinSet<(Uuid, Result<RunOutcome, PiRuntimeError>)>,
    pi: PiRuntime,
    run: crate::worker::ActiveRun,
) {
    let run_id = run.run_id();
    tracing::info!(%run_id, "claimed run");
    tasks.spawn(async move { (run_id, pi.execute(run).await) });
}

fn observe(joined: Option<RunTaskResult>) {
    match joined {
        Some(Ok((run_id, Ok(RunOutcome::Completed(result))))) => {
            tracing::info!(%run_id, turn = result.run.current_turn, "run completed");
        }
        Some(Ok((run_id, Ok(RunOutcome::Continued(result))))) => {
            tracing::info!(%run_id, next_turn = result.run.current_turn, "run turn continued");
        }
        Some(Ok((run_id, Ok(RunOutcome::Failed(result))))) => {
            tracing::warn!(%run_id, will_retry = result.will_retry, "run turn failed");
        }
        Some(Ok((run_id, Ok(RunOutcome::Aborted(_))))) => {
            tracing::info!(%run_id, "run abort acknowledged");
        }
        Some(Ok((run_id, Err(PiRuntimeError::Cancelled)))) => {
            tracing::info!(%run_id, "run processing cancelled during worker shutdown");
        }
        Some(Ok((run_id, Err(error)))) => {
            tracing::warn!(%run_id, %error, "run processing stopped without a terminal response");
        }
        Some(Err(error)) => tracing::error!(%error, "run task panicked"),
        None => {}
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerRuntimeError {
    #[error(transparent)]
    Worker(#[from] WorkerError),
    #[error(transparent)]
    Pi(#[from] PiRuntimeBuildError),
}
