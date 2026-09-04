# Platform runtime client

Typed Rust transport for the shared worker and trusted harnesses. It talks to
Platform over HTTP/HTTPS; it **does not connect to PostgreSQL**. Wire types are
re-exported as `platform_runtime_client::types` and shared with the server through
`platform-runtime-contracts`.

## Quick start

```rust,no_run
use platform_runtime_client::{
    types::{Claim, ContextQuery}, ClientConfig, Command, PlatformClient,
    RequestKey, WorkerRegistration,
};
use uuid::Uuid;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = PlatformClient::new(
    &std::env::var("PLATFORM_URL")?,
    Uuid::new_v4(), // fresh process identity, not a stable deployment ID
    std::env::var("PLATFORM_WORKER_TOKEN")?, // separate strong per-process secret
    ClientConfig::default(),
)?;
client.register(
    &std::env::var("PLATFORM_WORKER_REGISTRATION_TOKEN")?,
    &WorkerRegistration {
        build_id: "worker-v1".into(),
        supported_harnesses: vec!["my-harness".into()],
        capacity: 8,
    },
).await?;

// The caller chooses this key ONCE for this allocation attempt.
let claim = Command::new(
    RequestKey::new(Uuid::new_v4().to_string())?,
    Claim { limit: 8 },
);
let result = client.claim(&claim).await?;
for assignment in result.items {
    let run = client.run(assignment.lease())?;
    let context = run.context(&ContextQuery::default()).await?;
    // Dispatch using assignment.harness_id and context.
    // The worker supervisor must separately maintain heartbeats.
}
# Ok(())
# }
```

`PlatformClient` is cheaply cloneable and shares a connection pool. It owns one
worker identity; `RunClient` additionally binds one run ID and lease epoch. It
never silently replaces that epoch or claims historical ownership is valid.
Bootstrap credentials are only sent for registration and are not retained.

## Surface

| Caller | Operations |
| --- | --- |
| Worker supervisor | `register`, `heartbeat`, `claim`, `assignments`, `patch_worker` |
| Harness, through `RunClient` | `context`, `inputs`, `commit`, `append_events`, `create_child`, `send_message`, `request_abort` |
| Harness reads, through `RunClient::queries()` / `QueryClient` | Harnesses, sessions, session history/runs, run state, children, waits, inputs, event replay pages |

Requests, records, statuses, response pages and conflict codes are typed.
Harness-owned checkpoint state, event/input payloads, and configuration remain
JSON objects. Canonical messages use `llm-contracts`.

Parent harnesses can obtain a read-only handle without an additional dependency:

```rust,no_run
# async fn example(run: platform_runtime_client::RunClient, child_id: uuid::Uuid) -> platform_runtime_client::Result<()> {
use platform_runtime_client::types::{MessageQuery, RunStatus};
let reads = run.queries(); // Also available on PlatformClient.
let child = reads.run_state(child_id).await?;
if child.status == RunStatus::Completed {
    let page = reads.session_messages(child.session_id, &MessageQuery::default()).await?;
    // Locate child.final_message_id, following next_after_revision as needed.
}
# Ok(())
# }
```

`QueryClient` exposes no registration, claims, lease creation, or writes. Its
observations do not prove ownership and do not acknowledge inputs. It reuses the
same transport, retries and explicit pagination, without introducing caching or
background work. Existing `PlatformClient` read methods remain available.
Harness IDs are checked using `types::is_valid_harness_id`, shared with the server
and worker registry; the grammar remains `[a-z][a-z0-9_-]{0,127}`.

## Request identity and recovery

Every receipt-backed mutation requires `Command<T>`, which holds an immutable key
and payload and supports Serde persistence. No client method generates keys.
Automatic retries reuse identical serialized body bytes and headers.

- **Uncertain outcome:** retry the same command against the same operation and
  source run. Timeout/cancellation/invalid response does not prove rollback.
- **New claim attempt:** choose a new key, even after an empty claim. Reusing a
  key retrieves its original allocation, not new work.
- **Recovery-critical coordination:** persist the whole command in harness-owned
  checkpoint state *before* sending it. Save source run/operation identity too
  when not already implicit in the checkpoint. Preserve existing recovery state.
- **Receipt recovery:** the original issuer or current replacement owner can
  retrieve an existing successful receipt. Responses are historical; re-read
  context/assignments before executing or building another commit.
- **External effects:** save gateway operation handles and reconcile in the
  harness. A Platform receipt cannot guarantee exactly-once model/tool execution.

See `examples/checkpointed_child.rs` for a compile-checked recovery pattern.

## Conflicts and versions

Inspect `error.conflict()` or the raw server code, never message text. Unknown
future codes are retained. **No 409 is automatically retried or rebased.**

| Code | Meaning / caller decision |
| --- | --- |
| `LEASE_LOST` | Stop new effects under this epoch; reconcile ownership |
| `RUN_VERSION_CONFLICT` | Reload context; harness decides how newer state affects the commit |
| `SESSION_REVISION_CONFLICT` | History changed or the requested fork revision does not exist |
| `CHECKPOINT_VERSION_CONFLICT` | Reload checkpoint; do not overwrite newer recovery state |
| `IDEMPOTENCY_KEY_CONFLICT` | Same key with different payload; fix request identity handling |
| `WORKER_IDENTITY_CONFLICT` | Registration identity differs; use a fresh process identity |
| `WORKER_STATE_CONFLICT` | Lifecycle state prevents this operation |
| `INPUT_STATE_CONFLICT`, `WAIT_STATE_CONFLICT` | Input/wait missing or no longer in required state |
| `RUN_STATE_CONFLICT`, `SESSION_STATE_CONFLICT`, `HARNESS_DISABLED` | Lifecycle operation not allowed |
| `RUNTIME_CONSTRAINT_CONFLICT`, `RUNTIME_CONFLICT` | Other state/integrity conflict; inspect and reconcile |

Lease epoch, run version, session revision and checkpoint version are distinct.
Do not substitute a heartbeat's newer version into a commit built from older
context. `Commit::new` requires explicit run and session versions; checkpoint
writes require their own expected version (zero if absent). No automatic version
caching, increments, merging or conflict rebasing.

## Transport policy

- HTTPS required except loopback; redirects disabled. Base URLs may include a
  deployment path prefix, but no credentials, query, or fragment.
- Defaults: 10-second timeout per attempt including response body, 3-second
  connection timeout, three attempts, exponential full-jitter backoff starting
  at 200 ms and capped at two seconds. Bounds are explicit in `ClientConfig`.
- Retry-safe reads, registration, heartbeats and receipt-backed mutations retry
  transport failures and HTTP 429/502/503/504 with the exact same request.
- `Retry-After` exceeding the delay bound returns control to the caller rather
  than retrying early or sleeping indefinitely.
- Worker PATCH is **not** automatically retried: it has no receipt and concurrent
  state changes could make replay overwrite newer intent. Reconcile metadata.
- Authentication, validation, conflicts, other HTTP errors, malformed JSON and
  contract mismatches return immediately. No model/tool retries here.
- Requests capped at server's 1 MiB limit. Responses default to a configurable
  16 MiB limit; reduce page size for large history records.
- Sensitive authorization headers; client/error Debug and Display never dump
  tokens, URLs, request payloads or raw responses. Explicit server details are
  available through `ServerError.error`.

## Pagination and worker responsibility

Context has **independent** message (`next_after_revision`) and wait
(`next_after_wait_id`) cursors. Follow each collection's response cursor. No
unbounded automatic history downloads or hidden background caches. Later context
pages can contain newer metadata.

Reading inputs is not acknowledging them. On recovery, scan pending inputs from
sequence zero; do not permanently skip unhandled inputs because a cursor advanced.
Checkpoint buffered input before acknowledging it in a commit. The harness
decides when to apply it, including mid-response steering.

The supervisor must heartbeat roughly every 15–20 seconds, stop local publishing
for lost leases, reconcile after reconnect, control capacity and drain on shutdown.
A commit does not renew a lease. Empty claims do not start internal polling loops.
Abort delivery is cooperative, not a forced kill.

Not included: worker loop, harness registry/trait, automatic heartbeats,
model/tool execution, harness recovery policy, DB access, or SSE subscriptions.
Durable event replay pages are provided. `create_child` always creates/forks a
new session. Use `send_message` for an active run, or `follow_up` to start another
run in an existing idle session with its current history revision. Both are
lease-fenced, idempotent operations; follow-ups retain the session's harness and
record the calling run as parent. Persist the command before sending it, just as
for child creation.

Application history reads currently rely on private-network deployment; worker
credentials do not make the unauthenticated application routes publicly secure.

## Verification

From `platform/`:

```sh
cargo test -p platform-runtime-client -p platform-runtime-contracts
DATABASE_URL=postgresql://localhost/postgres cargo test -p platform-server --test runtime_client -- --include-ignored
cargo clippy -p platform-runtime-client -p platform-runtime-contracts -p platform-server --all-targets -- -D warnings
```

SQLx integration tests create isolated databases, do not touch development data,
and do not require live gateways.
