# llm-contracts

Provider-neutral Rust contracts shared by Agent Pane applications and packages.

The crate provides serializable messages, requests, model metadata, tools,
usage, provider errors, and a non-streaming transport trait. Contract JSON uses
snake-case field names and discriminators.

Every successful `AssistantMessage` must contain `native_message`. Its value is
opaque JSON so the original provider response is retained without coupling the
shared message collection to one provider SDK.

## Parsing untrusted JSON

Use the validation helpers at process and network boundaries:

```rust
use llm_contracts::{LlmRequest, validation::parse_json};

# fn parse(input: &str) -> Result<(), llm_contracts::ContractError> {
let request: LlmRequest = parse_json(input)?;
# Ok(())
# }
```

## Defining a typed function tool

`FunctionTool` stores ordinary JSON Schema, keeping `LlmRequest` serializable.
The schema can be generated from an application-owned Rust type:

```rust
use llm_contracts::FunctionTool;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
struct WeatherArguments {
    city: String,
}

# fn make_tool() -> Result<(), llm_contracts::ValidationError> {
let tool = FunctionTool::for_type::<WeatherArguments>(
    "get_weather",
    "Get the current weather for a city",
)?;
# let _ = tool;
# Ok(())
# }
```

Executable handlers stay outside this crate and can be registered by tool name.

