# machine-daemon

Thin deployable process exposing `execution-local` over structured transports.
It performs no Codex-, Pi-, or OpenCode-specific formatting.

## Commands

```bash
cargo run -p machine-daemon -- --config machine-daemon.json describe
cargo run -p machine-daemon -- --config machine-daemon.json doctor
cargo run -p machine-daemon -- --config machine-daemon.json register \
  --gateway https://gateway.example.com --token "$REGISTRATION_TOKEN"
cargo run -p machine-daemon -- --config machine-daemon.json serve
cargo run -p machine-daemon -- --config machine-daemon.json connect
cargo run -p machine-daemon -- --config machine-daemon.json stdio
```

Copy `machine-daemon.example.json` and configure explicit workspace roots. If
`machine_id` is omitted, a UUID is generated once and stored under the state
directory. Set `MACHINE_DAEMON_TOKEN` to require bearer authentication for
inbound `serve` connections.

`register` exchanges a short-lived registration token for a machine-specific
credential and saves it as `cloud-credential.json` under the state directory
(mode `0600` on Unix). `connect` requires this registration and uses the saved
WebSocket URL and credential. `--gateway` on `connect` can override only the
saved WebSocket URL; it does not bypass registration.

`serve` exposes:

- `GET /health`
- `GET /v1/descriptor`
- `GET /v1/ws`

`stdio` uses newline-delimited JSON. WebSockets use one JSON object per text
frame. Every session begins with a `ready` message containing the protocol name
and environment descriptor. Requests use a caller-generated `request_id` and a
tagged operation. Unary operations produce `response`; process attachment and
artifact opening produce `stream_item` messages followed by `stream_end`.

Cancellation is explicit:

```json
{"type":"cancel","request_id":"request-1"}
```

Logs always go to stderr, keeping stdout protocol-safe in `stdio` mode.
