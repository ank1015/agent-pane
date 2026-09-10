# provider-openai

A catalog-backed, non-streaming OpenAI Responses API implementation of
`llm_contracts::LlmTransport`.

Only model IDs in `OPENAI_MODELS` are accepted. The selected catalog entry
provides the normalized model name, token prices, context window, and output
limit. Every successful response includes the OpenAI response in
`AssistantMessage::native_message` with top-level `instructions` and `tools`
removed, plus a catalog-priced usage breakdown. These echoed request settings
would otherwise accumulate in conversation history. Native `output` and all
other response fields are preserved; follow-up replay uses only `output`.
Previously stored assistant messages are not rewritten by this change.

```rust,no_run
use llm_contracts::{LlmRequest, LlmTransport};
use provider_openai::OpenAiProvider;

# async fn run(request: LlmRequest) -> Result<(), llm_contracts::LlmError> {
let provider = OpenAiProvider::from_api_key("api-key")?;
let message = provider.complete(request).await?;
println!("{}", message.native_message);
# Ok(())
# }
```

The provider separately implements `llm_contracts::SearchTransport` for the
Codex provider-backed
`POST /v1/alpha/search` API. Search request and response types live in
`llm-contracts`, including the native command/settings schema and optional
`originator` and `x-codex-turn-metadata` forwarding.

The library does not read environment variables. Applications should load and
pass credentials explicitly.

Synchronous responses use a configurable 15-minute timeout by default. This is
long enough for many high-reasoning requests while still bounding a stalled
connection. Applications can override it with `OpenAiConfig::with_timeout`.

An ignored live integration test exercises every catalog model with a small
non-streaming request:

```sh
OPENAI_API_KEY=... cargo test -p provider-openai --test live \
  -- --ignored --nocapture --test-threads=1
```
