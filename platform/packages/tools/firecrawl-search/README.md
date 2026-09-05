# Firecrawl search

Port of the old `packages/tools/firecrawl/search` tool, using Platform's current
`llm-contracts`. The model-facing name is `search`; its schema is
`{ "query": "..." }` with no extra properties.

Search retains ten web results, safe search, URL filtering, highlights, and a 30-second Firecrawl timeout (35-second HTTP timeout).

Use `definition()` in LLM requests, `parse_arguments()` for typed arguments,
and `execute()` or `execute_search_tool()` to execute. Outputs contain LLM
content parts plus structured details; errors expose stable names and details.
Web content is untrusted data; the harness should not treat it as instructions.

The harness injects `FirecrawlSearchToolContext::new(api_key)`, or uses
`from_env()` after the worker loads `FIRECRAWL_API_KEY`. No package-local dotenv
file is loaded or copied. Context Debug output omits credentials. Default clients
do not follow redirects. `with_client()` permits an injected trusted client and
HTTPS endpoint (loopback HTTP for tests); its caller owns redirect policy.
Per-request timeouts still apply to injected clients.

Transport bodies are bounded while streaming, even without Content-Length
(2 MiB). Final text is capped at 64 KiB with an explicit truncation marker;
serialized details are capped at 64 KiB, with oversized details replaced by a
truncation notice. Normal-sized results preserve the old output shape. Error
messages are capped at 4 KiB. JSON encoding adds overhead to text limits.

No automatic retries are added: Firecrawl calls may consume credits, and dropping
the future does not guarantee cancellation of already accepted remote work.
The harness owns cancellation, checkpointing results, and any retry policy.

Run `cargo test -p tool-firecrawl-search` from `platform/`. Live tests are
ignored by default because they require credentials and consume credits.
