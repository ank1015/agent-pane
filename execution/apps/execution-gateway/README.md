# execution-gateway

Hosted control plane and execution router for the standalone execution system.

The gateway supports E2B hosts and user-owned Registered Hosts. It creates E2B
hosts from the fixed execution base image or gateway-managed snapshots, accepts
authenticated outbound Host Daemon connections, and routes the same
`execution-wire` operations through either transport.

## Responsibilities

- Encrypt and store multiple E2B API credentials.
- Track logical hosts separately from E2B resource IDs.
- Create, inspect, pause, resume, refresh, and delete E2B hosts.
- Create, track, and delete E2B snapshots.
- Issue single-use Registered Host enrollment tokens and hashed machine credentials.
- Maintain authenticated outbound Host Daemon WebSockets and online state.
- Route exact filesystem and process operations by logical host ID.
- Recover pending lifecycle work after a gateway restart.
- Prevent duplicate host and snapshot creation with idempotency keys.

The gateway does not own PTYs, child processes, process output, or filesystem
semantics. Those remain owned by `execution-supervisor` inside the execution
host.

## Configuration

Copy `.env.example` and provide:

- `EXECUTION_GATEWAY_DATABASE_URL`: PostgreSQL connection URL.
- `EXECUTION_GATEWAY_API_TOKEN`: bearer token for management and execution
  endpoints. Registration claim and Host Daemon connection endpoints use their
  scoped credentials instead.
- `EXECUTION_GATEWAY_VAULT_KEY`: base64 encoding of exactly 32 random bytes.
- `EXECUTION_GATEWAY_PUBLIC_URL`: externally reachable gateway URL used to
  construct the Host Daemon WebSocket URL.

Production public URLs must use HTTPS. Plain HTTP is rejected unless
`EXECUTION_GATEWAY_ALLOW_INSECURE_HTTP=true`, which is intended only for local
development and tests.

Generate a vault key with:

```sh
openssl rand -base64 32
```

Start the gateway from the `execution/` workspace:

```sh
cargo run -p execution-gateway
```

Migrations run automatically at startup. Authenticated `GET /healthz` checks
the process; authenticated `GET /readyz` additionally checks PostgreSQL.

The gateway bounds total concurrent execution operations, per-host Registered
Host operations, HTTP body sizes, authentication failures, and enrollment or
WebSocket entry attempts. Every HTTP response receives a request ID and
`Cache-Control: no-store`; logs contain request metadata but never request
bodies, credentials, or operation payloads.

## Host creation boundary

Arbitrary E2B template IDs are deliberately not part of the API. A host can be
created from exactly one of these sources:

```json
{
  "name": "clean-host",
  "source": {
    "type": "base",
    "e2b_account_id": "019...",
    "ram": 2048
  },
  "timeout_seconds": 3600,
  "network_access": true
}
```

`e2b_account_id` may be omitted when a default account exists. `ram` is optional,
defaults to `2048` MiB, and accepts `1024`, `2048`, `4096`, or `8192`. The
gateway resolves these tiers to public E2B templates with 1, 2, 2, and 4 vCPUs,
respectively. Every image contains `/usr/local/bin/execution-supervisor`.
Snapshot creation does not accept `ram`; restored sandboxes retain the snapshot's
hardware specification.

`network_access` is optional for both base and snapshot sources and defaults to
`true`. When false, the gateway sends E2B `allow_internet_access: false`.
`timeout_seconds` is the running window before E2B auto-pauses the sandbox, not
a retention deadline. Sandboxes preserve memory and filesystem state while
paused, auto-resume on activity, and remain until explicitly deleted.

`POST /v1/hosts` returns `202 Accepted` with the host resource. Its top-level
`roots` array is available immediately, including while `state` is `provisioning`
and `descriptor` is null:

```json
[{"id":"workspace","name":"Workspace","native_path":"/home/user","read_only":false}]
```

These roots use the same supervisor configuration as sandbox startup. Once a
descriptor is available, `roots` reflects its reported roots. Registered hosts
return an empty array until their descriptor is available. Root availability
does not imply readiness; wait for `state: "ready"` before executing operations.

Or create from a snapshot known to this gateway:

```json
{
  "name": "restored-host",
  "source": {
    "type": "snapshot",
    "snapshot_id": "019..."
  },
  "network_access": false
}
```

The gateway resolves the E2B account and provider snapshot ID from its database.
It never accepts a caller-provided provider snapshot ID.

## API

Management and execution requests use:

```text
Authorization: Bearer <EXECUTION_GATEWAY_API_TOKEN>
```

Host and snapshot creation additionally require a stable `Idempotency-Key`
header. Reusing the key with the same request returns the original logical
resource; reusing it with a different request returns `409`.

Registered Host creation also requires `Idempotency-Key`. Replaying the request
returns the same host with a newly issued registration token and invalidates the
previous unclaimed token.

### Registered Hosts

| Method | Path | Authentication | Purpose |
|---|---|---|---|
| `POST` | `/v1/registered-hosts` | Gateway API token | Create a logical host and single-use registration token |
| `POST` | `/v1/registered-hosts/{id}/registration-token` | Gateway API token | Reissue a registration token |
| `POST` | `/v1/registered-hosts/claim` | Registration token in body | Exchange the token for a machine credential |
| `GET` | `/v1/registered-hosts/{id}/connect` | Machine bearer credential | Upgrade to the Host Daemon WebSocket |

Creating a Registered Host leaves it in `provisioning`. A successful Host
Daemon handshake changes it to `ready`; a disconnect changes it to
`unavailable`; reconnecting returns it to `ready`. Deleting the host revokes its
credential and closes the connection but does not shut down or delete the
physical machine.

Registered Hosts do not support pause, resume, or snapshots. Those endpoints
return `HOST_OPERATION_UNSUPPORTED`.

### E2B accounts

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/v1/e2b-accounts` | Verify, encrypt, and store an E2B credential |
| `GET` | `/v1/e2b-accounts` | List accounts without credentials |
| `GET` | `/v1/e2b-accounts/{id}` | Get an account |
| `PATCH` | `/v1/e2b-accounts/{id}` | Change name, enabled state, or default status |
| `PUT` | `/v1/e2b-accounts/{id}/credential` | Verify and rotate the credential |
| `POST` | `/v1/e2b-accounts/{id}/verify` | Reverify the stored credential |
| `DELETE` | `/v1/e2b-accounts/{id}` | Delete an account that has never owned resources |

Accounts referenced by host or snapshot history cannot be deleted; disable them
instead. Credential material is never returned by an endpoint.

### Hosts

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/v1/hosts` | Create from `base` or a gateway snapshot |
| `GET` | `/v1/hosts` | List/filter hosts |
| `GET` | `/v1/hosts/{id}` | Get lifecycle and provider binding state |
| `PATCH` | `/v1/hosts/{id}` | Change name or metadata |
| `POST` | `/v1/hosts/{id}/pause` | Request pause |
| `POST` | `/v1/hosts/{id}/resume` | Request resume and supervisor handshake |
| `POST` | `/v1/hosts/{id}/refresh` | Reconcile the stored state with E2B now |
| `DELETE` | `/v1/hosts/{id}` | Request provider deletion |

Lifecycle mutations return `202` with the logical resource. Read the resource
until it reaches `ready`, `paused`, `deleted`, `failed`, or `lost`.

List filters are `state`, `kind`, `e2b_account_id`, and `include_deleted`.

### Snapshots

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/v1/hosts/{host_id}/snapshots` | Snapshot a ready host |
| `GET` | `/v1/snapshots` | List/filter snapshots |
| `GET` | `/v1/snapshots/{id}` | Get snapshot state |
| `PATCH` | `/v1/snapshots/{id}` | Change name or metadata |
| `DELETE` | `/v1/snapshots/{id}` | Request provider deletion |

E2B pauses a host when creating its snapshot. After successful snapshot
creation, the gateway records the source host as `paused`. The next execution
operation (including a Describe/SDK connect) requests resume and waits for the
supervisor handshake before forwarding the operation. No explicit resume tool
is required. The gateway waits up to 20 seconds; if still resuming it returns
retryable `503 HOST_RESUMING` without dispatching the requested operation.
Deleted hosts and pauses/snapshots in progress are not automatically overridden.
The explicit resume endpoint remains available for callers that want to start
compute before executing anything. Snapshot retries with the same key and body
return the original snapshot even after its source pauses or is deleted; a
different body with that key conflicts. This replay check precedes mutable host
validation inside the snapshot transaction.

List filters are `state`, `e2b_account_id`, `source_host_id`, and
`include_deleted`.

### Execution

`POST /v1/hosts/{host_id}/operations` accepts an
`execution_wire::RequestEnvelope` and returns an
`execution_wire::ResponseEnvelope`. It supports every operation in protocol
version 1:

- `describe`
- filesystem stat, read, write, create-directory, remove, and list
- process start, read, write, resize, signal, and terminate

Gateway routing failures use non-2xx HTTP responses. Once a request reaches the
supervisor, operation success or failure is represented by the wire envelope
with HTTP `200`.

## Persistence

The initial migration creates:

- `e2b_accounts`
- `execution_hosts`
- `e2b_hosts`
- `registered_hosts`
- `e2b_snapshots`
- `idempotency_records`

Desired and observed lifecycle states are separate. Reconciliation work uses
short database leases and `FOR UPDATE SKIP LOCKED`, allowing multiple gateway
replicas without performing the same lifecycle action concurrently.

Active Host Daemon sockets and their in-flight request correlation live in the
gateway process, not PostgreSQL. The initial Registered Host transport therefore
requires one active gateway replica. A later multi-replica deployment needs a
connection-owning router or cross-replica message transport.

## Verification

Run all local checks:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The ignored gateway integration test runs real migrations and a complete HTTP
flow against PostgreSQL while using an in-process fake E2B provider:

```sh
EXECUTION_GATEWAY_TEST_DATABASE_URL=postgresql:///execution_gateway_test \
  cargo test -p execution-gateway --test gateway -- --ignored --nocapture
```

The live test drives the hosted HTTP API against real E2B resources. It runs
the shared conformance suite on both a base host and a host restored from a
gateway-managed snapshot, then verifies pause, resume, and provider cleanup:

```sh
E2B_API_KEY=... \
EXECUTION_GATEWAY_TEST_DATABASE_URL=postgresql:///execution_gateway_live_e2b \
  cargo test -p execution-gateway --test live_e2b -- --ignored --nocapture
```

The Registered Host integration test uses PostgreSQL, a real embedded
supervisor, the WebSocket transport, the shared conformance suite, and a forced
disconnect/reconnect:

```sh
EXECUTION_GATEWAY_TEST_DATABASE_URL=postgresql:///execution_gateway_test \
  cargo test -p execution-gateway --test registered_host -- --ignored --nocapture
```

The final Registered Host black-box test launches the production
`execution-host` binary and routes every version 1 execution operation through
the gateway. Use an isolated database because the test truncates gateway
tables:

```sh
EXECUTION_GATEWAY_TEST_DATABASE_URL=postgresql:///execution_gateway_daemon_e2e \
  cargo test -p execution-host-daemon --test registered_host_e2e \
  -- --ignored --nocapture
```
