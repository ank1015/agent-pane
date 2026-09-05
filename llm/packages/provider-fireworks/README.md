# provider-fireworks

Catalog-backed Fireworks Chat Completions transport for Agent Pane.

The package accepts an `llm_contracts::LlmRequest` and returns one complete
`llm_contracts::AssistantMessage`. Fireworks supports non-streaming requests,
so the wire request explicitly sends `stream: false` and parses a single JSON
response. The full native Chat Completion is retained in `native_message`.

## Supported models

The strict standard-serverless allowlist is:

- `accounts/fireworks/models/glm-5p3-flash`
- `accounts/fireworks/models/glm-5p3`
- `accounts/fireworks/models/kimi-k3`
- `accounts/fireworks/models/deepseek-v4-pro-0813`
- `accounts/fireworks/models/deepseek-v4-flash-0731`
- `accounts/fireworks/models/qwen3p8-2p4t-a95b`

Catalog pricing is expressed in USD per million uncached input, cached input,
and output tokens. Usage cost is calculated from the response's
`prompt_tokens`, `prompt_tokens_details.cached_tokens`, and
`completion_tokens` fields.

Priority service tier requests are rejected because their rates differ from
the standard catalog. Fast router identifiers are also intentionally outside
the allowlist. The request builder forces `n: 1` because the shared transport
returns exactly one assistant message.

## Usage

```rust
use provider_fireworks::FireworksProvider;

let provider = FireworksProvider::from_api_key(api_key)?;
let assistant = llm_contracts::LlmTransport::complete(&provider, request).await?;
```

The package only accepts credentials from its caller. It does not read the
environment or filesystem.

Same-provider assistant messages replay choice zero's complete native message,
including `reasoning_content`, `internal_content`, and native tool calls.
Provider-specific raw message sequences can be supplied through a custom
message tagged `fireworks.native_input` whose content contains a `messages`
array.

## Live verification

The ignored live test sends a non-streaming request with a one-million-token
maximum to every catalog model:

```sh
FIREWORKS_API_KEY=... cargo test -p provider-fireworks --test live -- --ignored --nocapture
```
