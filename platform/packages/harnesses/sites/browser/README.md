# Sites browser runtime

Trusted Node + Playwright helper for the Sites harness's three-action browser.
The public API is documented in `../src/tools.d.ts`; there is no exposed Page
API, Node evaluation, model-selected URL, or separate outer browser tool.

Install once for the worker's OS/user:

```sh
npm ci
npm run install-browser
npm run check
npm test
```

Set `SITES_BROWSER_NODE` to the absolute Node executable and
`SITES_BROWSER_SCRIPT` to this directory's `runtime.mjs`. Optionally set
`PLAYWRIGHT_BROWSERS_PATH` to the installed browser cache. The worker performs
the check at startup whenever Sites is enabled. Chromium's native sandbox is
mandatory; never add `--no-sandbox`. Linux images also need Playwright's system
dependencies and a non-root user/kernel configuration supporting the sandbox.
Use resource-bounded containers in production. No Codex runtime is required.

## Internal transport

The Rust session starts one helper per active run, serializes tool actions and
passes newline-delimited JSON over private pipes. Node receives no Platform
credentials. This protocol is host-internal, not an additional model API:

- Rust → Node: `{kind:"action",id,input,preview}`. Preview is supplied only on
  first use or explicit reload and includes the signed content URL, configured
  dashboard origin, release ID and expiry. It is never model-selected.
- Node → Rust: `{kind:"result",id,result}` or `{kind:"result",id,error}`.
- Node → Rust: `{kind:"invoke",release_id,request:{id,endpoint,input?}}` from the
  trusted wrapper's MessageChannel. Rust forwards this through the current
  leased RunClient's `sites.browserInvoke`, not through a stored bearer token.
- Rust → Node: `{kind:"backend_result",id,result}` or `{...,error}`.

The wrapper is locally fulfilled at the configured dashboard origin; no human
dashboard login is used. The content document and its CSP/SDK are unmodified.
Only the actual main-frame binding caller may forward a backend call. The site
frame remains opaque-origin and cannot inspect the wrapper. A release-scoped
network filter blocks unrelated navigation/resources and all WebSockets.

Browser backend requests use the loaded release, even after a patch activates
new code. Reload creates a fresh context/bridge for the latest release. Backend
effects are real, and closing a page is not rollback. Requests already admitted
can finish after cancellation or lease loss; new requests must reauthorize.

The helper continues servicing application callbacks between tool calls. An
outstanding action's cancellation closes the helper. EOF, SIGTERM, run teardown,
30-second action timeout, and five-minute idle expiry clean up browser state.
Lost state requires explicit reload; evaluate source is never replayed.

Screenshots are 1024×768 JPEGs, at quality 80/60/40, with an 80 KiB binary cap.
The returned MCP-shaped image fits existing code-mode JSON/output limits, so no
global limit increases or external image storage are required. Image overflow
fails explicitly. Evaluate returns at most 96 KiB of JSON.

Tests use real Chromium with a local content fixture and the production frontend
SDK. The full Rust harness/lease/SQLite/media integration test is documented in
the parent README. No tests contact hosted models or user sites.
