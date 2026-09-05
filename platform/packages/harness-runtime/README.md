# Harness runtime

The shared Rust interface between harness libraries and the worker. This crate
owns only `Harness`, `Execution`, and `Signals`; it has no worker dependency,
registry, scheduler, HTTP listener, database pool, or agent loop.

Each harness lives in its own library package, depends on `harness-runtime` and
`platform-runtime-client`, and implements `Harness::run(Execution)`. Use
`futures_util::future::BoxFuture` for the return type. The worker depends on the
harness packages and registers their implementations under harness IDs. Adding a
harness requires rebuilding the shared worker, not deploying another server.

## Execution contract

- `run` handles one recoverable activation, not a fixed model turn. The worker
  may activate the same harness instance concurrently for different runs. Keep
  run/session state scoped to the activation or in durable storage, not in
  unkeyed fields on that shared instance.
- `Execution.client` is the lease-scoped Platform client. Restore run checkpoints
  and session-scoped tool state through it; a Rust future is not a durable stack.
  Checkpoints describe run progress; private session state survives follow-up
  runs. See the [client documentation](../platform-runtime-client/README.md).
- `Execution.signals` carries cooperative notifications. Inspect the initial
  value as well as changes. Input generations are hints to fetch pending inputs,
  not an input count or acknowledgement. The harness chooses when to consume
  inputs and how to handle abort; ownership loss means it must stop acting under
  that lease. Notifications do not guarantee time for cleanup before cancellation.
- Commit a ready, waiting, or terminal disposition before returning to release
  the lease. An error, panic, or cancellation does not invent a terminal result;
  an unreleased lease expires for recovery. Persist recovery-critical commands
  before external effects and keep effect reconciliation harness-owned.
- Yield Tokio execution, do not detach work, and keep CPU-bound work bounded and
  cancellable. Supervision and heartbeat renewal remain worker responsibilities.

`platform-worker` re-exports these same types for compatibility. New harness
packages should import them directly from `harness_runtime`, never depend on the
worker application.

## Verification

`cargo test -p harness-runtime` checks an independent harness implementation,
object-safe dispatch, a spawnable activation, and signal delivery without a
worker or database. The worker's database-backed integration tests exercise the
same interface through claiming, recovery, waiting, abort, and shutdown.
