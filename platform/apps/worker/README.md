# Shared worker

Run from this directory with `cargo run -p platform-worker`. Supply the variables
in `.env.example`; the registration secret must match Platform. Apply Platform's
migrations/start the updated server first. The worker needs no database URL or
broker. Gateway credentials are required only when enabling a harness that uses them.

The executable generates a fresh random process identity and token on every
start. An empty registry registers with zero capabilities and heartbeats normally;
it does not create claim receipts or claim another build's work. `/healthz` is
process liveness, `/readyz` requires registration and a successful heartbeat, and
`/status` exposes non-secret process counters. The listener defaults to loopback;
keep it private if configuring another address. Autoscaling infrastructure is not
provisioned here; replicas are independent and capacity-bounded.

Run the executable under a process supervisor with restart-on-failure enabled.

## Prompt work pickup

With spare capacity, the supervisor opens one cancellable
`GET /internal/workers/{id}/work-available?wait_seconds=25` request. An eligible
run wakes it and schedules the existing claim/reconciliation path immediately,
without waiting for the idle polling backoff. This also runs while other harnesses
are executing. No waits are started for empty registries or at full capacity.

The wait is separate from heartbeat and allocation tasks. Allocation, capacity
exhaustion, drain and shutdown cancel it. Transient failures reconnect with bounded
backoff; ordinary allocation polling remains independent as a fallback. An
uncertain claim retains its original idempotency key and is reconciled before
notifications can trigger another allocation attempt. There is no new broker,
database dependency, or per-harness notification code.

Notifications do not reserve work, replace leases, or start new worker processes.
All replicas may receive a hint; the normal claim locks decide ownership.

If Platform expires this worker's identity (for example, after a prolonged network
partition), the worker cancels local activations and exits unsuccessfully with
`WorkerOffline`. Restarting generates a fresh UUID/token and allows expired work
to be reclaimed. It never revives the old identity, marks runs terminal, or
cancels external operations. Transient transport failures still retry normally.

## Execution contract

`Registry` hosts trusted Rust libraries implementing `Harness::run(Execution)`.
`Harness`, `Execution`, and `Signals` live in
[`harness-runtime`](../../packages/harness-runtime/); the worker re-exports them
for compatibility. Each harness package imports that shared interface directly
and uses `platform-runtime-client` for durable state and Platform interactions.
It must not depend on this worker package: the worker depends on the harness
libraries, compiles them into one executable, and registers their implementations.
The registry and supervisor remain here, not in the shared interface crate.

Set `BASIC_CC_TOOLS_ENABLED=true` and supply the LLM/execution gateway settings
in `.env.example` to register `basic-cc-tools-harness`. Apply the server's harness
catalog migration before starting runs. See the
[harness package](../../packages/harnesses/basic-cc-tools-harness/) for its config
and recovery behavior. With the flag omitted/false the registry remains empty.
Integration tests also supply test-only implementations through the same interface.

- Each activation is one recoverable lease, not a universal agent turn. Harnesses
  load context/checkpoints and paginate history/inputs through the client.
- `execution.client.session_state(...)` loads private tool data for the current
  session; `Commit::session_state` writes it atomically with checkpoints and input
  handling. It survives worker replacement and follow-up runs, but not history
  forks. The harness owns namespaces/payloads; Platform owns durable storage and
  fencing. See the [SDK example](../../packages/platform-runtime-client/README.md#session-scoped-tool-state).
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
## Environments harness

Set `ENVIRONMENTS_ENABLED=true` to advertise the `environments` implementation.
It shares the LLM/execution gateway settings with the basic harness. Set
`FIRECRAWL_API_KEY` for environments runs; web tools are always enabled and
there is no configuration flag to disable them. Missing credentials fail the run
before model dispatch.
Filesystem targeting is per call; project scope is derived from the run. The
Platform catalog migration registers its config schema separately from worker
availability. Neither harness is enabled automatically.

## Sites harness

`SITES_ENABLED=true` registers the trusted `sites` harness. It needs the LLM gateway
settings, plus Platform Sites access/grants for authoring. Sites is Platform-owned
and does not require project opt-in.
A Sites-only worker does not load execution gateway credentials. `siteId` selects
an existing project site or is null for lazy creation. Multiple sessions may edit
the same live site. There is no development sandbox, draft or agent publication
step. See [Sites harness](../../packages/harnesses/sites/README.md) for recovery,
optional Firecrawl research, browser setup, and the complete system prompt.
