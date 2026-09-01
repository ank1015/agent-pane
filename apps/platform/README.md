# platform

The dashboard backend-for-frontend. `platform` exposes client-oriented APIs and
coordinates the workspace's internal services. Platform owns dashboard-level
data such as projects, while `providers` maps dashboard provider operations to
`llm-gateway`, `machines` maps machine and sandbox operations to
`execution-gateway`, and `harnesses` maps harness inventory to `agent`.

## Architecture

The service is a modular monolith organized by product capability (normally a
dashboard tab):

```text
src/
├── projects/               # Platform-owned project CRUD
│   ├── http.rs             # Dashboard-facing routes
│   ├── model.rs            # API and persistence models
│   └── mod.rs              # Application service
├── providers/              # Providers feature slice
│   ├── http.rs             # Dashboard-facing routes
│   ├── chatgpt_oauth.rs    # ChatGPT PKCE login and callback state
│   ├── model.rs            # API contracts
│   └── mod.rs              # Application service
├── machines/               # Machines and sandbox-account feature slice
│   ├── http.rs             # Dashboard-facing routes
│   ├── model.rs            # API contracts
│   └── mod.rs              # Application service
├── harnesses/              # Harness inventory feature slice
│   ├── http.rs             # Dashboard-facing routes
│   ├── model.rs            # Dashboard and Agent contracts
│   └── mod.rs              # Application service and pagination
├── upstream/
│   ├── agent.rs             # Agent control adapter
│   ├── execution_gateway.rs # Execution gateway control adapter
│   └── llm_gateway.rs       # LLM gateway admin adapter
├── config.rs               # Process configuration
├── db.rs                   # PostgreSQL connection and migrations
├── error.rs                # Shared HTTP error mapping
├── lib.rs                  # Router composition
└── main.rs                 # Process lifecycle
```

Add future tabs as sibling modules of `providers`. A feature owns its routes,
models, and use cases. Adapters shared by multiple features belong in
`upstream`; reusable domain logic should eventually move to `packages/`.

Provider credentials pass through platform memory only long enough to send the
request. `llm-gateway` remains responsible for validation, encryption, and
persistence, and neither service offers a credential reveal endpoint.

## Local setup

Create a PostgreSQL database for Platform, then start `llm-gateway`,
`execution-gateway`, and `agent` before configuring Platform. The platform token must be
the same value as `GATEWAY_ADMIN_TOKEN` used by `llm-gateway`. When both
processes load the same root `.env`, platform automatically falls back to that
variable; `PLATFORM_LLM_GATEWAY_ADMIN_TOKEN` can override it.
Likewise, `PLATFORM_EXECUTION_GATEWAY_CONTROL_TOKEN` must match
`EXECUTION_GATEWAY_CONTROL_TOKEN`; platform falls back to the latter when all
services load the root `.env`.
`PLATFORM_AGENT_CONTROL_TOKEN` must likewise match `AGENT_CONTROL_TOKEN`.

```sh
cp apps/platform/.env.example .env.platform
set -a
source .env.platform
set +a
cargo run -p platform
```

The default listener is `http://127.0.0.1:3100`. Liveness is available at
`GET /health`; `GET /ready` checks the database and all three upstream services.

## Projects API

```text
GET    /api/projects
POST   /api/projects
GET    /api/projects/{project_id}
PATCH  /api/projects/{project_id}
DELETE /api/projects/{project_id}
GET    /api/projects/{project_id}/environments
POST   /api/projects/{project_id}/harness-sessions
GET    /api/projects/{project_id}/harness-sessions/{session_id}
GET    /api/projects/{project_id}/harness-sessions/{session_id}/messages
GET    /api/projects/{project_id}/harness-sessions/{session_id}/runs
POST   /api/projects/{project_id}/harness-sessions/{session_id}/runs
GET    /api/projects/{project_id}/harness-sessions/{session_id}/runs/{run_id}
GET    /api/projects/{project_id}/harness-sessions/{session_id}/runs/{run_id}/events
GET    /api/projects/{project_id}/harness-sessions/{session_id}/runs/{run_id}/events/stream
POST   /api/projects/{project_id}/harness-sessions/{session_id}/runs/{run_id}/abort
```

Create a project with `{"name":"Agent Pane","avatar":null}`. `avatar` may be
omitted, supplied as a string, or set to `null` in a PATCH request to clear it.
The project environments endpoint combines configured machine environments and
sandbox templates. Its items expose `id`, `name`, `host_name`, an optional
`machine_id`, `path`, `type`, an optional `snapshot_id`, an optional
`setup_script`, and `created_at`.

Create a project-scoped harness session and its initial run with an
`Idempotency-Key` header. Platform reserves stable session, run, and message
IDs, creates the Agent session, and starts the run against the harness's active
revision. `config_override` is forwarded without Platform interpreting its
harness-specific shape:

```json
{
  "harness_id": "environment",
  "prompt": "Create a TypeScript environment",
  "attachments": [
    {
      "source": {
        "type": "url",
        "url": "https://example.com/reference.png"
      },
      "detail": "high"
    }
  ],
  "config_override": {
    "project_id": "0198f88e-2ff3-7000-8000-000000000010",
    "provider": "openai",
    "model_id": "gpt-5.6-sol",
    "reasoning_level": "high"
  },
  "limits": {
    "max_turns": 100
  }
}
```

Attachments are image content without the outer `{"type":"image"}` wrapper.
Their source may be an HTTP(S) URL or base64 data with a MIME type. Platform
places non-blank prompt text first, followed by image content in request order.
At least one prompt or attachment is required. A successful request returns
`202 Accepted` with the Agent session, trigger message, and accepted run.

Session reads are scoped through Platform's project/session ownership mapping
before Agent is called. The session detail response contains `session`, the
session's `harness_id`, `latest_run`, and `active_run`; waiting runs count as
active, and both run fields are nullable. Canonical transcript messages retain
Agent's revision pagination:

```text
GET .../messages?after_revision={revision}&limit={limit}
```

Run history retains Agent's status and cursor pagination:

```text
GET .../runs?status={active|waiting|aborted|completed|failed}&cursor={cursor}&limit={limit}
```

Start another run in an existing session with a new `Idempotency-Key`. Platform
reuses the session's harness ID and forwards the opaque `config_override` to
the active harness revision. `expected_session_revision` is required so Agent
can reject stale submissions or concurrent changes:

```json
{
  "prompt": "Now add a PostgreSQL service",
  "attachments": [],
  "config_override": {
    "provider": "openai",
    "model_id": "gpt-5.6-sol",
    "reasoning_level": "high"
  },
  "limits": {
    "max_turns": 100
  },
  "expected_session_revision": 2
}
```

The response is `202 Accepted` with `trigger_message` and `run`. Exact retries
reuse the reserved message and run IDs. Reusing an idempotency key with a
different body returns `409 Conflict`; an active run or stale session revision
is also returned as Agent's `409 Conflict` response.

Run detail and lifecycle operations are scoped through the complete
project/session/run ownership mapping before Platform calls Agent. Persisted
events use sequence pagination:

```text
GET .../runs/{run_id}/events?after_sequence={sequence}&limit={1..500}
```

The response contains `items` in ascending sequence order and a nullable
`next_after_sequence`. The SSE endpoint accepts the same `after_sequence`
resume position. A `Last-Event-ID` header takes precedence when both are
present:

```text
GET .../runs/{run_id}/events/stream?after_sequence={sequence}
Last-Event-ID: {sequence}
```

Platform relays Agent's event IDs, event names, JSON data, keep-alives, and
terminal stream closure without buffering. It preserves `text/event-stream`,
`Cache-Control: no-cache, no-transform`, and `X-Accel-Buffering: no`.

Abort an active or waiting run with a client-generated stable `abort_id`. The
ID makes exact retries idempotent, while `expected_state_version` prevents a
stale UI from aborting a run whose state changed after it was displayed:

```json
{
  "abort_id": "01991a8d-7eef-7000-8000-000000000001",
  "expected_state_version": 6,
  "reason": "Stopped by user",
  "payload": {}
}
```

A successful abort returns `202 Accepted` with `abort` and the updated `run`.
Agent conflict and validation responses are forwarded unchanged.

## Harnesses API

```text
GET /api/harnesses
GET /api/harnesses/{harness_id}/model-options
```

Platform follows Agent pagination internally and returns a dashboard-oriented
JSON array containing `id`, `name`, `description`, `created_at`, and
`updated_at`. Upstream authentication uses the Agent control token; the token
is never exposed to the dashboard.

The model-options endpoint combines the harness capability catalog from Agent
with enabled provider accounts from `llm-gateway`. It returns one `providers`
entry per matching account (so a provider may appear more than once) and the
harness's portable `reasoning_levels`:

```json
{
  "providers": [
    {
      "account_id": "0198f88e-2ff3-7000-8000-000000000001",
      "name": "Personal OpenAI",
      "provider": "openai",
      "model_ids": ["gpt-5.6-sol", "gpt-5.6-terra"]
    }
  ],
  "reasoning_levels": ["low", "medium", "high", "xhigh", "max"]
}
```

## Sandbox accounts API

```text
GET    /api/machines
PATCH  /api/machines/{machine_id}
GET    /api/machines/sandbox-accounts
POST   /api/machines/sandbox-accounts
GET    /api/machines/sandbox-accounts/{account_id}
GET    /api/machines/sandbox-accounts/{account_id}/sandboxes
POST   /api/machines/sandbox-accounts/{account_id}/sandboxes
POST   /api/machines/sandbox-accounts/{account_id}/sandboxes/{sandbox_id}/snapshots
DELETE /api/machines/sandbox-accounts/{account_id}
PUT    /api/machines/sandbox-accounts/{account_id}/credentials
GET    /api/machines/snapshots
POST   /api/machines/snapshots
GET    /api/machines/snapshots/{snapshot_id}
PATCH  /api/machines/snapshots/{snapshot_id}
DELETE /api/machines/snapshots/{snapshot_id}
GET    /api/machines/sandbox-environment-templates
POST   /api/machines/sandbox-environment-templates
GET    /api/machines/sandbox-environment-templates/{template_id}
PATCH  /api/machines/sandbox-environment-templates/{template_id}
DELETE /api/machines/sandbox-environment-templates/{template_id}
GET    /api/machines/sandbox-environment-templates/{template_id}/environments
POST   /api/machines/sandbox-environment-templates/{template_id}/environments
DELETE /api/machines/environments/{environment_id}
```

`GET /api/machines` returns both `connector_accounts` and `machine_daemons` in
one response for the Machines page. Connector accounts are account metadata;
machine daemons include their machine descriptor and current online state.

Create an E2B, Daytona, Blaxel, or Tensorlake account:

```json
{
  "provider": "e2b",
  "name": "main",
  "api_key": "...",
  "config": {},
  "enabled": true,
  "make_default": false
}
```

Platform forwards the API key once over its authenticated control connection.
`execution-gateway` encrypts and stores it; neither service exposes a secret
read endpoint, and platform responses contain account metadata only.

Snapshot creation accepts an optional `sandbox_account_id`. If it is absent,
execution-gateway chooses the provider's default enabled account at creation
time and stores that account on the snapshot. Sandbox environment templates
therefore always materialize with the snapshot's fixed provider account.

For a tracked E2B, Daytona, Blaxel, or Tensorlake sandbox, the nested snapshot
endpoint accepts `{"name":"Ready workspace"}` and asks execution-gateway to
create the provider snapshot before returning its stored record. Filter the
snapshot list with `?sandbox_account_id={account_id}` to retrieve one account's
snapshots.

## Providers API

```text
GET    /api/providers
POST   /api/providers
GET    /api/providers/{provider_id}
PATCH  /api/providers/{provider_id}
DELETE /api/providers/{provider_id}
PUT    /api/providers/{provider_id}/default
PUT    /api/providers/{provider_id}/credentials
POST   /api/providers/chatgpt/login
GET    /api/providers/chatgpt/login/{login_id}
DELETE /api/providers/chatgpt/login/{login_id}
```

`GET /api/providers` returns a JSON array containing `id`, `name`, `provider`,
`status`, `created_at`, and `is_default`. Accounts are sorted alphabetically by
provider and newest-first by creation date within the same provider.

Create a provider:

```json
{
  "provider": "openai",
  "name": "personal",
  "api_key": "..."
}
```

This create endpoint accepts Anthropic, DeepSeek, Fireworks, OpenAI, and
OpenRouter. ChatGPT uses the separate login endpoints: Platform creates a
short-lived PKCE attempt, handles the OAuth callback on `127.0.0.1:1455`, and
sends the resulting token bundle directly to `llm-gateway`. Tokens are never
returned to the dashboard. Configure the callback listener, redirect URI,
dashboard origin, issuer, and public OAuth client ID with the
`PLATFORM_CHATGPT_OAUTH_*` and `PLATFORM_DASHBOARD_ORIGIN` variables shown in
`.env.example`.

The API calls these resources `providers`, while `llm-gateway` calls the same
resources provider accounts. Upstream errors retain their original HTTP status
and safe JSON body; connection and malformed-response failures become `502`.

Platform currently assumes a trusted, loopback-only client. Add user/session
authentication before binding it to a non-loopback interface.
