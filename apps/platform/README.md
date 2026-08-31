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
```

Create a project with `{"name":"Agent Pane","avatar":null}`. `avatar` may be
omitted, supplied as a string, or set to `null` in a PATCH request to clear it.
The project environments endpoint combines configured machine environments and
sandbox templates. Its items expose `id`, `name`, `host_name`, an optional
`machine_id`, `path`, `type`, an optional `snapshot_id`, an optional
`setup_script`, and `created_at`.

## Harnesses API

```text
GET /api/harnesses
```

Platform follows Agent pagination internally and returns a dashboard-oriented
JSON array containing `id`, `name`, `description`, `created_at`, and
`updated_at`. Upstream authentication uses the Agent control token; the token
is never exposed to the dashboard.

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
