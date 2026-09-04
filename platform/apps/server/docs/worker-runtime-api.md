# Worker runtime API

Platform owns PostgreSQL and scheduling state. Workers use these HTTP routes;
they do not connect to the database. A worker process hosts any number of trusted
Rust harness implementations. These routes do not implement a universal turn
loop or execute harnesses inside Platform.

## Configuration and authentication

Set `PLATFORM_WORKER_REGISTRATION_TOKEN` in the server's `.env` to a strong random
secret of 32–256 printable ASCII characters without spaces. An unset token
disables registration. Migrations run at normal server startup. Deploy the same
bootstrap secret on Platform replicas and distribute it privately to workers.
Use TLS outside localhost; do not expose the unauthenticated application routes
publicly merely because the internal worker routes are authenticated.

Each process chooses a fresh UUID and a separate random `worker_token` (32–256
printable ASCII characters). Use the bootstrap bearer token **only** for worker
registration. Subsequent requests use `Authorization: Bearer <worker_token>`.
Tokens are stored as SHA-256 hashes in `worker_credentials`, never returned in
worker metadata, events, or context. A token authenticates only its own worker ID.

Run routes additionally require:

```http
X-Worker-Id: <registered worker UUID>
X-Lease-Epoch: <epoch returned by the claim>
Idempotency-Key: <stable logical operation key, for POST>
```

JSON only; 1 MiB body limit, unknown fields rejected, `Cache-Control: no-store`.
UUIDs identify database resources. Native message IDs inside `llm-contracts`
messages remain arbitrary nonempty strings, independent of database message IDs.
Errors use the existing `{"error":{"code","message"}}` envelope: 401 for missing
or invalid credentials, 404 for missing/out-of-project resources, 409 for stale
leases/versions or conflicting state, 400/422 for invalid requests, 503 for
temporary database contention. Error responses do not contain SQL or secrets.

Conflicts now have distinct stable codes: `LEASE_LOST`,
`RUN_VERSION_CONFLICT`, `SESSION_REVISION_CONFLICT`,
`CHECKPOINT_VERSION_CONFLICT`, `IDEMPOTENCY_KEY_CONFLICT`,
`WORKER_IDENTITY_CONFLICT`, `WORKER_STATE_CONFLICT`,
`INPUT_STATE_CONFLICT`, `WAIT_STATE_CONFLICT`, `RUN_STATE_CONFLICT`,
`SESSION_STATE_CONFLICT`, `HARNESS_DISABLED`, and
`RUNTIME_CONSTRAINT_CONFLICT` (with `RUNTIME_CONFLICT` as a generic fallback).
Callers must not parse message text or blindly retry/rebase 409s. Database
serialization failures, deadlocks, and lock/statement timeouts return 503
`RUNTIME_BUSY`; retry those with the same key and payload.

## Rust client

Use [platform-runtime-client](../../../packages/platform-runtime-client/README.md)
for typed worker/harness calls. It shares request contracts with the server,
provides lease-bound run handles and typed responses, and requires explicit
persistable `Command<T>` values for receipt-backed mutations. Automatic retries
preserve the exact request identity. It does not implement worker scheduling,
automatic heartbeats, or harness recovery policy.

## Worker lifecycle

### `PUT /internal/workers/{worker_id}`

Bootstrap authentication. Example:

```json
{
  "build_id": "worker-2026-09-04",
  "supported_harnesses": ["pi", "codex"],
  "capacity": 16,
  "worker_token": "<separate strong per-process secret>"
}
```

Returns worker metadata (200). Harness IDs must exist; disabled harnesses may
still need recovery. An empty `supported_harnesses` array is allowed for idle
worker builds and cannot claim any runs. Repeating registration with the same identity, build,
harness set, and token returns existing metadata without resetting capacity,
draining status, timestamps, or leases. Changed identity conflicts; use a new UUID
on process restart. Update capacity using PATCH, not repeated registration.

### `POST /internal/workers/{worker_id}/heartbeat`

```json
{"leases": [{"run_id": "<uuid>", "lease_epoch": 3}]}
```

Updates process liveness. Renews only listed, still-current, unexpired leases to
60 seconds from database time. Returns `renewed`, `lost`, and
`lease_duration_seconds`; renewed entries include the current run version and
abort marker. Send roughly every 15–20 seconds. An empty list heartbeats an idle
process. Maximum 1,000 unique runs per batch; larger workers can send batches.
Missing, expired, or superseded leases are reported as lost, never resurrected.
Draining workers can renew existing assignments. No idempotency key is required:
repeating a heartbeat simply renews still-valid leases.

### `POST /internal/workers/{worker_id}/claims`

`Idempotency-Key` required; body `{"limit": 16}` (1–200).

Returns `items` (assigned runs with `harness_id`) and `lease_duration_seconds`.
Only accepting workers claim work. Allocation respects supported harnesses and
remaining live-lease capacity, including simultaneous requests by the same
worker. Queue reads use `FOR UPDATE SKIP LOCKED`; competing workers cannot acquire
the same epoch. Due ready runs and expired running runs are eligible. A takeover
increments `lease_epoch`; a claim increments run version and emits started/resumed.
Disabling a harness prevents new runs, not recovery of existing ones.

Claims return immediately, possibly empty. Use backoff/jitter on empty polls and
a **new key for each new allocation attempt**, including after an empty result.
Retry the same key after a network error to retrieve the same allocation without
claiming extra work. A receipt is historical: its leases can expire before replay.
Use assignments/context to reconcile before executing. Claim receipts are retained
for the worker identity; no automatic receipt/history deletion is configured.

### `GET /internal/workers/{worker_id}/assignments`

Returns `worker` and `items`, containing only this process's unexpired running
assignments. Does not renew ownership. Reconcile local tasks after reconnects;
discard stale tasks/epochs. Returned metadata contains no credentials.

### `PATCH /internal/workers/{worker_id}`

Body: `{"status":"draining","capacity":8}`; either field may be omitted.
Status is accepting/draining/offline, capacity positive. Reducing capacity does
not interrupt existing work. Draining stops new claims but permits renewals and
commits. Offline requires no live assignments and is irreversible for that
process identity. It is not a force-kill operation.

## Execution and recovery

### `GET /internal/runs/{id}/context`

Requires current ownership. Returns a consistent `run`, `session`, latest
`checkpoint` (or null), a `messages` page, and a `waits` page including dependency
results. Run configuration is the immutable resolved snapshot from creation.
History includes the session's inherited fork prefix and earlier runs.

Query: `limit` (1–200, default 50), `after_revision` (default 0), `after_wait_id`
(optional UUID). Pages expose `next_after_revision` / `next_after_wait_id` inside
their respective objects; follow each cursor independently. Later context pages
may reflect newer run/input/wait state; use the latest run version and session
revision when committing. Other application reads can list child runs and events.

### `GET /internal/runs/{id}/inputs`

Requires current ownership. Query: `limit`, `after_sequence` (default 0), and
`status` (pending by default; also handled/rejected/all). Returns `items` and
`next_after_sequence`. Reading does **not** acknowledge an input. Re-scan pending
inputs from zero on recovery; do not advance a permanent cursor past unhandled
inputs. Harnesses decide when input reaches a model, including mid-response
steering. Persist an input in recovery state before acknowledging it if it will
be consumed later.

### `POST /internal/runs/{run_id}/commits`

One transaction coordinates messages, input outcomes, checkpoint CAS, wait
registration/cancellation, events, and lifecycle. No partial success.

```json
{
  "expected_run_version": 2,
  "expected_session_revision": 0,
  "checkpoint": {"expected_version": 0, "state": {"phase": "model"}},
  "messages": [{
    "message_id": "<new database UUID>",
    "message": {
      "role": "user", "id": "native-message-id", "timestamp": 0,
      "content": [{"type": "text", "content": "Solve this problem"}]
    }
  }],
  "input_results": [{"id": "<input UUID>", "status": "handled", "handling": {}}],
  "events": [{"type": "harness.progress", "payload": {"phase": "model"}}],
  "waits": [],
  "cancel_wait_ids": [],
  "disposition": {"status": "running"}
}
```

Only expected run/session versions are required; collections default empty and
disposition defaults running. Checkpoint state must be an object; expected version
0 means absent, subsequent writes compare and increment its version. No arbitrary
Rust stack is checkpointed. Messages validate against `llm-contracts`; sequence
and revision are allocated by Platform. Input results must reference pending
inputs in this run and use handled/rejected, with optional object `handling`.
Each collection is bounded to 200 items per request.

Disposition options:

| Status | Behavior |
| --- | --- |
| `running` | Keep lease; persist a recovery boundary without ending execution |
| `ready` | Release lease, queue immediately or at optional `available_at` |
| `waiting` | Release lease while durable waits remain pending |
| `completed` | Requires `final_message_id` referencing this run's final assistant message, with no tool calls |
| `failed` | Requires an object `error`; terminal harness-reported failure |
| `aborted` | Acknowledges a persisted abort request; terminal |

Completion cannot silently discard newly pending input: settle it or reload after
409. Failed/aborted runs may retain unhandled inputs for inspection. Terminal
commits cancel remaining local waits; they never cascade to children. Every
successful commit increments run version; heartbeat renewal does not. A commit
does not itself extend the lease. Return values include `run`, `session_revision`,
`checkpoint`, appended message/wait IDs, and appended harness events.

Wait objects contain `wait_key`, `mode` (any/all), optional `deadline_at`, optional
object `metadata`, and 1–200 dependencies:

```json
{"wait_key":"join-1","mode":"all","dependencies":[
  {"kind":"run_completion","target_run_id":"<child UUID>"},
  {"kind":"timer","wake_at":"2026-09-05T00:00:00Z"}
]}
```

Other dependency shapes are `{"kind":"input","input_kind":"approval_response",
"correlation_key":"approval-1"}` (correlation optional), and
`{"kind":"operation","correlation_key":"operation-1"}`. A run cannot depend on
itself or another project's run. Wait keys are unique within the run; reuse the
commit receipt for retries, and a new wait key for a new logical wait.

Conditions are checked during registration and by a background reconciler. Any
resolved wait wakes a suspended run; `all` combines dependencies **within a wait**,
not separate wait records. Terminal child states include completed/failed/aborted.
Uncorrelated input waits match pending input, not already-consumed history.
Correlated input/operation waits can match prior delivery even if acknowledged;
use a unique correlation key per logical operation. Operation results arrive as
durable `operation_result` inputs; Platform does not poll arbitrary gateways or
invent their success. A satisfied condition wins over a deadline if both are
observed in the same pass; otherwise deadline expiry produces `timed_out`.

New pending input prevents parking or a delayed retry from losing its wake-up:
the commit queues the run immediately instead. To pause with buffered input, first
checkpoint and acknowledge that input. Persisted run versions, not notifications,
are authoritative. Wait resolution while a worker is running does not stop it.
It increments the run version, so a commit based on pre-resolution context
conflicts; reload context and let the harness decide what to do next. Satisfied
waits consistently return `result.dependencies` entries containing `id`, `kind`,
and `result`, regardless of input delivery order. Settlement emits
`run.wait_resolved` once per batch of newly resolved waits.

### `POST /internal/runs/{id}/events`

Body `{"events":[{"type":"model.delta","payload":{"text":"hello"}}]}`.
One to 200 events; optional `occurred_at`. Requires a lease and idempotency key.
Returns ordered appended events under `items`. Events have source `harness`;
`run.*` is reserved for Platform. Use commits for events that must be atomic with
state changes. Application SSE consumes the same persisted event log.
Event types cannot contain CR or LF; both event append and commit requests reject
these names before persisting any changes.

## Multi-agent operations

All require current source-run ownership and stable idempotency keys. Targets
must belong to the same project. They do not require targets to be direct children:
trusted harnesses may coordinate ordinary runs throughout their project.

### `POST /internal/runs/{parent_run_id}/children`

```json
{
  "harness_id": "codex",
  "title": "Check the proof",
  "fork_at_revision": 12,
  "initial_run": {
    "expected_session_revision": 12,
    "input": {"role":"user","id":"child-task","timestamp":0,
      "content":[{"type":"text","content":"Check the proof"}]},
    "config_override": {}
  }
}
```

Creates a separate session and ready run with `parent_run_id`, returning `session`,
`run`, `input` (201). Harness defaults to the source session's harness. Omitting
`fork_at_revision` creates an empty session (expected revision 0). A fork copies
exact immutable message memberships through the parent's requested revision;
inherited messages have no child-run attribution. It copies neither checkpoints
nor machine files/sandboxes. Harnesses handle environment provisioning through the
execution gateway. New children require an enabled harness and valid resolved
configuration. Child creation does not automatically suspend the parent.

### `POST /internal/runs/{source_run_id}/follow-ups`

```json
{
  "target_session_id": "<existing session UUID>",
  "run": {
    "expected_session_revision": 14,
    "input": {"role":"user","id":"follow-up-task","timestamp":0,
      "content":[{"type":"text","content":"Try another approach"}]},
    "config_override": {}
  }
}
```

Starts a new ready run in an existing session, returning `session`, `run`, and
`input` (201). History and harness are retained; nothing is forked or reset.
The calling run becomes `parent_run_id`. The session must be in the same project,
unarchived, idle, at the expected history revision, and use an enabled harness.
An active target session returns a conflict: use `/messages` for its active run.
Concurrent starts serialize on the session; replaying the same idempotency key
returns the original result without creating another run. Source ownership is
checked before accepting new work and again before committing.

### `POST /internal/runs/{source_run_id}/messages`

```json
{"target_run_id":"<uuid>","kind":"agent_message","payload":{"text":"Try another approach"}}
```

Returns durable `input` plus target `run` (201); source attribution is set by
Platform. `kind` is agent_message or operation_result; the latter requires
`payload.correlation_key`. Messages may target a running, waiting, or queued run;
terminal targets conflict. Delivery wakes suspended/delayed targets and resolves
matching conditions; only the target harness decides when to apply the input.
This route cannot forge user approvals or abort markers.

### `POST /internal/runs/{source_run_id}/abort-requests`

Body `{"target_run_id":"<uuid>","reason":"No longer needed"}` (reason optional).
Returns target `run` and `input` (202). Persists an abort marker and one abort input,
wakes the target, and leaves cleanup/acknowledgement to its harness. Repeated
requests reuse the original abort input. Terminal targets return their existing
state without a new input, making parent cleanup race-safe. No implicit cascade,
force-kill, or external operation cancellation.

## Recovery and operation identity

An idempotency key identifies a **logical operation**, not its worker or lease.
Checkpoint keys before issuing uncertain coordination requests. The original
authenticated issuer can retrieve its successful receipt after losing ownership;
a replacement worker with a current lease can retrieve the same receipt. Neither
case repeats effects. Other workers cannot replay it. A reused key with a changed
body conflicts. Replayed responses are historical; reload context afterward.

Epoch checks fence **new writes**, including events and child/message operations.
Expiration never resumes a suspended Rust future: a new owner reconstructs from
context, checkpoints, inputs, waits, and receipts. A background pass every second
requeues expired leases, resolves ready conditions/deadlines, and marks processes
offline after 120 seconds without heartbeat and without live assignments. Safe
to run on all Platform replicas; each pass is bounded and retries on database
contention. Notification loss cannot lose work.

There is no exactly-once guarantee for remote model/tool effects. Save gateway
operation handles in checkpoint state and implement appropriate reconciliation in
the harness. Fleet autoscaling and worker execution remain separate from these
Platform routes and the Rust client package.

## Verification

From `platform/`, against a PostgreSQL role with `CREATEDB`:

```sh
DATABASE_URL=postgresql://localhost/postgres cargo test -p platform-server -- --include-ignored
cargo clippy -p platform-server --all-targets -- -D warnings
```

SQLx creates isolated databases. Tests cover every route, concurrent capacity and
commit fencing, receipt replay across takeover, immutable forks, mixed wait
conditions, deadlines, input-before-park, operation results, scoped messaging,
cooperative abort, and atomic rollback. They do not mutate the development project
database or require live gateways.
