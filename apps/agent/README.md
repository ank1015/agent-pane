# agent

`agent` is the durable control plane for hosted agent harnesses.
Its database stores product state and only the current worker ownership lease;
it does not persist turns, worker-attempt history, steering records, generic
events, or HTTP command receipts as separate concepts.

The initial schema has eight tables:

```text
harnesses
└── harness_revisions

sessions
├── session_messages
└── runs
    ├── run_leases      (zero or one current lease)
    ├── run_waits
    └── run_aborts
```

- A steer is a `session_messages` row with `delivery = 'next_turn'`. It remains
  `pending` without a transcript revision until a turn boundary commits it, or
  becomes `discarded` if the run terminates first.
- A run stores its current turn number and current-turn failure budget. Logical
  turns are not independent persisted resources.
- `run_leases` contains only current worker ownership. Replacing or ending a
  lease removes the old row rather than retaining attempt history.
- Waits and aborts remain separate because they have different initiators and
  lifecycle transitions.
- Safe retries rely on caller-generated resource IDs, run state versions, and
  lease fencing. Exact historical HTTP response replay is not part of this
  schema.

## Application

The application provides environment configuration, PostgreSQL pooling and
migrations, independent control and worker bearer tokens, a shared JSON error
envelope, request body limits, and public `GET /health` and `GET /ready`
endpoints.

The control token protects the harness registry, session, and execution routes:

```text
POST  /v1/harnesses
GET   /v1/harnesses
GET   /v1/harnesses/{harness_id}
PATCH /v1/harnesses/{harness_id}
PUT   /v1/harnesses/{harness_id}/enabled

POST  /v1/harnesses/{harness_id}/revisions
GET   /v1/harnesses/{harness_id}/revisions
GET   /v1/harnesses/{harness_id}/revisions/{revision_id}
PUT   /v1/harnesses/{harness_id}/active-revision
DELETE /v1/harnesses/{harness_id}/active-revision
POST  /v1/harnesses/{harness_id}/revisions/{revision_id}/retire

POST  /v1/sessions
GET   /v1/sessions/{session_id}
GET   /v1/sessions/{session_id}/messages
GET   /v1/sessions/{session_id}/runs

POST  /v1/sessions/{session_id}/runs
GET   /v1/runs/{run_id}
POST  /v1/runs/{run_id}/messages
GET   /v1/runs/{run_id}/messages
GET   /v1/runs/{run_id}/messages/{session_message_id}

GET   /v1/waits
GET   /v1/waits/{wait_id}
POST  /v1/waits/{wait_id}/resolve
GET   /v1/runs/{run_id}/waits

POST  /v1/runs/{run_id}/abort
POST  /v1/runs/{run_id}/resume
GET   /v1/runs/{run_id}/aborts
GET   /v1/aborts/{abort_id}
```

The worker token protects lease-fenced execution routes:

```text
POST /v1/worker/runs/claim
POST /v1/worker/runs/{run_id}/heartbeat
GET  /v1/worker/runs/{run_id}/messages
POST /v1/worker/runs/{run_id}/messages
POST /v1/worker/runs/{run_id}/complete
POST /v1/worker/runs/{run_id}/fail
POST /v1/worker/runs/{run_id}/wait
POST /v1/worker/runs/{run_id}/abort/acknowledge
```

Harness and revision creation use caller-supplied IDs for safe retries. Revision
status is derived from the harness's active revision pointer and the revision's
activation and retirement timestamps.

Session creation also uses a caller-supplied ID for safe retries. Transcript
reads return only committed messages in revision order; pending and discarded
next-turn messages are not canonical transcript entries. Run history is a
read-only session subresource here.

Control clients can inspect next-turn messages separately from the canonical
transcript. Inspection is ordered by `queue_sequence`, supports `state`,
`after_sequence`, and `limit` query parameters, and includes pending, committed,
and discarded messages.

The execution module starts runs by atomically committing the external trigger
message and queued run. It resolves the selected harness revision, applies the
configuration override as JSON Merge Patch, validates the resolved
configuration, and enforces server run limits. Caller-generated resource IDs,
run state versions, and lease fencing make retries and concurrent transitions
safe without storing HTTP receipts. Workers can suspend runs on waits, receive
abort directives through heartbeats, and reclaim resolved or resumed runs with
typed resume data.

For local configuration:

```sh
cp apps/agent/.env.example .env.agent
set -a
source .env.agent
set +a
cargo run -p agent
```
