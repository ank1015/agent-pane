# llm-client

Typed Rust client for submitting and awaiting LLM gateway runs. Uses
`llm-contracts::LlmRequest` as input and preserves the normalized
`AssistantMessage` in successful results, including usage and native content.

```rust,no_run
use std::time::Duration;
use llm_client::{CompletionRequest, IdempotencyKey, LlmClient, LlmClientConfig, WaitOptions};
use llm_contracts::LlmRequest;

# async fn example(request: LlmRequest) -> Result<(), Box<dyn std::error::Error>> {
let client = LlmClient::new(LlmClientConfig::new(
    std::env::var("LLM_GATEWAY_URL")?.parse()?,
    std::env::var("LLM_GATEWAY_API_TOKEN")?,
))?;
let input = CompletionRequest::new(request); // .with_account(account_id) to select one

// Persist or derive this key before submitting. Keep it for ambiguous failures.
let key = IdempotencyKey::new("agent-run-123/turn-4/generation-1")?;
let job = client.submit(&key, &input).await?;
// Persist job.run_id to retrieve or await it from another worker later.
let completion = client.wait(job.run_id, WaitOptions::new(Duration::from_secs(300))).await?;
let message = completion.message;
# let _ = message;
# Ok(())
# }
```

## Interface

| Method | Behavior |
| --- | --- |
| `submit(&key, &input)` | Start or recover a run; returns its typed snapshot |
| `get_run(run_id)` | Retrieve a snapshot immediately |
| `abort(run_id)` | Abort queued or active work; returns the actual terminal snapshot |
| `wait(run_id, options)` | Long-poll until success, failure, expiry, timeout, or retrieval error |
| `complete(&key, &input, options)` | Submit and await; uses an immediately available result without another request |

Snapshots contain `RunState::Running`, `Succeeded(Box<CompletionResponse>)`,
`Failed(GatewayFailure)`, `Aborted`, or `Expired`. `get_run`, `submit`, and `abort`
return terminal resources as snapshots; `wait` and `complete` return `RunFailed`,
`RunAborted`, or `RunExpired` errors for those outcomes. Completion results include the gateway
request/run ID and resolved provider account ID.

Input and message types remain in `llm-contracts`. This package owns typed
gateway request/result envelopes and client errors; it has no dependency on the
gateway application or provider implementations. It uses the asynchronous run
endpoints, including for `complete`.

## Configuration and waiting

The base URL is the deployment root, optionally with a path prefix. Do not append
`/v1`. Supply the runtime API token; provider credentials remain in the gateway.
Local HTTP requires `allow_insecure_http = true`. Redirects are disabled.

Clients are cheaply cloneable and share their HTTP connection pool. The default
request timeout is 35 seconds and the encoded response limit is 32 MiB. Response
bodies are bounded even without a Content-Length header. Credentials are
redacted from debug output and arbitrary HTTP error bodies are not exposed.

Waiting uses the gateway's long-poll endpoint with waits of up to 25 seconds.
Short HTTP timeouts reduce that interval. Early Running responses are paced to
at most one request per second. `WaitOptions` defaults to a ten-minute overall
wait; an explicit timeout also interrupts an in-progress response read. A zero
wait timeout returns immediately. For `complete`, this timeout begins after
submission is accepted; submission has the separate HTTP request timeout.

Dropping any client future stops the local wait. Accepted work continues at the
gateway until it finishes or is explicitly aborted. Retrieval failures during
waiting retain the run ID in `WaitInterrupted`; callers can call `wait` again.

## Aborting

Call `client.abort(run_id).await?` to explicitly stop a run. The request uses
`POST /v1/llm/runs/{run_id}/abort`. A successful abort returns `RunState::Aborted`;
if completion won the race, the method returns the existing completed snapshot.
Repeating an abort is safe and preserves terminal results. Aborting an unknown
run returns a gateway 404; expired runs return an Expired snapshot.

The response confirms a durable state change. Gateway workers observe it by
polling PostgreSQL every 250 ms, then drop queued admission or active provider
work and release capacity. This does not guarantee the upstream provider stops
generation or avoids billing for work already received. Waiters observe the
aborted state as `ClientError::RunAborted { run_id }`. A dropped or failed abort
request may already have taken effect; explicitly retry or retrieve the run to
resolve that ambiguity.

## Recovery and failures

- Keys contain 1–256 visible ASCII characters. They are shared across runtime
  callers of the gateway database, independently of request metadata.
- Persist a key before submission. If acceptance is ambiguous, explicitly
  resubmit the same key and identical typed input to recover the original run.
  The client never automatically retries or generates a replacement key.
- A changed input with the same key returns a typed gateway conflict. Failed and aborted
  runs are also deduplicated; a deliberate new generation requires a new key.
- Gateway/provider failures preserve kind, retryability, backoff, selected
  account, and nested provider error data. `can_retry` is metadata; it does not
  cause the client to submit another billable request.
- The current gateway retains results for 48 hours and keys until seven days
  after completion. Expired results produce HTTP 410 and an Expired snapshot.
  After cleanup, retrieval returns 404 and the old key may create a new run.
- In-flight provider execution is process-local in the current gateway. This
  client does not add recovery for a gateway restart or crash.

## Verification

From `llm/`, run `cargo test -p llm-client` and
`cargo clippy -p llm-client --all-targets -- -D warnings`.
