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

`AssistantMessage::native_message` retains every raw SSE JSON event, the raw
terminal response, and the completed native output items used for exact
follow-up replay.

`CHATGPT_MODELS` directly shares the three generated entries and pricing from
`provider-openai`, making the catalog a strict allowlist that cannot drift from
the OpenAI provider catalog.

An ignored live integration test exercises every catalog model. The application
or test runner remains responsible for loading credentials:

```sh
CHATGPT_ACCESS_TOKEN=... CHATGPT_ACCOUNT_ID=... \
  cargo test -p provider-chatgpt --test live \
  -- --ignored --nocapture --test-threads=1
```
