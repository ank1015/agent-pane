# execution-gateway

Cloud-side execution router for registered machines. It persists machine and
operation metadata in PostgreSQL, owns live `machine-daemon` sessions, and
routes the shared `execution-protocol` without adding tool-specific formatting.

## Run

```bash
createdb execution_gateway
set -a; source apps/execution-gateway/.env; set +a
cargo run -p execution-gateway
```

Migrations run automatically at startup. Copy `.env.example` to `.env` first
and replace every token. `EXECUTION_GATEWAY_DAEMON_WEBSOCKET_URL` is the URL
returned to a registering daemon, so it must be reachable from the machine.
`EXECUTION_GATEWAY_CONTROL_TOKEN` is reserved for the platform control plane,
and `EXECUTION_GATEWAY_VAULT_KEY` must be an independent base64-encoded 32-byte
key used only to encrypt stored connector credentials.

## Register and connect a machine

Mint a short-lived, single-use registration token with the admin credential:

```bash
curl -sS -X POST http://127.0.0.1:8790/v1/admin/machine-registrations \
  -H "Authorization: Bearer $EXECUTION_GATEWAY_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"label":"My workstation"}'
```

On the machine, exchange that token and then connect:

```bash
cargo run -p machine-daemon -- --config machine-daemon.json register \
  --gateway http://127.0.0.1:8790 \
  --token "$REGISTRATION_TOKEN"
cargo run -p machine-daemon -- --config machine-daemon.json connect
```

The daemon stores the returned machine-specific credential in its state
directory. Registration rotates the credential if the same machine identity is
registered again. A daemon cannot connect without a registered credential.

## Operational API

All routes below require `EXECUTION_GATEWAY_API_TOKEN` as a bearer token:

- `GET /v1/machines`
- `GET /v1/machines/{machine_id}`
- `POST /v1/machines/{machine_id}/operations`
- `GET /v1/environments/{environment_id}`
- `POST /v1/environments/{environment_id}/operations`
- `GET /v1/operations/{operation_id}`
- `GET /v1/operations/{operation_id}/events?after=0&limit=100`
- `POST /v1/operations/{operation_id}/cancel`

Operations use the tagged `execution_protocol::Operation` JSON shape. A
request is accepted durably and returns an operation record immediately. Unary
results appear on that record; process and artifact streams are stored as
sequence-numbered events for resumption.

Environment operations use the same operation lifecycle and routing as direct
machine operations. Their operation records additionally carry the saved
`environment_id` as provenance.

Example descriptor request:

```bash
curl -sS -X POST \
  http://127.0.0.1:8790/v1/machines/$MACHINE_ID/operations \
  -H "Authorization: Bearer $EXECUTION_GATEWAY_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"operation":{"operation":"describe"}}'
```

`MachineConnector` is the extension point for provider-backed or direct
connectors. The router supports machine-daemon connections and materialized
E2B, Daytona, Blaxel, and Tensorlake sandboxes.

## Environments

An environment is an immutable saved working location consisting of a machine,
a workspace root, and a normalized root-relative path. It is not a security
boundary: operations may address other allowed paths on the machine. Creating
an environment does not require the machine to be online or the path to exist.

The control-plane API requires `EXECUTION_GATEWAY_CONTROL_TOKEN`:

- `GET /v1/control/environments?machine_id={machine_id}`
- `POST /v1/control/environments`
- `GET /v1/control/environments/{environment_id}`
- `DELETE /v1/control/environments/{environment_id}`

Create requests have this shape:

```json
{
  "machine_id": "machine-id",
  "workspace_root_id": "workspace",
  "path": "projects/example"
}
```

Deleting an environment is a soft deletion so existing operation provenance is
retained. Deleting a machine also soft-deletes its active environments.

## Snapshots

Snapshots are immutable metadata records for snapshots retained by E2B,
Daytona, Blaxel, or Tensorlake. A snapshot is permanently bound to one sandbox
account, so materialization always uses the same provider and credentials that
own the provider snapshot ID.

The control-plane API requires `EXECUTION_GATEWAY_CONTROL_TOKEN`:

- `GET /v1/control/snapshots?provider={provider}&sandbox_account_id={account_id}&sandbox_id={sandbox_id}`
- `POST /v1/control/snapshots`
- `GET /v1/control/snapshots/{snapshot_id}`
- `PATCH /v1/control/snapshots/{snapshot_id}`
- `DELETE /v1/control/snapshots/{snapshot_id}`

Create requests have this shape:

```json
{
  "name": "Ready workspace",
  "provider": "e2b",
  "sandbox_account_id": "optional-account-id",
  "provider_snapshot_id": "provider-snapshot-id",
  "sandbox_id": "provider-sandbox-id"
}
```

If `sandbox_account_id` is omitted, the provider's default enabled account is
chosen when the snapshot record is created. Supplying it chooses that account
explicitly. The account must be enabled and must belong to `provider`.

The generic snapshot API manages the gateway's metadata records. Provider-backed
snapshot creation is exposed through provider-specific sandbox routes. Deleting
a snapshot currently removes only the gateway record.

## Sandbox environment templates

A sandbox environment template combines a fixed snapshot with a working
directory beneath the provider's writable workspace root and an optional
creation script. The API represents that root as `/`, so `/project` resolves to
`/home/user/project` on E2B, `/home/daytona/project` on Daytona,
`/blaxel/project` on Blaxel, and `/home/tl-user/project` on Tensorlake.
Materializing a template creates a fresh provider sandbox from the snapshot,
runs the script in the requested directory, and returns a normal environment.
Each call can run independently, allowing many environments from the same
template in parallel.

The control-plane API requires `EXECUTION_GATEWAY_CONTROL_TOKEN`:

- `GET /v1/control/sandbox-environment-templates`
- `POST /v1/control/sandbox-environment-templates`
- `GET /v1/control/sandbox-environment-templates/{template_id}`
- `PATCH /v1/control/sandbox-environment-templates/{template_id}`
- `DELETE /v1/control/sandbox-environment-templates/{template_id}`
- `GET /v1/control/sandbox-environment-templates/{template_id}/environments`
- `POST /v1/control/sandbox-environment-templates/{template_id}/environments`

Create requests have this shape:

```json
{
  "name": "TypeScript workspace",
  "snapshot_id": "snapshot-id",
  "cwd": "/workspace/project",
  "creation_script": "npm install"
}
```

Deleting an environment created from a template also terminates and removes
its provider sandbox. Deleting a persistent-machine environment remains a
metadata-only soft deletion.

## Sandbox accounts

The control-plane sandbox account API requires
`EXECUTION_GATEWAY_CONTROL_TOKEN`:

- `GET /v1/control/sandbox-accounts`
- `POST /v1/control/sandbox-accounts`
- `GET /v1/control/sandbox-accounts/{account_id}`
- `GET /v1/control/sandbox-accounts/{account_id}/sandboxes`
- `POST /v1/control/sandbox-accounts/{account_id}/sandboxes`
- `POST /v1/control/sandbox-accounts/{account_id}/sandboxes/{sandbox_id}/snapshots`
- `PUT /v1/control/sandbox-accounts/{account_id}/credentials`
- `DELETE /v1/control/sandbox-accounts/{account_id}`

Create requests accept `e2b`, `daytona`, `blaxel`, or `tensorlake`:

```json
{
  "provider": "e2b",
  "name": "E2B main",
  "api_key": "secret",
  "config": {},
  "enabled": true,
  "make_default": false
}
```

API keys are encrypted before storage and are never returned by the API.
The optional `config` object holds non-secret provider settings such as the
Blaxel workspace. When `config.workspace` is omitted, the gateway resolves it
automatically if the API key belongs to exactly one workspace. `make_default`
only controls which account is selected when
a snapshot create request omits `sandbox_account_id`; materialization itself
always uses the account fixed on the snapshot.

For an E2B account, create a sandbox and register it as an execution machine
with:

```json
{
  "template_id": "base",
  "name": "Optional machine name"
}
```

For a Daytona account, omit `template_id` because creation uses Daytona's
default sandbox image:

```json
{
  "name": "Optional machine name"
}
```

Blaxel and Tensorlake use the same name-only request. Blaxel creates the current
provider default sandbox image and relies on native scale-to-zero. Tensorlake
creates a named sandbox from its default managed image with a ten-minute idle
suspend interval.

If `name` is omitted, the gateway generates one. The gateway decrypts the API
key belonging to the account in the route, creates the provider sandbox, and
returns the provider sandbox ID together with the new machine summary. Daytona
sandboxes use a 15-minute auto-stop interval. Any later execution operation
starts a stopped Daytona sandbox or resumes a suspended Tensorlake sandbox
before connecting to it.

Creating an E2B, Daytona, Blaxel, or Tensorlake snapshot uses the source
provider sandbox ID in the route and accepts a display name:

```json
{
  "name": "Ready workspace"
}
```

The gateway verifies that the source sandbox belongs to the selected account,
creates the provider snapshot with that account's credentials, and stores the
returned provider snapshot ID in `snapshots`. Daytona cold snapshots stop the
sandbox first and start it again afterward. Tensorlake creates a filesystem
snapshot and waits until it is restorable. The supplied name remains gateway
display metadata. Blaxel snapshot and fork APIs are currently private preview
and require that Blaxel enable the feature for the account's workspace.
