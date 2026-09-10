# Sites in the dashboard

The project sidebar has a Sites tab below Environments. It uses the same table
styling and row layout as Environments. **Open** opens the live site in a new tab;
the site name opens its management page, where **Edit with Sites** is available.
The final three-dot menu uses the same Update Name and Delete dialogs as
Environments. Delete removes the site from listings and takes it offline while
retaining files and data; there is no dashboard undelete action.
**Create site** and **Edit with Sites**
use the existing project chat composer, model/account selection and conversations.
Sites is a Platform-owned harness, so every project gets it while it is globally
enabled. A Sites-capable worker must also be running.

The composer offers New site or an existing project site. Its immutable `siteId`
is null for a new site, created lazily on the first authoring call. Conversations
can investigate without authoring. Multiple conversations may edit one live site.
A derived `site_id` in session reads links a lazy-created site without changing the
frozen configuration. The management page lists up to 20 recent authoring chats.

The agent edits the live frontend/backend pair. There are no drafts, private
copies or publish steps. The site page shows a preview, lets the user rename the
site, save named code snapshots and restore one. Restoring affects code only;
SQLite data, accepted operations and running conversations remain. Snapshot pages
use an exclusive UUID cursor. Mutation IDs are saved in sessionStorage before
sending; lost replies are inspected and retried using the exact same identity.
Backend activity shows recent invocation status and saved bounded diagnostics.

## Try the isolated demo

From the repository root (local PostgreSQL with CREATEDB, Node/npm and installed
dashboard dependencies are required):

```sh
cargo build --manifest-path platform/Cargo.toml -p platform-sites-service
DATABASE_URL=postgresql:///postgres cargo run --manifest-path platform/Cargo.toml \
  -p platform-server --example sites-preview
```

The command prints a project Sites URL on `http://127.0.0.1:5176`. Open the listed
site, choose the harness, enter a task, and click Run session. Close the site and
reopen it to inspect the saved session and messages. The demo uses real Platform,
PostgreSQL, sites-service, SQLite, content tickets, and the dashboard bridge.
Only agent execution is simulated; it never contacts real gateways or machines.
Its credentials are temporary test credentials in the fixture, not user secrets.
Ctrl-C stops the child services and removes the disposable database and site data.
Port 5176 must be available. An abnormal forced kill may require manual cleanup
of a `sites_preview_*` database; it never uses the configured application database.

The fixture reuses the existing sites integration-test helpers. It is a manual
validation command, not an additional deployed service.

## Connect your existing local project

First configure Platform and sites-service according to
[Platform Sites setup](../server/SITES.md), including project harness/account
grants. Start the separate content listener with matching origins:

```dotenv
# Sites service
SITES_CONTENT_ORIGIN=http://127.0.0.1:3103
SITES_CONTENT_BIND_ADDRESS=127.0.0.1:3103
SITES_DASHBOARD_ORIGIN=http://127.0.0.1:5173
```

In the dashboard's ignored `.env.local`:

```dotenv
DASHBOARD_ORIGIN=http://127.0.0.1:5173
DASHBOARD_PLATFORM_URL=http://127.0.0.1:3100
DASHBOARD_SITES_CONTENT_ORIGIN=http://127.0.0.1:3103
DASHBOARD_SITE_PROJECT_TOKENS={"<project UUID>":"<that project's existing credential>"}
```

Use the exact configured browser origin (`127.0.0.1` and `localhost` are different
origins). Restart Vite after changing configuration. All these values are read by
the server; never put project/service credentials into `VITE_*` variables.

To publish the example into that project, set `SITE_PROJECT_TOKEN` and
`SITES_API_TOKEN` in your shell's environment and run:

```sh
python3 platform/scripts/publish-demo-site.py --project <project UUID>
```

The script provisions a deterministic demo site ID, uploads the checked-in
frontend/backend, and activates a release using the observed release generation.
It does not change project permissions or grants. Reruns create a new immutable
release of the same site. The example has no application tables or migrations;
its sessions, messages, status, and metrics live in Platform. A compatible worker
and gateway configuration are needed for real agent execution outside the isolated
demo. Only enabled, explicitly site-permitted harnesses/accounts are exposed.

## Bridge boundary

`server/sites-bridge.ts` is a server-side Vite dev/preview middleware for the
existing trusted local dashboard. It is not a multi-user login system. Keep the
dashboard listener private. A deployment serving only `dist/` must provide the
equivalent authenticated backend routes; static hosting alone is insufficient.
`vite preview` also installs the middleware when configured with the same env.

The middleware exposes a narrow site-scoped allowlist: listing/detail, rename, soft deletion,
authoring-chat and diagnostic reads, snapshot/restore and operation inspection,
opening a view, and backend invocation. Only the parent dashboard can call
management routes; the iframe receives only the callBackend MessageChannel.
It rejects cross-origin/opaque-origin HTTP requests and unexpected Host values.
The browser parent receives a short-lived view token in memory, bound to the
project, site, and active frontend release resolved when the page opens. Backend
requests cannot override that scope. Project/service bearer credentials remain in
the server. The view token is never passed to the iframe or placed in a URL.

The sites content host injects only `window.callBackend(endpoint, input)` before
application scripts. A validated parent/iframe handshake transfers a private
MessageChannel. The parent validates source window, opaque origin, message shape,
request identity, size, and concurrency. Only the expected iframe gets the port.
Closing or navigating the viewer disposes its channel; accepted server-side work
continues. Request timeouts do not imply that a mutation was rolled back. Site
backends should use stable application operation keys for retries.

The iframe allows scripts but not same-origin access, forms, popups, top
navigation, or downloads. The content CSP blocks fetch/WebSocket connections,
workers, external scripts/assets, objects, base tags, and form submission. The
parent CSP restricts iframe navigation to the configured content origin. Use
buttons with JavaScript handlers rather than native form submission. Sites cannot
call the dashboard's project APIs directly.

Published content uses scoped private tickets and a separate origin. Each mounted iframe pins its frontend and backend to the same release. The viewer
and embedded preview poll the live release every five seconds; activation or
restore opens a fresh iframe/channel for the new pair. Accepted work in an older
invocation continues using its pinned release. After one
hour, or a dashboard-server restart, reload the view to renew access. The bridge
allows eight outstanding calls per iframe, 128 KiB requests, and bounded replies;
page large session histories through the backend. Existing sites-service limits
and project access checks still apply to every invocation.

## Validation

```sh
cd platform/apps/dashboard
npm test
npm run lint
npm run build
```

Tests cover credential confinement, origin rejection, release binding, invalid
scope overrides, bounded requests, backend failures, and the injected SDK's
parent handshake. Existing service tests cover content access and storage/runtime
boundaries. The isolated browser flow verifies navigation, iframe rendering,
run acceptance, closure/reopening, and persisted session inspection.


### Real Sites harness browser smoke

Start the configured Platform, Sites, worker and dashboard services. The manual
UI smoke requires `npm ci` and `npx playwright install chromium` in
`platform/apps/dashboard/tests/browser`. This is test-only Playwright setup;
the agent's three-action browser uses its separate runtime under
`platform/packages/harnesses/sites/browser` (see its harness README for worker configuration).
The UI smoke spends real
model usage and changes only an explicitly selected test site.

From Create site, select Sites and the configured ChatGPT account / GPT-5.6 Luna.
Ask it to create a self-contained counter with heading `Sites E2E v1`, an Increment
button, `#count`, and backend `/count` and `/increment` returning `{count}`. Require
persistent SQLite data, `callBackend`, no external assets, and backend verification.
Save the returned IDs to a local JSON file (no credentials):

```json
{"project":"<project UUID>","session":"<authoring session UUID>","site":"<new test site UUID>"}
```

Then, from the dashboard directory:

```sh
SITES_LIVE_E2E=1 SITES_E2E_STATE=/absolute/path/to/test-state.json node tests/sites-live-ui.mjs
```

This checks the authenticated backend counter, user rename, snapshot acceptance
with a lost POST reply, a second Sites conversation editing the same site,
automatic preview refresh, user restore, unchanged counter data and browser errors.
It saves operation/run IDs in the state file and desktop/narrow screenshots in
`/tmp/sites-live-*.png`. It deliberately leaves the test site and conversations
available for inspection. The test requires the named model; use the isolated
service tests for deterministic CI without real accounts.

### Live callback and restart check

On the same test counter, author a Launch check button (`#launch`), status text
(`#job-status`) and these backend routes: `/launch` accepts `{operationKey}` and
returns a durable job, `/job` reads it, `/latest-job` returns `{job}`, and
`/completed` handles a trusted Platform callback. Job fields are `operation_key`,
`session_id`, `run_id`, `status` and `event_id`. Render `Callback child completed.`
only on actual completion. Launch a short research-only Sites child with null
`siteId`; this also checks that a completed run need not publish execution outputs.
Keep launch input IDs, timestamps, config and callback payload stable on retries.
The iframe cannot use browser storage; resume by reading the saved job on load.
Use the callback shape and retry rules in the Sites system prompt.

With the worker stopped/drained and no unrelated active work, run:

```sh
SITES_LIVE_E2E=1 SITES_E2E_STATE=/absolute/path/to/test-state.json \
  SITES_CALLBACK_PHASE=launch node tests/sites-live-callback.mjs
```

Restart Platform and Sites after launch acceptance, then start the worker. Run
with `SITES_CALLBACK_PHASE=verify` to check completion across restart, reload,
unchanged counter data and a duplicate launch returning the same session/run.
These phases deliberately do not stop services themselves. Inspect the saved
callback event/receipt to confirm deduplication; delivery is at least once, not
an exactly-once transport guarantee. The test uses the site's configured account
and can spend model credits. The completed Step 7 checks are recorded in
[the validation report](../../SITES_STEP7_VALIDATION.md).
