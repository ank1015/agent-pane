# environment-harness

`environment-harness` is the long-running server for the `environment` harness. It creates
and updates project execution environments. The service consumes durable turn requests
from NATS JetStream, runs up to a configured number of environment
workers inside the process, and publishes exactly one lifecycle operation back
to Agent: `complete`, `continue`, `fail`, or `wait`.

The binary uses `packages/agent-harness-sdk` for the reusable broker server,
cancellation, command-result routing, and Agent transcript access. Environment-specific
model, context, compaction, tool, and execution behavior remains in this app.

Agent does not poll this process and this process does not claim or lease runs.
The broker provides at-least-once delivery; Agent state versions and stable
command IDs make redelivery safe.

## Runtime ownership

- A shared durable consumer load-balances `environment` turns across harness replicas.
- Each server executes up to `ENVIRONMENT_HARNESS_MAX_CONCURRENT_TURNS` turns concurrently.
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

## Tool targeting

`get_tunnel_machines_list` takes no arguments and returns connected, online
machine-daemon tunnels from the execution gateway. Each result contains the
machine name, machine ID, and all workspace roots advertised by that machine.

`get_sandbox_accounts_list` also takes no arguments and returns every configured
sandbox account with its account name, provider name, and account ID. The
execution gateway exposes only this minimal metadata to the harness.

`list_environments` takes no arguments and lists the tunnel-machine and sandbox
template environments belonging to the `project_id` in the resolved run
configuration. It preserves the execution gateway project-environment response
shape, including `machine_id` on tunnels and `snapshot_id` plus `setup_script`
on templates. Paths for both types are workspace-root-relative.

`create_sandbox` requires `account_id` and accepts an optional `snapshot_id`.
Without a snapshot it creates the provider's base sandbox; with a snapshot it
restores a new sandbox from that saved state. Its JSON result reports
`successful` with the machine ID and workspace root. Failures use a structured
tool error.

`snapshot_sandbox` requires the `machine_id` of an existing sandbox machine.
It saves the machine's current provider state and reports `successful` with the
new snapshot ID. Failures use a structured tool error. The execution gateway resolves the
sandbox account and provider resource from the machine ID.

`create_tunnel_machine_environment` requires `name`, `machine_id`, and `path`.
The harness takes `project_id` from the resolved run configuration, verifies
that the machine is a tunnel, and uses its sole workspace root. Its JSON result
reports `successful` with the environment ID, name, normalized path, host name,
and creation timestamp. Failures use a structured tool error.

`create_sandbox_template_environment` requires `snapshot_id`, `name`, and an
workspace-root-relative sandbox `path`, and accepts an optional `setup_script`. The harness
takes `project_id` from the resolved run configuration and maps the setup script
to the gateway's template creation script. Its result matches the tunnel
environment summary and additionally includes the snapshot ID.

`update_environment` requires an environment ID from `list_environments`.
Tunnel environments can update their name, relative path, or machine. Sandbox
templates can update their name, relative path, snapshot, or setup script. The
hidden project scope is enforced by the gateway on every update.

The `read`, `bash`, `edit`, and `write` tools each require a `machineId`. The
harness resolves that machine through the execution gateway and executes from
its workspace root (`cwd = "."`). `machineId` is harness routing metadata and
is removed before the remaining arguments are passed to the reusable Pi tool
executors.

A machine must expose exactly one workspace root. Calls targeting a machine
with no root or multiple roots return a structured tool error; no root is
selected implicitly when the target is ambiguous.

## Run locally

Start PostgreSQL and NATS using the Agent development stack, then start Agent,
the LLM gateway, and the execution gateway:

```sh
docker compose -f apps/agent/compose.yaml up -d
cp apps/environment-harness/.env.example .env.environment-harness
# Set both required service tokens and use the same Agent harness token in
# AGENT_HARNESS_TOKEN and ENVIRONMENT_HARNESS_AGENT_TOKEN.
set -a
source .env.environment-harness
set +a
cargo run -p environment-harness
```

Set `ENVIRONMENT_HARNESS_AGENT_CONTROL_TOKEN` to the Agent control token to have the
server idempotently register and activate revision
`environment-2026-09-01-web-tools` during startup. Otherwise provision the harness
through Agent before submitting runs.

## Broker topology

| Direction | Subject | Consumer behavior |
| --- | --- | --- |
| Agent → Environment | `agent.harness.environment.turn.requested.v1` | Shared durable pull consumer |
| Environment → Agent | `agent.run.{run_id}.command.v1` | Durable Agent command stream |
| Agent → Environment | `agent.harness.environment.result.v1` | Per-instance result router |
| Agent → Environment | `agent.harness.environment.run.cancelled.v1` | Per-instance cancellation listener |

The result and cancellation listeners are per-instance because command results
must return to the process waiting for them and every replica must observe
cancellations for work it may own.

## Tests

```sh
cargo test -p environment-harness
cargo clippy -p environment-harness --all-targets --all-features
```
