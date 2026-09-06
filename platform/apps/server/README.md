# Platform server

## Project landing bootstrap

`GET /api/projects/{project_id}/bootstrap` returns exactly three arrays:

```json
{"harnesses": [], "provider_accounts": [], "project_environments": []}
```

`harnesses` contains only globally enabled entries available to this project
(required platform harnesses or explicit project opt-ins), with the same fields as
`GET /api/harnesses` items (including `config_schema`, `default_config`, and
`supported_models`). Catalog availability does not guarantee an online worker.
`provider_accounts` contains the same credential-free summaries as
`GET /api/providers`, including disabled/reauth-required accounts and their status;
the UI should only offer usable accounts matching the selected harness provider.
`project_environments` contains that project's existing environment records,
including native workspace-root paths. No host or snapshot discovery is performed.

Malformed project UUIDs return 400; missing projects return 404 before contacting
the LLM gateway. After project validation, independent reads run concurrently
without holding a database transaction across the gateway request. A failed source
fails the request (no partial-success empty arrays); existing sanitized gateway
timeout/status handling applies. Every response has `Cache-Control: no-store`.
The frontend can cache this GET through its query library. This is a fresh
aggregate, not a cross-service transactional snapshot.

## Project harness availability

`harnesses.project_policy` is `required` or `opt_in` (new registrations default
to `opt_in`). Environments is required for every project. The existing `enabled`
field is a global switch and overrides project enablement. Only the authenticated
platform catalogue administration API sets policy; worker registration cannot.
`PUT /internal/harnesses/{id}` accepts `project_policy`, preserving an existing
policy when omitted. `PATCH` can explicitly update it.

`GET /api/projects/{project_id}/harnesses` returns `{ "items": [...] }`, including
disabled optional harnesses for a future settings UI. Each item contains the usual
harness fields plus `globally_enabled` (an explicit alias of `enabled`),
`project_enabled`, and `available`. `project_enabled` is true for required
harnesses or an enabled project grant; `available` additionally requires the
global flag. An offline worker does not change these policy fields.

`PUT /api/projects/{project_id}/harnesses/{harness_id}` accepts exactly
`{ "enabled": true }` or `{ "enabled": false }` and returns the updated item.
It is an idempotent set operation; no Idempotency-Key header is required.
Enabling a required harness is a no-op. Enabling a globally disabled harness
stores the project preference, but does not make it available until globally
enabled again. Missing projects/harnesses return 404; invalid input is rejected.
Responses are `Cache-Control: no-store`.

Conflicts return HTTP 409 with these stable codes:

- `PROJECT_HARNESS_REQUIRED`: a project cannot disable a required harness.
- `PROJECT_HARNESS_ACTIVE_RUNS`: finish or abort this project's ready (including
  delayed), running, and waiting runs on that harness before disabling it.
- `PROJECT_HARNESS_DISABLED`: opt in before creating a session, forking into the
  harness, or starting a new run, including worker child/follow-up requests.
- `HARNESS_DISABLED`: the global catalogue switch prevents new work.

The indexed `project_harnesses` association stores one boolean per project/harness.
No row means no opt-in. Required harnesses do not need association rows. The
migration enables optional harnesses only for projects with existing sessions
using them, including archived sessions; future projects receive no optional
grants. Session configurations and run snapshots are not modified.

Admission checks hold shared project locks through transaction commit. Settings
writes take an exclusive project lock and check active runs without taking
session/run locks, so a concurrent disable cannot strand newly admitted work.
Existing run recovery, wait results, aborts, and worker commits remain allowed.
Historical sessions/messages/runs remain readable after disabling; replaying an
already accepted idempotent request returns its original result. Re-enabling
allows old sessions to continue with their frozen configuration.

These routes share the existing single-user dashboard API boundary, not a new
multi-user authorization system. Add project membership checks before public
multi-user deployment. The future settings UI should invalidate project harness
and bootstrap queries after updates. No frontend controls are implemented here.

Like the existing dashboard APIs, this endpoint is local-development/single-user:
provider accounts are gateway-wide, not filtered by an authenticated user. Add
user/project authorization before exposing it to multiple users. This endpoint
does not create sessions/runs. The project landing UI consumes it with a
project-scoped query cache, bounded transient retries, and foreground/focus/reconnect
refreshes. It intersects enabled accounts with harness model capabilities and
derives reasoning/web-search controls from the selected harness schema. The
`environments` harness hides the environment picker. Starting runs is not wired yet.

## Harness model capabilities

Harness catalog records returned by `GET /api/harnesses` and
`GET /api/harnesses/{id}` include `supported_models`, a JSON object mapping
provider IDs to explicit model-ID arrays, alongside `config_schema` and
`default_config`. For example: `{"openai":["gpt-5.6-sol"]}`.

Authenticated `PUT /internal/harnesses/{id}` accepts this field (omission defaults
to `{}`); `PATCH` replaces the whole map when supplied and preserves it otherwise.
Empty maps mean no declared capabilities, not support for every model. Provider
keys use the same lowercase ID syntax as harness IDs. Model IDs must be nonempty,
unique within a provider, at most 512 characters, and free of surrounding
whitespace/control characters. Maps are limited to 64 providers, 1,000 models per
provider, and 64 KiB. These are catalog metadata, not account availability or new
run-validation rules; the harness still validates its own configuration.

The environments migration seeds its map from the current compiled catalogs.
Its package exports `supported_models()` and a parity test detects catalog drift;
update registration metadata with a new migration or the admin API when changing
those catalogs. Worker registration continues to advertise only harness IDs.

The Rust API used by the platform dashboard.

## Machines API

`GET /api/machines` returns the active execution hosts from the hosted execution
gateway. The response is a JSON array using the stable `execution-api`
`ExecutionHost` contract. Its `kind` field distinguishes managed Registered
Hosts (`registered`) from sandbox hosts (`e2b`).

The dashboard displays registered hosts only, plus sandbox **accounts** from
`GET /api/machines/e2b-accounts`. Sandbox hosts are not displayed.

Registered machine cards have a three-dot menu with Update Name and Delete:

- `PATCH /api/machines/{id}` accepts only `{ "name": "New name" }` and returns
  the updated gateway host with HTTP 200. This changes the gateway display name,
  not the computer's OS hostname. Names contain 1–200 characters without
  surrounding whitespace or control characters.
- `DELETE /api/machines/{id}` returns HTTP 204 after the gateway reports the
  registration deleted. Deleting an already-deleted registered host also returns
  204. This disconnects its tunnel and revokes its credentials; reconnecting
  requires registering the machine again. It does not delete files, uninstall
  the daemon, or delete project environment records referencing the machine.

Both routes first check the gateway host is a registered machine, rejecting E2B
sandbox hosts. They use the configured bearer credential, bounded 1 MiB host
responses, timeouts, no redirects/retries, sanitized errors, UUID validation,
16 KiB request limits, and no-store responses. A missing host returns 404.
After a timeout or an uncertain failure, refresh the list before retrying.
The UI asks for delete confirmation, prevents duplicate submissions, pauses
inventory refreshes during mutations, updates the cache optimistically, restores
failed changes, and revalidates the list afterward.

`POST /api/machines/e2b-accounts` accepts `{ "name": "My account", "api_key": "..." }`
and returns `201` with the gateway's `E2bAccount` record (never the API key).
The gateway verifies the key with E2B and encrypts it for storage. This does not
create a sandbox host. Creating accounts is not retried automatically; after a
timeout, refresh the account list before attempting creation again.

Requests are limited to 16 KiB, names to 200 characters, and keys to 4096 bytes.
Unknown fields and invalid input are rejected locally. Gateway failures use
sanitized error messages and logs. Account credentials are not stored in this
server's configuration. These local-development routes are unauthenticated;
add application authentication/authorization before exposing the server publicly.

## Providers API

`GET /api/providers` reads **all** configured accounts from the hosted LLM
gateway's `GET /v1/admin/accounts`. No provider or status filter is applied.
The response is a JSON array of dashboard summaries:

```json
[
  {
    "id": "018f47a8-80cc-7b2f-9d44-6657f5f82ad0",
    "name": "Personal",
    "provider": "openai",
    "status": "enabled",
    "created_at": "2026-09-03T00:00:00Z",
    "is_default": true
  }
]
```

Supported providers mirror the new gateway: `openai`, `chatgpt`, and `fireworks`.
Gateway `active` maps to the old dashboard's `enabled`; `disabled` and
`reauth_required` are preserved. Accounts sort alphabetically by provider,
then newest creation date first, then ID. An empty inventory returns `[]`.
Config, credentials, and encryption metadata are never included.

`GET /api/providers/{id}` fetches one UUID-addressed account from the gateway
and returns `{ "provider": { ... } }`, matching the previous dashboard's detail
response wrapper. The detail contains provider configuration, mapped status,
default/runtime fields, timestamps, and credential **metadata** such as version
and expiry. It never contains API keys, OAuth tokens, or encrypted credential
material. Invalid IDs return `400`, and a missing gateway account returns a
sanitized `404`.

`POST /api/providers` accepts an API-key account as
`{ "provider": "openai" | "fireworks", "name": "Personal", "api_key": "..." }`
and returns `201` with the dashboard summary. The key is forwarded directly to
the gateway and is never returned or retained by the platform server. Account
creation is not retried automatically. Requests are limited to 16 KiB, names to
200 characters, and keys to 4096 bytes; unknown fields and invalid values are
rejected before the gateway is called.

ChatGPT accounts use browser authentication rather than an API-key payload:

- `POST /api/providers/chatgpt/login` starts a single-use PKCE login.
- `GET /api/providers/chatgpt/login/{id}` reports only pending, exchanging,
  succeeded, or a sanitized failure.
- `DELETE /api/providers/chatgpt/login/{id}` cancels a pending login.
- `http://localhost:1455/auth/callback` exchanges the authorization code and
  creates the gateway account without exposing tokens to the dashboard.

Port `1455` must be available when the server starts. Login attempts expire
after 15 minutes and are bounded in memory. Once token exchange begins it is
not reported as canceled, because the upstream account write may already be in
flight. This follows OpenAI's documented browser-login pattern, where the
browser returns credentials through a localhost callback:
<https://learn.chatgpt.com/docs/auth#sign-in-with-chatgpt>.

The server uses `PLATFORM_SERVER_LLM_GATEWAY_ADMIN_TOKEN`, **not** the runtime
token or a GCP identity token. The admin secret is named
`llm-gateway-admin-token` in GCP Secret Manager. The local `.env` is ignored and
should remain mode `0600`; never put this token in dashboard/Vite variables.

`PLATFORM_SERVER_LLM_GATEWAY_URL` points to `https://llm.acentric.dev/`.
`PLATFORM_SERVER_LLM_GATEWAY_TIMEOUT_SECONDS` defaults to 30. HTTPS is required
except for loopback test servers; redirects are not followed. Responses are
capped at 4 MiB and marked `Cache-Control: no-store`, including errors. Upstream
authentication failures, malformed responses, and transport errors produce a
sanitized `502`; timeouts produce `504`, and upstream rate limits produce `503`.
The module is split into client, model, service, HTTP routes, and error handling
under `src/providers/`. The Providers dashboard page consumes this list endpoint.

## Provider analytics

- `GET /api/providers/{id}/usage` returns all-time request, success/failure and
  usage-record counts, token breakdowns, and USD cost breakdowns for that account.
  The platform maps the gateway's `totals` into a flat response with `account_id`.
- `GET /api/providers/{id}/requests?limit=25&cursor=...` returns `items` and
  `next_cursor`. Limits are 1–100; cursors are opaque and URL-encoded.
  Request DTOs include model, status, timestamps, and optional usage/costs,
  but omit labels, prompts, and raw provider errors.

Both routes use the existing admin credential, timeouts, response-size cap,
no-redirect policy, sanitized errors and `Cache-Control: no-store`.
Missing accounts return 404. These are gateway-recorded totals, not external
provider billing or activity performed outside this gateway.

The overview mirrors the previous dashboard's seven usage cards and paginated
requests table, with a shared Cost/Tokens switch. Queries refresh every five
seconds in the foreground and on focus/reconnect, retain cached data after
refresh errors, and abort requests when no longer observed.

## Project environments API

Environments are saved project-owned records, not running machines or sandboxes.
The `project_environments` table lives in the platform database. Gateway IDs are
external references, not cross-database foreign keys.

All routes are under `/api/projects/{project_id}/environments`:

- `GET /` lists records alphabetically by name, then ID (an empty project returns `[]`).
- `POST /` creates a record and returns it with HTTP 201.
- `GET /{id}` returns one record.
- `PATCH /{id}` updates supplied fields and returns the record.
- `DELETE /{id}` removes only the record and returns HTTP 204.

Example create body:

```json
{
  "name": "Development",
  "type": "machine",
  "machine_id": "018f47a8-80cc-7b2f-9d44-6657f5f82ad0",
  "snapshot_id": null,
  "workspace_root": "/home/user",
  "path": "."
}
```

Responses add `id`, `project_id`, `created_at`, and `updated_at`. A `machine`
requires only `machine_id`; a `sandbox` requires only `snapshot_id` (the gateway
snapshot record UUID, not an external E2B identifier). The unused reference is
null. Identity, project ownership, and type cannot be changed. Names need not be
unique and are limited to 128 characters. `workspace_root` is the absolute native
workspace path (POSIX, Windows drive, or UNC), limited to 4096 bytes; `path` is
relative to it, with `.` selecting the root. Machine roots must match the gateway
descriptor. Sandbox paths are supplied from discovery and verified against the
restored host when used; registration never provisions compute. Renames do not
contact the gateway. The API has no root-ID or `workspace_root_path` field.

Before upgrading an existing database, run `node scripts/backfill-environment-roots.mjs`
from this server folder, then start the server to apply migrations. The preflight
uses gateway metadata to fill unresolved legacy root paths without waking sandboxes.
It stops on unknown roots; the migration also refuses unresolved records rather
than guessing paths. Session/run history stays immutable; the basic harness privately
supports legacy root-ID configurations.
Paths use execution-core validation: `.` means the
root itself; absolute paths, traversal, backslashes, and empty segments are rejected.

Creation and target/root/path updates validate references with read-only gateway
requests. Machines must be registered, non-deleted, and advertise the root in
their last descriptor; being offline is fine. Snapshots must be ready and
non-deleted. Snapshot records do not advertise roots, so sandbox root/path
existence must be checked when execution is implemented. No endpoint provisions
a sandbox, checks directory existence, or changes files. Listing, reading,
renaming, and deleting do not require the gateway to be available.

Database constraints enforce the type/reference pairing and immutable identity;
project deletion cascades to its environment records. An indexed project foreign
key supports scoped listing. Concurrent conflicting updates return HTTP 409;
refetch before retrying. Missing project/environment records return 404, invalid
references return 422, and gateway failures return sanitized 502/504 errors.
Unknown fields and empty patches are rejected; explicit null is only accepted
for the nullable reference fields, subject to the type constraint. Requests are
limited to 16 KiB and responses use `Cache-Control: no-store`.

Database-backed environment tests use isolated SQLx test databases (the role
must have `CREATEDB`), not the application's database:

```sh
DATABASE_URL=postgresql://localhost/postgres cargo test -p platform-server --test environments -- --include-ignored
```

## Agent runtime storage

The agent runtime tables live in the same dedicated platform database as projects
and environments. Application routes, worker lifecycle/coordination routes, and
the durable-wait/lease reconciler are implemented in `src/runtime/`. See
[Runtime application API](docs/runtime-api.md) and
[Worker runtime API](docs/worker-runtime-api.md), and
[Administration and background work](docs/runtime-admin-api.md) for request contracts and recovery
semantics. The shared worker executable, Rust SDK, harness execution, and fleet
deployment/autoscaling remain separate work.

| Table | Purpose |
| --- | --- |
| `harnesses` | Registered implementation IDs, configuration defaults, and availability |
| `workers` | Build, supported harnesses, capacity, and heartbeat metadata |
| `sessions` | Project-owned history, a fixed harness and resolved configuration, and optional fork provenance |
| `runs` | Parent/child relationships, configuration, scheduling, leases, and lifecycle |
| `messages` | Immutable common-envelope message content, including custom messages |
| `session_messages` | Ordered history membership; forks share message records |
| `run_checkpoints` | Latest versioned harness-owned recovery state |
| `run_inputs` | Ordered, deduplicated inputs and their handling outcomes |
| `run_waits` | Durable any/all waits, deadlines, and resolution |
| `run_wait_dependencies` | Run completion, input, timer, or external-operation conditions |
| `run_events` | Append-only, ordered runtime and harness events |
| `runtime_requests` | Scoped idempotency receipts committed with their effects |
| `worker_credentials` | Hashed per-process credentials, separate from metadata |
| `worker_claim_requests` | Idempotent assignment-allocation receipts |

See [CHAT_RUNTIME_API.md](CHAT_RUNTIME_API.md) for session creation/fork configuration,
accepted-input reconciliation, harness-owned environment resolution, and live chat reads.

The migration enforces project-scoped relationships, one live run per session,
immutable history and identity, exact fork prefixes, monotonic versions and lease
epochs, checkpoint ownership, valid lifecycle field combinations, and typed wait
dependencies. Queue, lease-expiry, pending-input, timer, and history indexes cover
the expected access paths. Repeated `project_id` fields support composite foreign
keys; they are not independently editable ownership fields. Projects with runtime
history cannot be deleted through a cascading delete.

Append message memberships, inputs, and events without supplying their sequence
or revision; triggers allocate the next value while locking the owning row.
Fork creation must copy the source memberships through the requested revision in
the same transaction, with no child-run attribution on inherited memberships.
This copies history references, not checkpoints, machine files, or runtime state.

Deferred constraints check the final transaction state. Completing a run requires
its own final assistant message without tool calls, a matching runtime event, and
no pending waits. Waiting requires a persisted dependency; resolving its final
wait must also wake the suspended run in the same transaction. Abort acknowledgement
requires an abort request, terminal event, and resolution/cancellation of local
waits; it does not automatically stop children.

The worker interface authenticates process credentials and unexpired lease epochs,
validates message contracts/configuration, enforces worker capacity, and commits
checkpoints, input acknowledgements, history, receipts, and lifecycle events
transactionally. Each Platform process runs a bounded, replica-safe reconciliation
loop for expired leases, wait conditions, and worker liveness. Claims use database
locks and `SKIP LOCKED`; notifications are hints, never the durable work queue.
Persist external operation handles in checkpoint state for harness-specific
recovery. These tables do not make gateway operations exactly-once.

Resolve idempotent replays before appending, under the same transaction locks;
do not use `ON CONFLICT DO NOTHING` as a substitute for replay handling after an
append trigger advances a counter. Compare a receipt's request hash and return
its saved result. Run-scoped receipts are retained for recovery; only explicitly
expired non-run receipts can be deleted.

Run the schema and concurrency tests against isolated databases using a local
PostgreSQL role with `CREATEDB`:

```sh
DATABASE_URL=postgresql://localhost/postgres cargo test -p platform-server --test runtime_schema -- --include-ignored
```

Apply pending migrations without starting HTTP listeners or gateway clients
(run from `platform/apps/server` so its `.env` is loaded):

```sh
cargo run -p platform-server --example migrate
```

## Run locally

### Worker work-availability notifications

`GET /internal/workers/{id}/work-available?wait_seconds=25` uses the existing
per-process worker token and returns `{"available":true|false}`. The optional
wait is bounded to 0–25 seconds (default 25); zero is an immediate check.
This is a read-only hint: it does not claim runs, renew leases, or count as a
heartbeat. Offline workers receive `WORKER_STATE_CONFLICT`.

The server subscribes before checking the database, preventing a lost wake-up
between checking and waiting. Eligibility matches claims: supported harness,
accepting worker, free registered capacity, and either due ready work or expired
running work. Existing PostgreSQL notifications carry hints across Platform
replicas. Hint bursts are coalesced; a check at most five seconds later also catches
missed hints, future availability and lease expiry. Every wait has a final check.

No transaction, row lock, or pool connection is held while awaiting a hint.
The existing ready/lease/worker indexes support these reads; no migration or
additional infrastructure is required. Keep any reverse-proxy request timeout
longer than the 25-second wait plus response overhead.

Projects use a dedicated PostgreSQL database configured with
`PLATFORM_SERVER_DATABASE_URL`. Create an empty `platform_server` database and
set the connection URL for your local role before starting the server. Do not
point this setting at the LLM or execution gateway databases. Embedded migrations
run at startup with checksum validation and migration locking enabled. The build
script tracks the migrations directory so new SQL files are embedded on rebuild.

`GET /api/projects` returns projects ordered by name, then ID.
`POST /api/projects` accepts `{ "name": "My project", "avatar": null }` and
returns the persisted `{ "id", "name", "avatar" }` record with HTTP 201.
Names are limited to 128 characters. Optional avatars use PNG/JPEG/WebP/GIF
base64 data URLs, limited to 512 KiB decoded; requests are limited to 800 KiB.
The dashboard lists projects and provides the Add Project dialog. Clicking a
project opens `/projects/{id}` with an Environments tab and a read-only table of
saved environment records. Lists are cached per project, refresh every 15 seconds
in the foreground and on focus/reconnect, and retain saved data after refresh
failures. Existing projects are not imported automatically.

Copy `.env.example` to `.env`, provide the execution gateway bearer token and
LLM gateway admin token, and
run:

```sh
cargo run -p platform-server
```

```sh
curl http://127.0.0.1:3100/api/providers
cargo test -p platform-server
cargo clippy -p platform-server --all-targets -- -D warnings
```

All routes are intended for local development. Add application
authentication and authorization before exposing this server publicly.
## Harness environment integration

Workers can GET/POST `/internal/runs/{id}/environments` using their normal worker
token, `X-Worker-ID`, and `X-Lease-Epoch`. POST also requires `Idempotency-Key`
and the same environment fields as project environment creation; the project is
derived from the run. GET returns `{ "items": [...] }`; POST returns the created
environment with status 201. Records and receipts are committed atomically in
the existing database, with no additional migration required. The public
dashboard POST remains unchanged; harnesses must use the fenced internal route
for recoverable creation. Runtime construction must inject the existing
`EnvironmentService` with `RuntimeService::with_environments` (wired in main).

## Sites integration

See [SITES.md](SITES.md) for authenticated project site management, provisioning,
backend forwarding, operator grants, and the session-oriented backend SDK.
