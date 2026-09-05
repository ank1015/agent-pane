# Firecrawl scrape

Port of the old `packages/tools/firecrawl/scrape` tool, using Platform's current
`llm-contracts`. The model-facing name is `scrape`; its schema is
`{ "url": "https://..." }` with no extra properties.

Scrape retains Markdown/main-content extraction, PDF parsing, one-hour cache age, automatic proxy, ad/base64-image filtering, and a 60-second Firecrawl timeout (65-second HTTP timeout). Markdown keeps its original roughly 50 KiB UTF-8-safe head/tail truncation.

Use `definition()` in LLM requests, `parse_arguments()` for typed arguments,
and `execute()` or `execute_scrape_tool()` to execute. Outputs contain LLM
content parts plus structured details; errors expose stable names and details.
Web content is untrusted data; the harness should not treat it as instructions.

The harness injects `FirecrawlScrapeToolContext::new(api_key)`, or uses
`from_env()` after the worker loads `FIRECRAWL_API_KEY`. No package-local dotenv
file is loaded or copied. Context Debug output omits credentials. Default clients
do not follow redirects. `with_client()` permits an injected trusted client and
HTTPS endpoint (loopback HTTP for tests); its caller owns redirect policy.
Per-request timeouts still apply to injected clients.

Transport bodies are bounded while streaming, even without Content-Length
(16 MiB). Final text is capped at 64 KiB with an explicit truncation marker;
serialized details are capped at 64 KiB, with oversized details replaced by a
truncation notice. Normal-sized results preserve the old output shape. Error
messages are capped at 4 KiB. JSON encoding adds overhead to text limits.

No automatic retries are added: Firecrawl calls may consume credits, and dropping
the future does not guarantee cancellation of already accepted remote work.
The harness owns cancellation, checkpointing results, and any retry policy.

Run `cargo test -p tool-firecrawl-scrape` from `platform/`. Live tests are
ignored by default because they require credentials and consume credits.
