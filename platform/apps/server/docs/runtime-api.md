# Runtime application API

These routes use the Platform PostgreSQL database, not either gateway database.
They create durable work records; they do not execute harnesses. Workers claim
`ready` runs through the [worker runtime API](worker-runtime-api.md). No harnesses
are seeded automatically. Register trusted implementations in `harnesses` before
creating sessions. Without a connected worker, runs remain queued.

Like the existing Platform routes, these are **trusted local-development APIs**.
They have no user authentication/authorization; do not expose them publicly.
Project associations are checked, but knowing a resource ID currently grants
access. Configurations must contain gateway/account references, not credentials:
the application can read harness defaults, run configuration, messages and events.
Worker lease ownership and private checkpoint state are not exposed.

## Shared conventions

- JSON bodies only, maximum 1 MiB; unknown body/query fields are rejected.
- Every POST requires `Idempotency-Key`: 1–256 printable ASCII characters without
  spaces. Reuse it with the same body after a timeout or uncertain result.
  Receipts are scoped to the operation and target, committed with all effects,
  and return the original status/body even if the resource subsequently changes.
  A different body using the same key returns 409. Failed transactions do not
  consume keys. Receipts currently have no automatic expiry.
- PATCH is naturally idempotent and does not require a key.
- UUID paths, missing records, and relationships are validated. Errors are JSON
  `{ "error": { "code": "...", "message": "..." } }`. Invalid parameters use
  400; malformed/unsupported JSON uses 400/415/422; oversized bodies 413; missing
  records 404; state or idempotency conflicts 409; configuration-schema mismatch
  422; contention timeouts 503. Internal errors contain no SQL or request bodies.
- Responses are `Cache-Control: no-store` except SSE. Application query caches
  can still cache results explicitly and invalidate/refetch after mutations.
- Titles are nullable or 1–512 characters without surrounding whitespace.
- Run inputs use `llm-contracts::Message`, restricted to the `user` role for
  application submissions. The client supplies a stable message ID and a Unix
  timestamp in milliseconds. Text parts use `content`, not `text`.

## Harnesses

| Route | Response |
| --- | --- |
| `GET /api/harnesses` | `{items: [...]}`, enabled harnesses sorted by name/ID |
| `GET /api/harnesses/{id}` | Harness record, including disabled registrations |

Records include ID, name, description, default configuration, configuration
schema, availability and timestamps. Harness IDs are strings, not UUIDs.

## Sessions

| Route | Request / response |
| --- | --- |
| `POST /api/projects/{project_id}/sessions` | Create below; 201 `{session, run, input}` |
| `GET /api/projects/{project_id}/sessions` | `{items, next_cursor}` |
| `GET /api/sessions/{id}` | Session record including `active_run` or null |
| `PATCH /api/sessions/{id}` | `{title?, archived?}`; 200 session record |
| `GET /api/sessions/{id}/messages` | `{items, next_after_revision}` |
| `POST /api/sessions/{id}/forks` | Fork below; 201 `{session, run, input}` |

Create a session without starting work:

```json
{"harness_id":"my-harness","title":"Investigate a problem"}
```

Optionally add `initial_run` using the run-start shape below. Creation, initial
input, run, event and receipt are one transaction. `run` and `input` are null
when `initial_run` is omitted. The harness must be enabled, but a session without
a run need not yet supply all required run configuration.

PATCH changes only supplied fields. `title: null` clears the title. `archived`
must be a boolean; archiving neither stops an active run nor deletes history,
but prevents starting a new run until unarchived. Harness/project identity cannot
be changed through PATCH. An empty PATCH is rejected.

Session listing defaults to non-archived sessions, ordered by last activity
descending, then ID descending. `archived=true` lists only archived sessions.
An `active_run` field contains the current live run if one exists.

Fork request:

```json
{"at_revision":12,"title":"Alternative approach","harness_id":"my-harness"}
```

`at_revision` is required (0 means empty history), and must exist in the source.
Title and harness default to the source; a different enabled harness may be
selected. Optional `initial_run` starts work atomically in the new session.
Inherited history shares immutable message records through the exact cutoff.
It has no child-run attribution, but retains each message's `origin_run_id`.
Forking does not clone files, environments, pending inputs, waits, checkpoints,
or run ancestry. In particular, a history fork does not imply `parent_run_id`.

Message reads accept `after_revision` (exclusive, default 0), `limit`, and
optional `run_id` belonging to this session. Each item contains `message_id`,
`revision`, `run_id`, `origin_run_id`, the common-envelope `message`, and
`created_at`. Messages enter canonical history through atomic worker commits,
not merely because the application submitted a pending input.

## Runs and inputs

| Route | Request / response |
| --- | --- |
| `POST /api/sessions/{id}/runs` | Start below; 201 `{run, input}` |
| `GET /api/sessions/{id}/runs` | `{items, next_cursor}` |
| `GET /api/runs/{id}` | Run record |
| `GET /api/runs/{id}/children` | Direct child runs, `{items, next_cursor}` |
| `POST /api/runs/{id}/inputs` | Input below; 201 `{input, run}` |
| `GET /api/runs/{id}/inputs` | `{items, next_after_sequence}` |
| `POST /api/runs/{id}/abort` | `{reason?: string}` (or `{}`); 202 `{run, input}` |
| `GET /api/runs/{id}/waits` | Waits with dependencies, `{items, next_cursor}` |

Run start / `initial_run`:

```json
{
  "expected_session_revision": 0,
  "input": {
    "role": "user",
    "id": "client-message-1",
    "timestamp": 1788480000000,
    "content": [{"type": "text", "content": "Investigate this problem."}]
  },
  "config_override": {"model": "my-model"}
}
```

`expected_session_revision` is required: use the session's current revision,
0 for a new session, or the fork cutoff for a fork's initial run. Stale history
and a second live run both return 409. Mid-run requests should use `/inputs`.

Configuration overrides are an optional object, limited to 64 KiB, merged into
current harness defaults using JSON Merge Patch semantics (recursive objects,
replace arrays/scalars, null deletes). The resolved object is validated against
the harness's JSON Schema using Draft 2020-12, without network/file retrieval,
and frozen on the run. Each new run resolves configuration afresh.

Additional user input:

```json
{
  "kind": "user_message",
  "message": {
    "role": "user",
    "id": "client-message-2",
    "timestamp": 1788480001000,
    "content": [{"type": "text", "content": "Focus on the second approach."}]
  }
}
```

Approval input:

```json
{"kind":"approval_response","correlation_key":"approval-1","approved":true,"data":{}}
```

Only these two input kinds are accepted from application callers. Internal
operation results, child results, source-run attribution and worker state cannot
be injected through these routes. Approval keys are 1–256 trimmed characters.

Inputs are persisted as `pending`, with monotonically allocated per-run sequence
numbers. Matching input wait dependencies are satisfied in the same transaction;
fulfilled any/all waits are resolved. Waiting runs become `ready`, even if the
new input does not match a wait, so the harness can decide what to do. Unmatched
waits remain pending. Running runs retain their worker ownership and lifecycle.
Durable acceptance is **not** acknowledgement of model delivery.

Abort records a sticky timestamp, a single `abort` input, and an event. Waiting
runs are made ready for cleanup; running harnesses must cooperatively observe
the request. Repeated requests do not overwrite the first reason or create more
abort inputs. It does not immediately mark a run `aborted`, kill operations,
resolve unrelated waits, or cancel descendants. New requests to terminal runs
return 409; existing idempotent replays still succeed. Reasons are at most 2048
trimmed characters.

## Pagination and events

All paginated collections accept `limit` from 1–200 (default 50). Cursor lists
accept opaque `cursor` values bound to the collection, resource and filters;
reuse the returned cursor with the same filters. Runs/children/waits sort by
creation time then ID ascending. Run lists filter by `status`; wait lists filter
by `pending`, `satisfied`, `cancelled`, or `timed_out`. Session activity may reorder
between pages; these are keyset pages, not frozen snapshots.

Input/event lists accept exclusive `after_sequence` (default 0). Inputs can
filter `status=pending|handled|rejected`. Message lists use `after_revision`.
Continuation fields are null on the last page. When polling for new items,
remember the last observed sequence/revision even when the continuation is null.

`GET /api/runs/{id}/events` returns `{items, next_after_sequence}`. Events include
their sequence, type, source, payload and timestamps. Application mutations emit
`run.created`, `run.input_received`, `run.abort_requested`, and, when applicable,
`run.woken`. A created/ready run has not necessarily begun execution.

`GET /api/runs/{id}/events/stream?after_sequence=0` replays and follows the same
durable events as SSE:

```text
id: 1
event: run.created
data: {"sequence":1,"type":"run.created", ...}

```

`Last-Event-ID` takes precedence over the query cursor on reconnect. IDs must be
nonnegative integers. Each SSE data payload is the full event record. Keepalive
comments occur every 15 seconds; proxy buffering is disabled. Local notifications
accelerate delivery; one-second database checks cover writes from other processes
and lost hints. Streaming does not hold a transaction or database connection while
idle. A terminal run's stream closes only after its committed events are drained.
On an in-stream database failure, an unnumbered `error` event instructs the client
to reconnect from its last event ID. Streams are not model-token streams unless
a harness persists such events. Clients should deduplicate by sequence.

## Verification

Run from `platform/`, with a local PostgreSQL role allowed to create databases:

```sh
DATABASE_URL=postgresql://localhost/postgres cargo test -p platform-server --test runtime_api -- --include-ignored
DATABASE_URL=postgresql://localhost/postgres cargo test -p platform-server --test runtime_schema -- --include-ignored
cargo test -p platform-server
cargo clippy -p platform-server --all-targets -- -D warnings
```

These suites use disposable databases, not the configured application database.
