# agent

`agent` is the durable control plane for harness runs. Harnesses are independent
services: they consume run-turn work from NATS JetStream, own execution
concurrency and retries, and send one of four lifecycle commands back to Agent:
`complete`, `continue`, `fail`, or `wait`.

Agent never polls a harness, leases a run, counts execution attempts, sends
heartbeats, or retries harness work. A command that fails a run is terminal.
Harnesses can instead ask Agent to wait; a wait may be externally resolved or
automatically resolved at `expires_at`, after which Agent republishes the same
turn with typed resume data.

## Durable messaging

Agent provisions three JetStream streams on startup:

| Stream | Subjects | Purpose |
| --- | --- | --- |
| `AGENT_HARNESS_WORK` | `agent.harness.*.turn.requested.v1` | Per-harness turn work |
| `AGENT_COMMANDS` | `agent.run.*.command.v1`, `agent.run.*.event.v1` | Ordered per-run harness inputs |
| `AGENT_HARNESS_EVENTS` | results and cancellations | Command outcomes and best-effort abort notifications |

Delivery is at least once. Agent uses a transactional PostgreSQL outbox for
events, a command inbox keyed by `command_id`, `Nats-Msg-Id` publication
deduplication, and `expected_state_version` plus `turn_number` fencing. There
are intentionally no leases, attempts, heartbeats, or worker ownership rows.

Session transcript reads and appends remain HTTP because a running harness may
need the canonical message history. The harness bearer token protects:

```text
GET  /v1/harness/runs/{run_id}/messages
POST /v1/harness/runs/{run_id}/messages
```

All harness lifecycle transitions travel through NATS.

Harnesses may also publish fenced, non-authoritative observations on
`agent.run.{run_id}.event.v1`. Agent consumes commands and observations through
the same ordered stream, persists accepted observations in `run_events`, and
exposes the canonical event log through:

```text
GET /v1/runs/{run_id}/events
GET /v1/runs/{run_id}/events/stream
```

The stream endpoint is SSE. It replays events after `Last-Event-ID` (or the
`after_sequence` query parameter), remains open while the run is active or
waiting, and closes after `completed`, `failed`, or `aborted`.

Agent owns only the small lifecycle vocabulary: `run.started`,
`turn.requested`, `turn.ended`, `run.waiting`, `run.resumed`, and the terminal
run events. A harness emits `turn.started` and optional named `progress` events
such as model or tool-call boundaries. Harness observations are fenced by the
current `turn_number` and `expected_state_version`; stale observations are
acknowledged but not added to the log.

Each SSE frame uses the per-run sequence as its `id`, the dotted event name as
its `event`, and the complete `RunEvent` JSON object as its `data`. Connecting
mid-turn therefore returns all events after the supplied sequence, then waits
for new ones. A client can resume without gaps after a disconnect:

```sh
curl -N \
  -H "Authorization: Bearer $AGENT_CONTROL_TOKEN" \
  -H "Last-Event-ID: 12" \
  http://127.0.0.1:8080/v1/runs/$RUN_ID/events/stream
```

`run_events` in PostgreSQL is the durable source of truth. Inserts issue a
transactional PostgreSQL notification after commit so every Agent instance can
wake its local SSE clients; streams also periodically re-read the log to recover
from missed notifications. No Redis or separate event database is required.

## State model

The schema contains ten application tables:

```text
harnesses
└── harness_revisions

sessions
├── session_messages
└── runs
    ├── run_waits
    ├── run_aborts
    └── run_events

broker_outbox
broker_inbox
```

Run states are `active`, `waiting`, `aborted`, `completed`, and `failed`.
Queued steering messages are `session_messages` rows with
`delivery = 'next_turn'`; they enter the canonical transcript only when a
`continue` command advances the turn.

## Local development

Start PostgreSQL and NATS JetStream:

```sh
docker compose -f apps/agent/compose.yaml up -d
cp apps/agent/.env.example .env.agent
set -a
source .env.agent
set +a
cargo run -p agent
```

Set both bearer tokens to independent values of at least 32 bytes before
starting Agent. The NATS monitoring endpoint is available at
`http://127.0.0.1:8222` in the local compose setup.

`apps/pi-harness` implements the `pi` harness as a NATS-native server. It owns
its worker concurrency and turn retries while Agent owns only durable run state.
