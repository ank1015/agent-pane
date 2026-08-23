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
- `GET /v1/operations/{operation_id}`
- `GET /v1/operations/{operation_id}/events?after=0&limit=100`
- `POST /v1/operations/{operation_id}/cancel`

Operations use the tagged `execution_protocol::Operation` JSON shape. A
request is accepted durably and returns an operation record immediately. Unary
results appear on that record; process and artifact streams are stored as
sequence-numbered events for resumption.

Example descriptor request:

```bash
curl -sS -X POST \
  http://127.0.0.1:8790/v1/machines/$MACHINE_ID/operations \
  -H "Authorization: Bearer $EXECUTION_GATEWAY_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"operation":{"operation":"describe"}}'
```

`MachineConnector` is the extension point for provider-backed or direct
connectors. The router already recognizes `machine_daemon`, `e2b`, and `ssh`;
the latter two return an explicit unsupported error until their connectors are
installed.

## Sandbox accounts

The control-plane sandbox account API requires
`EXECUTION_GATEWAY_CONTROL_TOKEN`:

- `GET /v1/control/sandbox-accounts`
- `POST /v1/control/sandbox-accounts`
- `GET /v1/control/sandbox-accounts/{account_id}`
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
