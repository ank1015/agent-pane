# llm-gateway

Authenticated HTTP gateway for the standalone LLM system. It executes requests
through the OpenAI, ChatGPT, and Fireworks packages, stores encrypted provider
credentials, and records request outcomes and normalized usage. Optional asynchronous runs
retain results for 48 hours so callers can recover after disconnecting.

## Storage

The gateway owns five application tables:

- `provider_accounts`: non-secret configuration, status, default selection, and
  the runtime revision used to invalidate cached transports.
- `provider_credentials`: the current XChaCha20-Poly1305 encrypted credential
  payload and credential/key versions.
- `llm_requests`: completion and search outcomes, safe error metadata, timing,
  provider/model identity, and non-secret accounting labels.
- `llm_runs`: idempotency fingerprints, execution status, and temporary results.
- `llm_usage`: the exact normalized `Usage` object plus generated token and USD
  cost columns for reporting.

Account deletion is a soft deletion of account identity and a hard deletion of
its ciphertext. Historical request attribution therefore remains intact while
the credential becomes unrecoverable. Synchronous requests store no prompts or responses. Asynchronous runs store the
normalized completion (including any content, tool arguments, and native data
present in that completion), or a normalized failure. Submitted prompts are not
stored; only a SHA-256 fingerprint of the typed submission is retained. Raw
provider error bodies are excluded. Run retrieval uses the shared runtime token,
so every holder of that token has access to runs.

## Local setup

```sh
createdb llm_gateway
cp apps/llm-gateway/.env.example .env
openssl rand -base64 32
openssl rand -hex 32
openssl rand -hex 32
```

Use the generated values for `LLM_GATEWAY_VAULT_KEY`,
`LLM_GATEWAY_API_TOKEN`, and `LLM_GATEWAY_ADMIN_TOKEN`. Runtime and admin tokens
must differ. The vault key must remain stable and backed up; encrypted provider
credentials cannot be recovered without it.

Start the gateway from the `llm/` workspace:

```sh
cargo run -p llm-gateway
```

Pending migrations run before the listener starts.

## Authentication

Every `/v1/*` request requires a bearer token:

- Runtime routes use `LLM_GATEWAY_API_TOKEN`.
- `/v1/admin/*` routes use `LLM_GATEWAY_ADMIN_TOKEN`.
- `/health` and `/ready` use `LLM_GATEWAY_API_TOKEN` too. Unmatched paths are
  also authenticated before returning `404`.

Failed bearer-token attempts are bounded per gateway instance by
`LLM_GATEWAY_MAX_AUTH_FAILURES_PER_MINUTE`. Valid requests do not consume this
limit. Runtime and admin tokens must be different, between 32 and 512 bytes,
and contain no whitespace.

Use TLS or an encrypted service network outside local development. Bearer
tokens and submitted credentials are exposed to anyone who can observe plain
HTTP traffic.

## Runtime API

### Execute an LLM request

```http
POST /v1/llm
Authorization: Bearer LLM_GATEWAY_API_TOKEN
Content-Type: application/json
```

```json
{
  "account_id": null,
  "request": {
    "model": {"provider":"openai","id":"gpt-5.6-luna"},
    "instructions": "Answer concisely.",
    "messages": [],
    "tools": [],
    "provider_options": {},
    "metadata": {"run_id":"run-1"}
  }
}
```

`account_id` selects one account explicitly. When omitted, the provider's
default account is used. The response contains `request_id`, the resolved
`account_id`, and the unchanged normalized `AssistantMessage` as `message`.
The IDs are also returned as `x-request-id` and `x-account-id` headers.

### Submit and retrieve an asynchronous run

```http
POST /v1/llm/runs
Authorization: Bearer LLM_GATEWAY_API_TOKEN
Idempotency-Key: my-job-123-generation-1
Content-Type: application/json
```

The body is identical to `POST /v1/llm`. This endpoint acknowledges acceptance
without waiting for the provider. It returns `202 Accepted` while running,
with `Location: /v1/llm/runs/{run_id}` and `Retry-After: 1`.

```json
{
  "run_id": "019...",
  "status": "running",
  "result": null,
  "created_at": "2026-09-04T10:00:00Z",
  "completed_at": null,
  "expires_at": null
}
```

```text
GET /v1/llm/runs/{run_id}
GET /v1/llm/runs/{run_id}?wait_seconds=25
```

Both require the runtime bearer token. `wait_seconds` defaults to 0 and accepts
0–25. A wait returns when execution finishes or the wait period ends. Repeat it
while `status` is `running`; disconnecting from either submission or retrieval
does not cancel accepted work. `running` includes waiting in the gateway's
bounded concurrency queue.

Completed runs return `200` with `status: succeeded` and `result` containing the
usual `{request_id, account_id, message}` completion, or `status: failed` and
`result` containing `{request_id, account_id?, error}`. Semantic validation,
account selection, queue rejection, and provider failures are recorded as failed
runs. Malformed JSON and missing/invalid keys are rejected before acceptance.

The required idempotency key contains 1–256 visible ASCII characters and is
scoped to this gateway database, shared by all runtime callers. Persist the key
before submission, or derive it from a stable logical operation identifier.
Retrying with the same key and typed payload returns the existing run (including
its result when complete), even if the initial response was lost. Object key
order and omitted defaults do not change the fingerprint. A changed payload
returns `409 Conflict`. Failed runs are also deduplicated: an intentional new
execution requires a new key. Keys are independent of `request.metadata.run_id`.

Results expire 48 hours after completion. Retrieval and resubmission return
`410 Gone` with `status: expired` and no result after expiry. The server clears
expired payloads every five minutes and retains the fingerprint/key until seven
days after completion. After that cleanup, retrieval returns `404` and the key
can create a new run. Usage/accounting history is unaffected by run cleanup.

Execution remains process-local: gateway restart/crash recovery is not provided.
Saving a completed result is retried independently without repeating the provider
call. The synchronous `/v1/llm` endpoint retains its existing behavior.

### Abort an asynchronous run

```http
POST /v1/llm/runs/{run_id}/abort
Authorization: Bearer LLM_GATEWAY_API_TOKEN
```

Atomically changes a running run to `aborted`, with `result: null`, a completion
timestamp, and the usual 48-hour expiry. Returns the run snapshot with HTTP 200.
Repeated aborts return the same snapshot. Completed or failed runs remain
unchanged; expired runs return 410, and unknown IDs return 404. The original
idempotency key continues to resolve to the aborted run. Use a new key for an
intentional new execution.

Abort and completion both update only running rows: the first committed terminal
transition wins. An abort response confirms the durable state transition, not
that upstream work has already stopped. The owning worker checks PostgreSQL at
250 ms intervals (plus database latency), drops queued admission or the active
provider future, and releases concurrency capacity. Existing accounting rows are
finalized as `cancelled` with error kind `aborted` when interruption wins. If a
provider response already arrived, its actual usage can still be accounted for;
it cannot overwrite an aborted run result.

Because the abort signal is persisted, it works across gateway replicas and
does not depend on the caller remaining connected. It does not add recovery for
in-flight execution after a gateway crash. A provider may still complete or bill
work already received; there is no provider-independent upstream cancellation
guarantee. This endpoint applies to asynchronous runs, not the synchronous
`POST /v1/llm` route.

### Execute provider-backed search

```http
POST /v1/search
Authorization: Bearer LLM_GATEWAY_API_TOKEN
Content-Type: application/json
```

```json
{
  "account_id": null,
  "provider": "openai",
  "request": {
    "id": "search-1",
    "model": "gpt-5.6-luna",
    "commands": {
      "search_query": [{"q":"OpenAI Codex","domains":["openai.com"]}],
      "response_length": "short"
    }
  },
  "request_options": {"originator":"agent-pane"}
}
```

OpenAI and ChatGPT support search. Fireworks returns
`unsupported_capability`. Search outcomes are stored in `llm_requests`; the
current search contract has no usage object, so it does not create an
`llm_usage` row.

### Catalog

```text
GET /v1/providers
GET /v1/models
GET /v1/models?provider=openai
```

Provider capabilities and model catalogs come directly from the provider
packages and are not stored in PostgreSQL.

## Provider account API

All routes below require the admin token.

```text
POST   /v1/admin/accounts
GET    /v1/admin/accounts
GET    /v1/admin/accounts?provider=openai
GET    /v1/admin/accounts/{account_id}
PATCH  /v1/admin/accounts/{account_id}
DELETE /v1/admin/accounts/{account_id}
PUT    /v1/admin/accounts/{account_id}/credentials
PUT    /v1/admin/accounts/{account_id}/default
POST   /v1/admin/accounts/{account_id}/validate
```

Create an API-key account:

```json
{
  "provider": "openai",
  "name": "personal",
  "credentials": {"api_key":"..."},
  "config": {},
  "status": "active",
  "make_default": true
}
```

ChatGPT credentials use:

```json
{
  "id_token": "...",
  "access_token": "...",
  "refresh_token": "...",
  "account_id": "...",
  "access_token_expires_at": "2026-09-03T20:00:00Z"
}
```

ChatGPT access tokens are refreshed five minutes before expiry. Refresh is
serialized per account both inside a process and across gateway replicas. A
permanently invalid refresh token changes the account status to
`reauth_required`; rotating credentials restores it to `active`.

`PATCH` accepts `name`, `config`, or `status`. Supported non-secret config keys:

- OpenAI: `base_url`, `organization`, `project`
- ChatGPT and Fireworks: `base_url`

Unknown keys and malformed URLs are rejected. The validation endpoint decrypts
and validates stored credentials and constructs the provider configuration. It
does not make a billable upstream request and therefore returns `live: false`.
There is no credential reveal endpoint.

Provider and ChatGPT OAuth endpoints must use HTTPS. Plain HTTP is accepted
only for loopback addresses so local mock servers remain usable. Provider HTTP
clients do not follow redirects, preventing credentials from being forwarded
to an unexpected endpoint. Cached provider configurations zeroize credentials
when dropped, provider responses are capped at 16 MiB, and raw provider error
bodies are not returned by the gateway.

## Accounting API

```text
GET /v1/admin/requests
GET /v1/admin/requests/{request_id}
GET /v1/admin/usage
GET /v1/admin/accounts/{account_id}/requests
GET /v1/admin/accounts/{account_id}/usage
```

Request and usage lists accept these filters where applicable:

```text
account_id provider model operation status from to cursor limit
```

`operation` is `complete` or `search`. `limit` defaults to 25 and accepts 1
through 100. Request pagination is newest-first using the returned opaque
`next_cursor`.

Usage reports additionally accept:

```text
group_by=account|provider|model|day
```

Totals include request, success, failure, and usage-record counts along with
input/output/cache token and USD-cost components. Missing provider usage
components remain null in the stored normalized object and contribute zero to
aggregates.

## Execution behavior

- A gateway request performs one provider attempt. It never switches accounts
  or performs model failover.
- ChatGPT may repeat the same request once after refreshing credentials in
  response to a provider `401`.
- A fair bounded queue runs up to `LLM_GATEWAY_MAX_CONCURRENT_REQUESTS`
  provider operations, holds up to `LLM_GATEWAY_MAX_QUEUED_REQUESTS` additional
  requests, and admits them as capacity becomes available. A full queue or a
  wait exceeding `LLM_GATEWAY_QUEUE_TIMEOUT_SECONDS` returns a retryable `503`
  with `Retry-After`.
- Runtime payloads use the configured body limit; admin payloads are capped at
  1 MiB. Configured runtime body limits cannot exceed 256 MiB.
- A request row is inserted before contacting the provider and finalized with
  success or normalized failure afterward.
- If final accounting persistence fails after a provider success, the gateway
  logs the accounting failure but still returns the successful response. This
  avoids encouraging a duplicate billable model call.
- Provider transports are cached by account runtime and credential versions.
  Configuration changes, credential rotation, and account deletion evict the
  affected in-memory transport.
- Every response, including authentication failures and `404`s, receives a
  request ID, `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`,
  `X-Frame-Options: DENY`, and `Referrer-Policy: no-referrer`.

## Verification

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
