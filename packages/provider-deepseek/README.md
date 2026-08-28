# provider-deepseek

A catalog-backed, non-streaming implementation of DeepSeek's Chat Completions
API for `llm_contracts::LlmTransport`.

The catalog accepts exactly:

- `deepseek-v4-flash`, currently serving DeepSeek V4 Flash 0731
- `deepseek-v4-pro`, currently serving DeepSeek V4 Pro 0813
- `deepseek-v4-flash-vision-exp`, the experimental multimodal V4 Flash model

```rust,no_run
use llm_contracts::{LlmRequest, LlmTransport};
use provider_deepseek::DeepSeekProvider;

# async fn run(request: LlmRequest) -> Result<(), llm_contracts::LlmError> {
let provider = DeepSeekProvider::from_api_key("api-key")?;
let message = provider.complete(request).await?;
println!("{}", message.native_message);
# Ok(())
# }
```

Every request explicitly sends `stream: false`. The default timeout is 30
minutes. DeepSeek keeps long non-streaming connections alive with blank lines;
the client buffers those safely before parsing the complete JSON response.

All models have a 1,048,576-token context window and a 384,000-token output
limit. Only `deepseek-v4-flash-vision-exp` accepts image content. It supports
JPEG, PNG, GIF, and WebP through HTTP URLs or base64 data URLs.

DeepSeek-native assistant messages are replayed verbatim in follow-up requests,
retaining `reasoning_content`, native tool calls, log-probability data, and
future response fields. When thinking and tools are enabled, assistant history
without reasoning is sent with an empty `reasoning_content` field because
DeepSeek requires the field on tool-bearing multi-round requests. The complete
native response is retained in the returned `AssistantMessage`.

Pricing changes by request start time. Peak windows are 01:00–04:00 and
06:00–10:00 UTC on Monday through Friday; weekends and all other hours are
off-peak. Usage cost separately prices cache misses, cache hits, and output
tokens using the catalog tier active when the request began.

The library never reads environment variables. Applications load credentials
and pass them through `DeepSeekConfig`. An ignored live test exercises every
models with `max_tokens: 384000` and non-streaming transport:

```sh
DEEPSEEK_API_KEY=... cargo test -p provider-deepseek --test live \
  -- --ignored --nocapture --test-threads=1
```
