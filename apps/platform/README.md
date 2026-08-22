# platform

The dashboard backend-for-frontend. `platform` exposes client-oriented APIs and
coordinates the workspace's internal services; it does not take ownership of
their data. The first feature slice is `providers`, which maps dashboard
provider operations to `llm-gateway`'s authenticated account-management API.

## Architecture

The service is a modular monolith organized by product capability (normally a
dashboard tab):

```text
src/
├── providers/              # Providers feature slice
│   ├── http.rs             # Dashboard-facing routes
│   ├── chatgpt_oauth.rs    # ChatGPT PKCE login and callback state
│   ├── model.rs            # API contracts
│   └── mod.rs              # Application service
├── upstream/
│   └── llm_gateway.rs      # Internal-service adapter
├── config.rs               # Process configuration
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

Start `llm-gateway` first, then configure platform. The platform token must be
the same value as `GATEWAY_ADMIN_TOKEN` used by `llm-gateway`. When both
processes load the same root `.env`, platform automatically falls back to that
variable; `PLATFORM_LLM_GATEWAY_ADMIN_TOKEN` can override it.

```sh
cp apps/platform/.env.example .env.platform
set -a
source .env.platform
set +a
cargo run -p platform
```

The default listener is `http://127.0.0.1:3100`. Liveness is available at
`GET /health`; `GET /ready` checks that `llm-gateway` is reachable and ready.

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
