# Sites in the dashboard

The project sidebar has a Sites tab below Environments. It contains only a table
and Open links. Each link opens `/projects/{project}/sites/{site}/view` in a new
tab with a full-page sandboxed iframe. There are no creation, lifecycle, authoring,
preview editing, release management, or harness authoring controls in this version.

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

The middleware exposes only listing, opening a view, and backend invocation.
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

Published content uses scoped private tickets and a separate origin. The viewer
keeps its original backend release if another release becomes active. After one
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
