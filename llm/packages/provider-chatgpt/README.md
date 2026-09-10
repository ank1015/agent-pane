# provider-chatgpt

A catalog-backed ChatGPT Codex backend implementation of
`llm_contracts::LlmTransport`.

Applications provide a ChatGPT OAuth access token and ChatGPT account ID. This
crate does not read environment variables, perform login, refresh tokens, or
persist credentials.

```rust,no_run
use llm_contracts::{LlmRequest, LlmTransport};
use provider_chatgpt::ChatGptProvider;

# async fn run(request: LlmRequest) -> Result<(), llm_contracts::LlmError> {
let provider = ChatGptProvider::from_credentials("oauth-access-token", "account-id")?;
let message = provider.complete(request).await?;
println!("{}", message.native_message);
# Ok(())
# }
```

The provider separately implements `llm_contracts::SearchTransport` for the
Codex provider-backed search operation at
`POST /backend-api/codex/alpha/search`. It uses the same OAuth access token,
ChatGPT account ID, timeout, and normalized error behavior as completions while
returning the non-streaming native search response.

The public operation returns one complete `AssistantMessage`. Internally, the
ChatGPT backend requires SSE, so every request forces:

```json
{
  "store": false,
  "stream": true,
  "include": ["reasoning.encrypted_content"]
}
```

The ChatGPT backend rejects the public Responses API's `max_output_tokens`
parameter. This provider rejects that option locally with `invalid_request`
instead of sending it or silently ignoring it.

When `provider_options.prompt_cache_key` is present, it must be a string. The
provider sends the same value as `prompt_cache_key` in the request body and as
the `session-id` and `x-client-request-id` headers. These stable affinity
signals match the Codex backend clients and keep append-only turns on a reusable
prompt-cache route.

`AssistantMessage::native_message` retains the terminal response with its
`instructions` and `tools` fields removed, and the completed native output items
used for exact follow-up replay. These echoed request settings would otherwise
accumulate in conversation history. Previously stored assistant messages are
not rewritten by this change. Transient SSE
events are consumed internally but omitted from the returned message so they do
not inflate gateway responses and persisted transcripts.

`CHATGPT_MODELS` directly shares the four generated entries and pricing from
`provider-openai`, making the catalog a strict allowlist that cannot drift from
the OpenAI provider catalog.

An ignored live integration test exercises every catalog model. The application
or test runner remains responsible for loading credentials:

```sh
CHATGPT_ACCESS_TOKEN=... CHATGPT_ACCOUNT_ID=... \
  cargo test -p provider-chatgpt --test live \
  -- --ignored --nocapture --test-threads=1
```
