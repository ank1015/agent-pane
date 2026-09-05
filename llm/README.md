# LLM system

This directory contains the clean-slate LLM architecture while the legacy LLM gateway, contracts, and provider code remain in their current locations.

- `apps/` contains independently deployable LLM services.
- `apps/llm-gateway` executes provider calls, stores encrypted provider
  accounts, and records request outcomes and usage.
- `packages/llm-contracts` contains provider-neutral completion contracts and
  optional provider API contracts such as search.
- `packages/llm-client` provides typed submission, retrieval, and waiting for
  gateway-managed LLM runs.
- `packages/provider-openai` implements OpenAI Responses completion and the
  optional provider-backed search API.
- `packages/provider-chatgpt` implements the ChatGPT Codex backend using its
  OAuth/account transport and the shared OpenAI model catalog.
- `packages/provider-fireworks` implements Fireworks Chat Completions against a
  strict standard-serverless model catalog.
- Other reusable LLM implementation crates belong under `packages/`.

Additional applications and packages will be added as the new architecture is
defined.

`llm/` is an independent Cargo workspace. After the first workspace member is added, run its complete local verification from this directory:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets
cargo test --workspace
```
