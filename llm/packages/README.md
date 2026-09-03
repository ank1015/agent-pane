# LLM packages

Reusable contracts and implementation crates in the standalone LLM architecture belong in this directory.

- `llm-contracts`: provider-neutral completion contracts plus separate
  extension contracts for optional provider APIs such as search.
- `provider-openai`: catalog-backed OpenAI Responses and provider-backed search
  transport.
- `provider-chatgpt`: ChatGPT Codex backend transport built on the shared OpenAI
  catalog and response mapping.
- `provider-fireworks`: catalog-backed, non-streaming Fireworks Chat Completions
  transport.
