# Sites harness

A site-authoring harness with exactly two outer tools, raw JavaScript `exec`
and JSON `wait`, and eight inner tools:

- `search({query})`: up to ten public-web results with source metadata.
- `scrape({url})`: bounded Markdown from a public webpage or PDF.
- `metadata()`: bound site metadata, no source listing.
- `read({file_path, offset?, limit?})`: one of index.html/backend.js, shared
  line-window reading, source revision and continuation information.
- `apply_patch(patch)`: raw patch text; Update edits, Add replaces, Delete resets
  to blank HTML/a 404 backend. Only the two Site paths, no moves; ordered same-file
  Delete/Add is supported. Stages the final pair and validates backend
  and atomically activates source. Code-mode success is `{}`, matching the
  referenced Codex nested patch result, without a Codex dependency.
- `invoke({method,path,query?,body?,idempotency_key?})`: live backend response
  and diagnostics.
- `browser({action:"evaluate",code})`: async function body in the real site
  frame; returns `{action:"evaluate",result}` with JSON (no return = null).
  `browser({action:"screenshot"})` returns `{action:"screenshot",image}` for
  `image(result.image)`. `browser({action:"reload"})` loads the current release
  and returns `{action:"reload",release_id}`.
- `sql({sql,params?,idempotency_key?})`: one bounded SQLite statement,
  including schema inspection, schema changes, DML and RETURNING.

There are no agent-side ctx wrappers, raw Platform tools or legacy browser
probes. The backend still receives its application SDK through ctx;
its reference is clearly labeled backend-only in the prompt. See
[src/tools.d.ts](src/tools.d.ts) for the model-facing contracts.

The six Site tools use the session's site binding; search and scrape access only
public web content. Metadata/reading may lazily provision a new site; new sites
have default two-file scaffolding. Edits are live, not drafts. Code snapshots and
rollback are user operations and do not undo SQL. Internal authoring/invocation
records remain service infrastructure, not tools.

Code mode uses the independent tool-code-mode live session with no extensions.
The host checkpoints cell admission; interrupted scripts are not replayed.
Nested site mutations receive stable per-call operation keys; explicit keys
are scoped to the agent run and permit same-input retries across its cells.
Cancellation does not undo accepted effects. Unknown outcomes remain explicit.
The store and live cells survive only within a run activation.

First source/SQL/patch access waits up to five seconds for lazy provisioning;
only readiness is polled, never the requested mutation. Public service rejection
codes and explanations reach the model explicitly; generic transport logs still
omit error bodies. Backend schema permissions remain separate from authoring.

Saved assistant records and outgoing LLM requests retain the complete native
provider message unchanged. The harness does not filter provider metadata or
summarize history; oversized histories can exceed gateway request-size limits.

Browser state persists across calls/cells within one run activation, not across
worker replacement or runs. Calls serialize. The browser runs sandboxed Chromium
via a trusted Node/Playwright helper, with no worker credentials in that process.
Model JavaScript runs only in the opaque-origin site frame, not in Node. The
trusted preview wrapper speaks the production MessageChannel protocol; each
backend request is admitted through the current leased RunClient and pinned to
the loaded release. Patches do not reload the page. Explicit reload discards it
and connects the latest frontend/backend pair. All backend effects are live.

The v1 viewport is 1024x768. Screenshots use JPEG quality 80, 60, then 40 until
the image fits 80 KiB; otherwise they fail explicitly. No resizing/truncation.
This keeps base64 plus envelope below the existing 128 KiB code-mode tool-result
limit and one emitted image below the 256 KiB cell-output limit. Emit/collect
images promptly instead of accumulating them in one cell. Evaluate returns at
most 96 KiB of JSON and accepts at most 48 KiB of source. Actions time out after
30 seconds and destroy timed-out pages. Idle helpers exit after five minutes;
preview tickets expire after fifteen. Cancellation/crashes require explicit
reload, never automatic replay. Reset does not roll back backend effects.

Install with `npm ci && npm run install-browser` in `browser/`. Configure
SITES_BROWSER_NODE and SITES_BROWSER_SCRIPT with absolute paths to Node and
browser/runtime.mjs; PLAYWRIGHT_BROWSERS_PATH optionally selects the installed
browser cache. Workers fail startup if the native Chromium sandbox cannot launch.
Production should run the worker/browser in resource-bounded containers as a
non-root user with Chromium sandbox support; browser contexts alone are not OS
isolation. Content requests are restricted to the signed release path, redirects
and WebSockets are blocked, and production content sandbox headers are required.
Sites-only workers need no execution gateway, but do require FIRECRAWL_API_KEY for
the nested search and scrape tools. Enable SITES_ENABLED with the LLM gateway and
content listener configured.

Checkpoint protocol v4 is breaking: older checkpoints are rejected, not migrated.
Start a new Sites session for the new tool surface. Site data is preserved.
Only OpenAI/ChatGPT providers currently support the outer custom-tool contract.

Verification:
`cargo test -p sites-harness -p tool-read -p tool-code-mode -p tool-firecrawl-search -p tool-firecrawl-scrape`.
The service SQL tests cover real SQLite reads, writes, schema, bounded results
and receipts. Server integration tests additionally require PostgreSQL and
built platform-sites-service/code-mode-runtime binaries.
Browser tests: `npm test` in `browser/` (real Chromium, local fixture, no hosted
services). End-to-end harness test, including an image in the next model request:
`DATABASE_URL=postgresql:///postgres SITES_TEST_BROWSER_NODE=/absolute/path/to/node cargo test -p platform-server --test sites browser_real_frontend_backend_sql_screenshot_and_pinned_reload -- --ignored`.
