# Chat integration contract

## Session configuration

Only harnesses available to the project may create sessions or start new runs.
Bootstrap filters the picker catalogue. Use `GET /api/projects/{id}/harnesses`
for the full settings catalogue and `PUT /api/projects/{id}/harnesses/{harness}`
with `{ "enabled": true }` to opt in. Required platform harnesses are automatic.
`PROJECT_HARNESS_DISABLED` and `HARNESS_DISABLED` are 409 conflicts: refresh
availability rather than automatically retrying the same rejected submission.
Existing history remains readable after disabling. These checks also apply to
forks, child-agent creation, and harness-initiated follow-up runs.

Create with `POST /api/projects/{id}/sessions` and an `Idempotency-Key` header:

```json
{
  "harness_id": "basic-cc-tools-harness",
  "title": "Work on the project",
  "config_override": {
    "model": {"provider": "openai", "id": "gpt-5.6-terra"},
    "reasoning_level": "high",
    "account_id": "<provider account UUID>",
    "environment": {
      "type": "machine",
      "machine_id": "<host UUID>",
      "workspace_root": "/home/user",
      "path": "project"
    }
  },
  "initial_run": {
    "expected_session_revision": 0,
    "input": {
      "role": "user", "id": "<stable message ID>", "timestamp": 0,
      "content": [{"type": "text", "content": "Implement the feature"}]
    }
  }
}
```

`timestamp` is Unix milliseconds. Preserve the key, message ID, timestamp, and
entire body on an ambiguous retry. Response is `{session, run, input}`; `run` and
`input` are null if `initial_run` is omitted. Even an empty session resolves and
validates configuration at creation. `session.config` is immutable and includes
the harness defaults as they existed then. Settings/default/schema changes do
not rewrite existing sessions or affect subsequent runs' config snapshots.

Start follow-ups using `POST /api/sessions/{id}/runs` with only `input` and
`expected_session_revision`. `config_override` is rejected on this endpoint,
including an empty object. A run uses the frozen session config. Concurrent work
or changed history returns a coded 409; refresh state before deciding how to retry.

`POST /api/sessions/{id}/forks` accepts `at_revision`, optional `harness_id`,
`title`, `config_override`, and `initial_run`. Same-harness forks merge overrides
into the parent's frozen config. A different harness resolves from that harness's
defaults, not incompatible parent fields. Forks never copy private session state.
Overrides use recursive merge semantics: null removes a field. When switching
environment variants on a fork, remove the old source ID with null as well as
setting the new type and source ID.
Worker `Child` commands likewise put `config_override` on the child request, not
inside `initial_run`; non-fork children resolve from harness defaults. Worker
follow-up commands cannot change their target session's config.

The migration backfills existing sessions from their most recent run, or harness
defaults if they have no runs. Old run snapshots are not rewritten. Legacy basic
direct-host configs are upgraded on sessions; old run snapshots remain readable
by the basic harness for recovery. This is a breaking request-contract change:
callers must move creation overrides out of `initial_run`.

## Environment policy is harness-owned

The basic harness accepts one descriptor under `environment`: either the machine
shape above, or `{type:"sandbox", snapshot_id, workspace_root, path}`. All IDs must
be valid UUIDs; `workspace_root` is an absolute native path, and `path` is portable
and relative to that workspace. Execution root IDs are private implementation details. It resolves the target
and persists it in its private session state. Sandbox creation uses a stable
session-level gateway key, including recovery after an uncertain response.
Follow-ups reuse the target. Forks materialize their own target. Sandbox forks
start from the configured snapshot, not live parent filesystem changes.

The environments harness needs no environment setting. Platform remains generic:
other harnesses can define zero, one, or multiple environments in their schema.

## Accepted input versus conversation history

`GET /api/sessions/{id}/inputs?limit=50&cursor=...` returns `{items,next_cursor}`,
ordered by creation time and ID across all runs. Optional `status` is `pending`,
`handled`, or `rejected`; default is all. Cursors are session/filter-scoped.
Records contain `run_id`, `kind`, `payload`, `status`, `handling`, and timestamps.
This read never acknowledges input. Internal deduplication keys are omitted.

Use this feed to display submitted input immediately and after reload, even if
the worker has not claimed the run. Existing per-run `/inputs` reads also remain.
`pending` means queued; it does not mean model-visible. Both current harnesses
atomically append accepted user input to history and mark it handled with
`handling.message_id` referencing the history message. That means incorporated
into harness history, not proof that a provider has processed it. Reconcile by
that ID; legacy inputs can be matched by the common user-message ID. Do not hide
unconsumed inputs just because a run failed or was aborted.

## Live conversation

History: `GET /api/sessions/{id}/messages?after_revision=...`; optionally `run_id`
for the details drawer. Runs: `GET /api/sessions/{id}/runs`. Live status and replay:
`GET /api/runs/{id}`, `/events`, `/events/stream?after_sequence=...`.

Treat `run.committed` as a hint to fetch incremental history and refresh input
handling. Lifecycle events trigger run/session refreshes. Replay by event sequence,
deduplicate by event ID, and refresh on reconnect. The stream supports Last-Event-ID.
Current harnesses publish complete model messages and committed tool results,
not token deltas or streaming command output. Usage/cost are optional message data.

Steering: `POST /api/runs/{id}/inputs` with `{kind:"user_message", message:...}`.
Stop: `POST /api/runs/{id}/abort` with optional `reason`. Both require idempotency
keys. Stop is cooperative; request acceptance is not terminal acknowledgement.
The frontend should show `ready`, `running`, and `waiting` distinctly and keep
queued input visible until handled/rejected. No frontend wiring is included here.
