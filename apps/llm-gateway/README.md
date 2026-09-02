# llm-gateway

A non-streaming HTTP gateway over the workspace's OpenAI, ChatGPT,
Fireworks, Anthropic, OpenRouter, and DeepSeek providers. One gateway request
performs exactly one provider attempt using either the explicitly selected
account or that provider's default account.

The gateway stores accounting metadata for every successful completion. It does
not store prompts, assistant content, native provider responses, failed
attempts, or retry state.

## Local setup

Create the database and local configuration once:

```sh
createdb llm_gateway
cp apps/llm-gateway/.env.example .env
openssl rand -base64 32
openssl rand -hex 32
```

Copy the base64 output into `GATEWAY_MASTER_KEY` and the hex output into
`GATEWAY_ADMIN_TOKEN` in `.env`. Keep the master key stable and backed up:
encrypted provider credentials cannot be recovered without it. Neither value
is stored in PostgreSQL.

Run the service:

```sh
cargo run -p llm-gateway
```

Pending SQLx migrations run before the listener starts. To listen on a trusted
local network, set `GATEWAY_BIND_ADDRESS=0.0.0.0:3000`. Runtime generation and
discovery routes are intentionally unauthenticated. Account-management routes
require the admin token, but plain HTTP still exposes that token and provider
credentials to anyone who can observe the connection. Use TLS or an encrypted
tunnel outside a trusted machine, and do not expose the service to the public
internet.

## Provider accounts

Accounts can be managed through either the authenticated HTTP API or the local
CLI. Secret prompts do not echo values or place them in shell history.

```sh
cargo run -p llm-gateway -- account add \
  --provider openai \
  --name personal \
  --default

cargo run -p llm-gateway -- account list
cargo run -p llm-gateway -- account set-default ACCOUNT_ID
cargo run -p llm-gateway -- account enable ACCOUNT_ID
cargo run -p llm-gateway -- account disable ACCOUNT_ID
cargo run -p llm-gateway -- account rotate-credentials ACCOUNT_ID
cargo run -p llm-gateway -- account remove ACCOUNT_ID
```

The first enabled account for a provider becomes its default automatically.
Disabling the current default is rejected until another default is selected.

For automation, `account add` and `account rotate-credentials` accept
`--credentials-stdin`. The JSON is tagged by provider:

```json
{"provider":"openai","api_key":"..."}
```

ChatGPT uses:

```json
{"provider":"chatgpt","access_token":"...","account_id":"..."}
```

Legacy access-token-only records remain usable until the access token expires.
Accounts created by Platform also store the ID token, refresh token, access
token expiry, and last refresh time. The gateway refreshes those accounts five
minutes before expiry and retries one request after a provider `401`. Rotating
refresh tokens are persisted atomically in the encrypted credential record.

Optional, non-secret provider configuration is supplied with `--config`:

```sh
cargo run -p llm-gateway -- account set-config ACCOUNT_ID \
  --config '{"base_url":"http://127.0.0.1:4000/v1"}'
```

Supported configuration keys:

- OpenAI: `base_url`, `organization`, `project`
- ChatGPT, Fireworks, DeepSeek: `base_url`
- Anthropic: `base_url`, `api_version`, `beta_header`
- OpenRouter: `base_url`, `http_referer`, `app_title`, `router_metadata`

Unknown or invalid keys are rejected before saving.

## HTTP API

### Manage provider accounts

Every admin request must carry the configured token:

```http
Authorization: Bearer GATEWAY_ADMIN_TOKEN
```

Create an account with encrypted credentials:

```http
POST /v1/admin/accounts
Content-Type: application/json
Authorization: Bearer GATEWAY_ADMIN_TOKEN
```

```json
{
  "provider": "openai",
  "name": "personal",
  "credentials": {"api_key": "..."},
  "config": {},
  "enabled": true,
  "make_default": true
}
```

ChatGPT accounts created through OAuth use all of
`id_token`, `access_token`, `refresh_token`, `account_id`, and
`access_token_expires_at`. The remaining admin operations are:

```http
GET    /v1/admin/accounts
GET    /v1/admin/accounts?provider=openai
GET    /v1/admin/accounts/{account_id}
PATCH  /v1/admin/accounts/{account_id}
PUT    /v1/admin/accounts/{account_id}/default
PUT    /v1/admin/accounts/{account_id}/credentials
GET    /v1/admin/accounts/{account_id}/usage
GET    /v1/admin/accounts/{account_id}/requests?cursor=...&limit=25
DELETE /v1/admin/accounts/{account_id}
```

`PATCH` accepts any combination of `name`, `config`, and `enabled`. Credential
rotation accepts the provider-specific credential object directly. Admin read
responses include account configuration, timestamps, credential version,
encryption-key version, and credential update time. There is deliberately no
credential reveal endpoint: plaintext credentials, ciphertext, nonces, and
internal secret IDs are never returned.

The account usage endpoint returns the successful completion count and lifetime
USD cost and token totals, split into input, output, cache-read, and cache-write
components. Missing provider usage components contribute zero to their
aggregate; component costs may therefore not add up to the authoritative total
when a provider supplies only a total cost.

The account requests endpoint returns successful completions newest first. Its
`items` include request, model, and assistant-message IDs, the exact normalized
`usage` object, provider duration, and completion timestamp. `limit` defaults to
25 and accepts 1 through 100. Pass `next_cursor` back as `cursor` to retrieve
the next page. Both accounting endpoints require the admin bearer token.

### Complete a request

```http
POST /v1/complete
Content-Type: application/json
```

```json
{
  "account_id": null,
  "request": {
    "model": {"provider":"openai","id":"gpt-5.6-luna"},
    "instructions": "Answer concisely.",
    "messages": [
      {
        "role": "user",
        "id": "message-1",
        "timestamp": 1787385600000,
        "content": [{"type":"text","content":"What is Tokio?"}]
      }
    ]
  }
}
```

The success body contains `request_id`, the resolved `account_id`, and the
unchanged `AssistantMessage` as `message`. Errors contain a stable `kind`,
`can_retry`, optional `retry_after_ms`, and the normalized `LlmError` for
provider failures. `x-request-id` and, when resolved, `x-account-id` are also
returned as headers.

The application owns retries. The gateway never switches accounts or retries a
provider call.

### Run a Codex web search

OpenAI and ChatGPT accounts also expose the provider-backed Codex search API:

```http
POST /v1/search
Content-Type: application/json
```

```json
{
  "account_id": null,
  "provider": "openai",
  "request": {
    "id": "session-1",
    "model": "gpt-5.6-luna",
    "commands": {
      "search_query": [{"q":"OpenAI Codex","domains":["openai.com"]}],
      "response_length": "short"
    },
    "max_output_tokens": 2048
  },
  "request_options": {
    "originator": "agent-pane",
    "codex_turn_metadata": "{\"turn_id\":\"turn-1\"}"
  }
}
```

`request` is forwarded using the Codex `alpha/search` wire format. The gateway
routes it to `/v1/alpha/search` for an OpenAI API-key account or
`/backend-api/codex/alpha/search` for a ChatGPT account. The optional request
metadata is forwarded as `originator` and `x-codex-turn-metadata` headers but is
never added to the upstream JSON body. Other provider kinds are rejected.

The success body contains `request_id`, the resolved `account_id`, and the
unchanged provider result as `response` (`encrypted_output`, `output`, and
optional opaque `results`). Search calls share completion calls' concurrency
limit, timeout, normalized errors, account selection, and one-time ChatGPT
token refresh after a `401`.

### Discovery and health

```http
GET /v1/models
GET /v1/models?provider=openai
GET /v1/accounts
GET /v1/accounts?provider=openai
GET /health
GET /ready
```

Account discovery returns enabled account metadata only. It never returns
credentials, encrypted payloads, secret IDs, or provider configuration.

## Storage and scaling

- `vault_secrets` stores XChaCha20-Poly1305 encrypted credentials and a
  monotonically increasing credential version.
- `provider_accounts` stores provider, name, enabled/default state, and
  non-secret configuration.
- `llm_completion_accounting` stores the request ID, resolved account, requested
  and response model identities, assistant message ID, provider duration, and
  the exact normalized `usage` object returned in the assistant message. Token
  and USD cost fields are exposed as generated numeric columns for reporting.
  A row is still stored when a successful provider response omits usage.
- Provider transports and their HTTP pools are cached by
  `(account_id, credential_version)`.
- Credential rotation or provider configuration changes increment that version
  and automatically cause a new transport to be built.
- A bounded Tokio semaphore rejects excess concurrency with a retryable `503`.
- The service is stateless apart from PostgreSQL and can run as multiple
  replicas sharing the same database and master key.
