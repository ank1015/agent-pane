# Platform server

The Rust API used by the platform dashboard.

## Machines API

`GET /api/machines` returns the active execution hosts from the hosted execution
gateway. The response is a JSON array using the stable `execution-api`
`ExecutionHost` contract. Its `kind` field distinguishes managed Registered
Hosts (`registered`) from sandbox hosts (`e2b`).

The dashboard displays registered hosts only, plus sandbox **accounts** from
`GET /api/machines/e2b-accounts`. Sandbox hosts are not displayed.

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

## Run locally

Projects use a dedicated PostgreSQL database configured with
`PLATFORM_SERVER_DATABASE_URL`. Create an empty `platform_server` database and
set the connection URL for your local role before starting the server. Do not
point this setting at the LLM or execution gateway databases. Embedded migrations
run at startup with checksum validation and migration locking enabled.

`GET /api/projects` returns projects ordered by name, then ID.
`POST /api/projects` accepts `{ "name": "My project", "avatar": null }` and
returns the persisted `{ "id", "name", "avatar" }` record with HTTP 201.
Names are limited to 128 characters. Optional avatars use PNG/JPEG/WebP/GIF
base64 data URLs, limited to 512 KiB decoded; requests are limited to 800 KiB.
The dashboard lists projects and provides the Add Project dialog; project detail
navigation is not implemented yet. Existing projects are not imported automatically.

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

Both sets of routes are intended for local development. Add application
authentication and authorization before exposing this server publicly.
