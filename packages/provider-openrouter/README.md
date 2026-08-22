# provider-openrouter

A curated-catalog, non-streaming implementation of OpenRouter's Chat
Completions API for `llm_contracts::LlmTransport`.

The package currently accepts only these curated models:

- `google/gemini-3.7-flash`
- `meta/muse-spark-1.2-contributor`
- `deepseek/deepseek-v4-flash-vision-exp`
- `z-ai/glm-5.3`
- `stealth/ox-alpha`

Additions to `OPENROUTER_MODELS` are intentionally explicit: arbitrary
OpenRouter model IDs, including IDs supplied through the `models` fallback
option, are rejected until they are reviewed and added to the catalog.

Muse Spark 1.2 Contributor is Meta's lower-cost data-sharing tier. OpenRouter
states that prompts and outputs may be retained and used to improve Meta's
products. Applications should select that model only when this policy is
acceptable to the user.

```rust,no_run
use llm_contracts::{LlmRequest, LlmTransport};
use provider_openrouter::OpenRouterProvider;

# async fn run(request: LlmRequest) -> Result<(), llm_contracts::LlmError> {
let provider = OpenRouterProvider::from_api_key("api-key")?;
let message = provider.complete(request).await?;
println!("{}", message.native_message);
# Ok(())
# }
```

Every request explicitly sends `stream: false`. The default timeout is 30
minutes and can be changed with `OpenRouterConfig::with_timeout`. OpenRouter's
optional application attribution headers can be configured with
`with_http_referer` and `with_app_title`.

OpenRouter-native assistant messages are replayed without modification during
follow-up calls. This preserves ordered `reasoning_details`, signatures,
encrypted reasoning, provider-specific tool state, and future native fields.
Cross-provider assistant messages are reconstructed from normalized content.

The returned `AssistantMessage` retains the complete native response and the
actual model ID reported by OpenRouter. When OpenRouter supplies `usage.cost`,
that value is authoritative because routes, fallbacks, plugins, discounts, and
BYOK can change the actual charge. Catalog pricing is used as a fallback when
native cost is absent.

The library never reads environment variables. Applications load credentials
and pass them through `OpenRouterConfig`. An ignored live test exercises Gemini
at its full configured output allowance with non-streaming transport:

```sh
OPENROUTER_API_KEY=... cargo test -p provider-openrouter --test live \
  -- --ignored --nocapture
```
