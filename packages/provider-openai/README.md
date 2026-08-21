# provider-openai

A catalog-backed, non-streaming OpenAI Responses API implementation of
`llm_contracts::LlmTransport`.

Only model IDs in `OPENAI_MODELS` are accepted. The selected catalog entry
provides the normalized model name, token prices, context window, and output
limit. Every successful response includes the untouched OpenAI response in
`AssistantMessage::native_message` and a catalog-priced usage breakdown.

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

