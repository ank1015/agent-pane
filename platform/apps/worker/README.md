# Shared worker

Run from this directory with `cargo run -p platform-worker`. Supply the variables
in `.env.example`; the registration secret must match Platform. Apply Platform's
migrations/start the updated server first. No database URL, gateway credential,
broker, or production harness is needed by this empty build.

The executable generates a fresh random process identity and token on every
start. An empty registry registers with zero capabilities and heartbeats normally;
it does not create claim receipts or claim another build's work. `/healthz` is
process liveness, `/readyz` requires registration and a successful heartbeat, and
`/status` exposes non-secret process counters. The listener defaults to loopback;
keep it private if configuring another address. Autoscaling infrastructure is not
provisioned here; replicas are independent and capacity-bounded.

Run the executable under a process supervisor with restart-on-failure enabled.
If Platform expires this worker's identity (for example, after a prolonged network
partition), the worker cancels local activations and exits unsuccessfully with
`WorkerOffline`. Restarting generates a fresh UUID/token and allows expired work
to be reclaimed. It never revives the old identity, marks runs terminal, or
cancels external operations. Transient transport failures still retry normally.

## Execution contract

`Registry` hosts trusted Rust libraries implementing `Harness::run(Execution)`.
There are **no production harness registrations**. Integration tests supply
test-only implementations through that same interface.

- Each activation is one recoverable lease, not a universal agent turn. Harnesses
  load context/checkpoints and paginate history/inputs through the client.
- `execution.client.queries()` supplies read-only shared session history, child
  status/results, waits, and events. It shares transport/retry policy without
  exposing registration or claiming. Reading another run does not confer its
  execution ownership; mutations still use the lease-scoped client.
- Input polling notifies independently of harness execution; it never consumes
  inputs. Harnesses determine steering timing and checkpoint buffered input
  before acknowledgement. Abort is a sticky cooperative signal.
- Heartbeats renew independently. Conservative monotonic deadlines cancel local
  execution on uncertain ownership. Server epochs still fence durable writes;
  they cannot undo external effects already sent to gateways.
- Allocation reconciles assignments, reuses an uncertain claim command, validates
  ownership with a heartbeat, and prevents duplicate local `(run, epoch)` tasks.
  Empty/error polling uses bounded exponential backoff with jitter.
- Commit a ready/waiting/terminal disposition before returning. A returned or
  panicked activation is not restarted under that same epoch. Its unreturned
  lease expires; no invented failure or cancellation is written. Recovery and
  effect retries remain harness-owned, including persistence of command receipts.
- Shutdown drains claims and signals active harnesses while continuing renewals.
  After the grace period, local futures are cancelled. No user-abort request,
  child cascade, or external process kill is manufactured. Offline is requested
  only through Platform's guarded lifecycle operation; live leases can prevent it.
- Trusted harnesses must yield Tokio execution and must not detach work. CPU-bound
  work needs bounded blocking execution with cancellation. A process crash still
  relies on lease expiry; Rust futures are not durable stack snapshots.

## Verification

`DATABASE_URL=postgresql://localhost/postgres cargo test -p platform-worker -- --include-ignored`
runs tests against actual Platform HTTP handlers, migrations, and isolated SQLx
databases. The test database role needs CREATEDB. No development data is modified.
