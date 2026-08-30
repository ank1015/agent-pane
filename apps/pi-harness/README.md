# pi-harness

`pi-harness` is the long-running server for the `pi` coding harness. It consumes
durable turn requests from NATS JetStream, runs up to a configured number of Pi
workers inside the process, and publishes exactly one lifecycle operation back
to Agent: `complete`, `continue`, `fail`, or `wait`.

The binary uses `packages/agent-harness-sdk` for the reusable broker server,
cancellation, command-result routing, and Agent transcript access. Pi-specific
model, context, compaction, tool, and execution behavior remains in this app.

Agent does not poll this process and this process does not claim or lease runs.
The broker provides at-least-once delivery; Agent state versions and stable
command IDs make redelivery safe.

## Runtime ownership

- A shared durable consumer load-balances `pi` turns across harness replicas.
- Each server executes up to `PI_HARNESS_MAX_CONCURRENT_TURNS` turns concurrently.
- LLM/provider retries and their backoff happen inside the turn worker.
- Exhausted retryable LLM failures produce an expiring `wait`. Agent resumes the
  same turn when that wait expires or is resolved externally.
- Agent transcript reads and appends remain authenticated HTTP calls.
- Command publication and result acknowledgement are retried by the harness
  with the same command ID.
- Agent cancellation events stop matching in-process work. Broker delivery is
  NAKed on shutdown or transient transport failure and ACKed after a lifecycle
  command is applied, duplicated, rejected as stale, or externally cancelled.

JetStream progress ACKs only extend the broker delivery window while a model or
tool call is active. They are not Agent heartbeats and carry no run ownership.

## Run locally

Start PostgreSQL and NATS using the Agent development stack, then start Agent,
the LLM gateway, and the execution gateway:

```sh
docker compose -f apps/agent/compose.yaml up -d
cp apps/pi-harness/.env.example .env.pi-harness
# Set both required service tokens and use the same Agent harness token in
# AGENT_HARNESS_TOKEN and PI_HARNESS_AGENT_TOKEN.
set -a
source .env.pi-harness
set +a
cargo run -p pi-harness
```

Set `PI_HARNESS_AGENT_CONTROL_TOKEN` to the Agent control token to have the
server idempotently register and activate revision
`pi-2026-08-28-deepseek` during startup. Otherwise provision the harness
through Agent before submitting runs.

## Broker topology

| Direction | Subject | Consumer behavior |
| --- | --- | --- |
| Agent → Pi | `agent.harness.pi.turn.requested.v1` | Shared durable pull consumer |
| Pi → Agent | `agent.run.{run_id}.command.v1` | Durable Agent command stream |
| Agent → Pi | `agent.harness.pi.result.v1` | Per-instance result router |
| Agent → Pi | `agent.harness.pi.run.cancelled.v1` | Per-instance cancellation listener |

The result and cancellation listeners are per-instance because command results
must return to the process waiting for them and every replica must observe
cancellations for work it may own.

## Tests

```sh
cargo test -p pi-harness
cargo clippy -p pi-harness --all-targets --all-features
```
