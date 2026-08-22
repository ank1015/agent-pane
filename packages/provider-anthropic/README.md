# provider-anthropic

A catalog-backed, non-streaming Anthropic Messages API implementation of
`llm_contracts::LlmTransport`.

Only exact model IDs in `ANTHROPIC_MODELS` are accepted. The selected catalog
entry supplies the normalized model name, token prices, context window, and
output limit. Every successful call returns an `AssistantMessage` containing
the complete, untouched Anthropic Message in `native_message`.

```rust,no_run
use llm_contracts::{LlmRequest, LlmTransport};
use provider_anthropic::AnthropicProvider;

# async fn run(request: LlmRequest) -> Result<(), llm_contracts::LlmError> {
let provider = AnthropicProvider::from_api_key("api-key")?;
let message = provider.complete(request).await?;
println!("{}", message.native_message);
# Ok(())
# }
```

The package sends `stream: false` and parses the one complete response. Its
default HTTP timeout is 30 minutes because large `max_tokens` requests may keep
the connection open for a long time. Override it with
`AnthropicConfig::with_timeout` when the owning application needs a different
bound.

`max_tokens` may be as high as the selected catalog model's output limit. The
streaming recommendation enforced by some Anthropic SDKs above 21,333 tokens is
SDK-side protection against long-lived HTTP connections; this package talks to
the API directly and intentionally supports non-streaming requests.

Anthropic-native response content is important conversation state. Follow-up
requests to Anthropic replay its `content` blocks without modification, keeping
thinking signatures, redacted thinking, hosted-tool results, and future native
blocks intact. Cross-provider assistant messages are converted from normalized
text and portable tool calls.

Usage cost includes input, output, cache reads, and cache creation. When the API
returns its five-minute/one-hour cache-creation breakdown, each TTL is charged
at its own catalog rate. The Sonnet 5 catalog entry currently uses Anthropic's
introductory direct-API price effective through August 31, 2026.

The library never reads environment variables. Applications load credentials
and pass them through `AnthropicConfig`. Optional beta features can be enabled
with `AnthropicConfig::with_beta_header`. Standard Anthropic API keys use
`AnthropicConfig::new`; compatible services that explicitly require
`Authorization: Bearer`, including Claude Platform on AWS, can use
`AnthropicConfig::from_bearer_token`.

An ignored live integration test exercises every catalog model with
`max_tokens: 128000` and `stream: false`:

```sh
ANTHROPIC_API_KEY=... cargo test -p provider-anthropic --test live \
  -- --ignored --nocapture --test-threads=1
```
