# LLM contracts

Provider-neutral request, response, message, tool, usage, error, and transport
contracts for the standalone LLM workspace.

Completion and provider-specific extension APIs are deliberately separate.
`LlmTransport` represents model completion, while optional APIs such as search
use their own transport traits. Provider capability discovery is intentionally
left for a later package revision.
