# Platform sites service

Stages 1–5: authenticated service, durable provisioning, versioned source/releases,
static serving, isolated JavaScript backends, site SQL SDK, schema migrations,
backups, invocation records, and trusted Platform completion delivery. See
[completion callbacks](../server/CALLBACKS.md) for durable background continuations. See [backend API and SDK](BACKEND.md) for Stage 3.
Platform SDK bindings use the trusted parent transport; see [Platform integration](../server/SITES.md).
Callbacks and dashboard integration remain later stages.
Builds run outside this service, for example in an agent's development sandbox.
This crate requires Rust 1.87+; backend execution needs macOS sandbox-exec or Linux
bubblewrap. Static serving and storage do not require the backend runtime.

## Run

From `platform/apps/sites-service/`, copy `.env.example` to `.env` and fill in:

- `SITES_DATA_DIR`: required **absolute path** to a dedicated persistent local
  directory. The service creates it if necessary. Do not use a shared working
  directory, a network filesystem, or disposable container storage.
- `SITES_API_TOKEN`: required secret of 32–256 visible ASCII characters, shared
  only with trusted Platform callers. For example, generate one with
  `openssl rand -hex 32`. Keep the `.env` file private.
- `SITES_BIND_ADDRESS`: defaults to `127.0.0.1:3102`.
- `RUST_LOG`: defaults to `info`.
- `SITES_CONTENT_ORIGIN` and `SITES_DASHBOARD_ORIGIN`: optional together; setting
  both enables static hosting. For local development, use e.g.
  `http://127.0.0.1:3103` and `http://127.0.0.1:5173`. Origins must have no paths,
  credentials, queries, or fragments. Non-loopback origins require HTTPS.
- `SITES_CONTENT_BIND_ADDRESS`: defaults to `127.0.0.1:3103`; must differ from the
  internal API bind address when static hosting is enabled.

```sh
cargo run -p platform-sites-service
```

The workspace's `apps/*` pattern includes this crate. You can also run it from
`platform/` with the variables exported. Dotenv reads from the working directory
and its ancestors, not automatically from the crate directory.

The service runs migrations, reconciles existing sites, and recovers staged
bundles before listening. It
handles SIGINT/SIGTERM by draining HTTP requests, stopping reconciliation, and
closing the metadata pool. New databases use SQLite WAL with `synchronous=FULL`.

## Storage and ownership

```text
SITES_DATA_DIR/
  .owner.lock
  service.sqlite
  sites/
    <site UUID>/
      data/
        site.sqlite
      revisions/
        <revision UUID>/
          .bundle.json
          backend.ts
          frontend/...
      releases/
        <release UUID>/
          .bundle.json
          backend.js
          public/...
  staging/
    <site UUID>-<revision|release>-<bundle UUID>/...
```

SQLite may also create `-wal` and `-shm` sidecars. The private `service.sqlite`
contains `sites`, `bundles`, activation receipts, the service migration ledger,
and a readiness probe row. Each
site database starts with the reserved `__sites_identity` table, binding it to its
site and project. Application tables are created by controlled site migrations; backend SQL is
scoped through the injected SDK, not exposed as a browser SQL endpoint.
`storage_initialized` is private metadata; it is never caller-controlled.

Platform will own logical project/site authorization. This internal service
trusts its authenticated Platform caller to supply an existing project UUID; the standalone provisioning route
does not validate project existence. The Stage 4 Platform facade validates project
access before provisioning and gates all injected Platform capabilities. The
site-to-project binding is immutable. Neither callers nor site code supply paths.

Run **one active owner** of this local volume. An advisory exclusive lock is held
for the service lifetime; a second service using the same directory fails startup.
Never delete `.owner.lock` while a process is running. On Unix, managed directories
are private (`0700`) and files are private (`0600`). Symlinks at managed directory,
database, and SQLite sidecar paths are rejected. The volume must not be writable
by untrusted processes; these checks are not a sandbox against concurrent host
filesystem tampering.

Published bundles and application databases are never automatically deleted.
An identical upload retry may replace its own incomplete staging directory.
Missing/corrupt initialized site storage is
reported as failed, never replaced with an empty database. Losing `service.sqlite`
beside existing site directories fails startup. A persistent volume is not a
backup: per-site consistent snapshots are available through the Stage 3 backup API.
Service-wide backup and automated restore remain operator responsibilities. Use SQLite-consistent
backups or stop the service before copying its data; do not copy only an active
database while ignoring its WAL.

## Internal HTTP API

Every route on the internal API listener, including health checks and unknown
routes, requires:

```http
Authorization: Bearer <SITES_API_TOKEN>
```

Keep the listener private. Terminate HTTPS at a trusted proxy if making service
calls across an untrusted network; the executable itself serves HTTP. The API
token never appears in URLs, responses, or request logs. This listener provides
no browser CORS access. The separate content listener uses scoped signed URLs,
described below.

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/healthz` | Process liveness; `200 {"status":"ok"}` |
| GET | `/readyz` | Validate service storage paths and commit a metadata probe write; `200 {"status":"ready"}` or 503 |
| PUT | `/internal/sites/{id}` | Provision with `{"project_id":"<uuid>"}` |
| GET | `/internal/sites/{id}` | Inspect current site state |
| PATCH | `/internal/sites/{id}` | Set desired lifecycle with `{"status":"suspended"}` or `{"status":"ready"}` |

All site responses are direct records:

```json
{
  "id": "01900000-0000-7000-8000-000000000001",
  "project_id": "01900000-0000-7000-8000-000000000002",
  "status": "ready",
  "desired_status": "ready",
  "error_code": null,
  "active_release_id": null,
  "release_generation": 0,
  "created_at": "2026-09-06T00:00:00.000Z",
  "updated_at": "2026-09-06T00:00:00.000Z"
}
```

`status` is `provisioning`, `ready`, `suspended`, or `failed`. `desired_status`
preserves `ready` or `suspended` even while storage is being provisioned/recovered.
In this stage, ready means storage is provisioned; it does not mean there is a
published frontend or executable backend. Suspension blocks new uploads,
activations, content grants, and subsequent content requests. Internal inspection
and source retrieval remain available. Suspension does not stop already loaded
browser JavaScript, abort runs, or remove files.

### Provisioning and retries

The caller chooses a stable site UUID. PUT commits its provisioning intent before
filesystem work and returns **202** while provisioning/failed or **200** for an
already ready/suspended site. Poll GET for completion. UUID spelling is normalized.
Reusing the same site/project returns current state and never creates duplicates,
rewrites application data, or resumes a suspended site. Reusing a site UUID with
another project returns **409 SITE_PROJECT_CONFLICT**. No `Idempotency-Key` is
required: the PUT target and immutable project binding identify the operation.
Responses are current snapshots, not historical receipt replays.

The reconciler wakes after accepted PUTs and otherwise runs every five seconds.
It also runs on startup. Durable metadata is written before directories/files;
the per-site identity is committed before the metadata becomes initialized.
Recovery can finish an intent with no files, partial directories, an empty new
SQLite file, or a fully initialized file awaiting the metadata transition. An
unrelated database at the target path is rejected, not adopted or overwritten.

Failed provisioning reports `SITE_PROVISIONING_FAILED` and is retried after the
underlying problem is fixed. Previously initialized storage reports
`SITE_STORAGE_UNAVAILABLE` when missing or invalid; restore the correct database
and reconciliation restores the desired status. An isolated site failure does
not make the entire service unready or block healthy sites.

PATCH persists the desired state and reconciles it, returning **200** on a healthy
site or **202** if storage is still failed. Repeating the same PATCH is a no-op
when healthy. Concurrent different PATCHes use last accepted write order; after
an uncertain response, inspect state before retrying an older lifecycle command.
PATCH never creates a missing site or changes its project. Responses acknowledge
storage/lifecycle state, not frontend publication or harness execution.

### Validation and operational bounds

- UUID paths and JSON bodies; unknown fields rejected. Only release listing
  accepts query parameters (`after` and `limit`).
- Only `ready`/`suspended` accepted in PATCH; desired `failed` cannot be injected.
- Request bodies capped at 16 KiB, except bundle uploads (24 MiB JSON); up to 64
  concurrent authenticated requests, two uploads/publications, and 32 content
  requests. Filesystem mutations are serialized.
- 400 invalid paths/queries, 401 authentication, 404 missing site, 409 binding
  conflict, 413 body size, 415 content type, 422 invalid JSON shape, 503 unavailable
  storage/capacity. Error bodies use `{"error":{"code":"...","message":"..."}}`.
- Responses include request IDs, `Cache-Control: no-store`, and sanitized errors;
  503 responses also include `Retry-After: 5`.
- Logs contain method, route template, status, duration, and generated request ID;
  not credentials, bodies, or raw database errors.
- SQLite connections have five-second busy/acquisition timeouts. Provisioning
  mutations are serialized and reconciliation reads IDs in pages of 100. This
  foundation targets a small single-owner deployment, not high-volume hosting.

## Source revisions and releases

All routes below are internal and require the API token. `{id}` is a site UUID;
`{bundle}` is a revision or release UUID, scoped to that site.

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/internal/sites/{id}/revisions` | Upload an immutable source revision |
| GET | `/internal/sites/{id}/revisions/{bundle}` | Retrieve revision metadata and file inventory |
| GET | `/internal/sites/{id}/revisions/{bundle}/files/{path}` | Download one source file as an attachment |
| POST | `/internal/sites/{id}/releases` | Upload a complete built release |
| GET | `/internal/sites/{id}/releases?after={uuid}&limit=50` | List releases in UUID order; returns `items` and `next_after` |
| GET | `/internal/sites/{id}/releases/{bundle}` | Retrieve release metadata and manifest |
| PUT | `/internal/sites/{id}/active-release` | Atomically activate a release with a generation check |
| POST | `/internal/sites/{id}/releases/{bundle}/content-access` | Issue a temporary frontend URL |

### Upload contract

A revision body has `id` and `files`; a release adds `manifest`. This release body
is schematic (replace all placeholders with real IDs, bytes, and digests):

```json
{
  "id": "<release UUID>",
  "manifest": {
    "source_revision_id": "<revision UUID>",
    "frontend_entrypoint": "public/index.html",
    "backend_entrypoint": "backend.js",
    "sdk_version": "1"
  },
  "files": [
    {"path": "public/index.html", "content_base64": "<base64 bytes>", "sha256": "<lowercase SHA-256 hex>"},
    {"path": "backend.js", "content_base64": "<base64 bytes>", "sha256": "<lowercase SHA-256 hex>"}
  ]
}
```

- Sources require `backend.ts` and at least one file beneath `frontend/`.
  Other source files such as `package.json` are allowed.
- Releases contain only `public/` assets and `backend.js`. The frontend
  entrypoint must be an existing nonempty UTF-8 `.html` file under `public/`;
  the backend entrypoint must be the existing nonempty UTF-8 `backend.js`.
- The referenced revision must be ready and belong to this site. The reference
  records provenance; the service does not rebuild or prove that output matches
  source. SDK version `"1"` is the only accepted compatibility identifier.
  [Stage 3](BACKEND.md) executes bundled backends and defines optional manifest
  schema ranges and migration references.
- At most 256 files, 4 MiB decoded per file, 16 MiB decoded per bundle, and
  24 MiB for the whole JSON body. Use standard base64 and SHA-256 of decoded bytes.
- Paths are relative, at most 512 ASCII bytes, with components at most 128 bytes
  using letters, digits, `.`, `_`, or `-`. Hidden/dot components, traversal,
  absolute paths, backslashes, reserved Windows device names, duplicate paths,
  inconsistent directory casing, and file/directory collisions are rejected.
  Archives and symlink entries are not accepted.

Successful uploads return **200** with metadata: `format_version`, `site_id`,
`id`, `kind`, `manifest`, `files` (a path-to-size/hash map), `status`, `error_code`,
and `created_at`. A source revision's manifest is null. Uploads never activate
code. Reusing an ID with identical content is safe, including reordered file
arrays. Different content under the same ID returns **409 BUNDLE_IMMUTABLE**.
List pages default to 50, allow 1–100, and may return an empty final page after
a non-null cursor. Listing includes failed and staging records.

### Publication and recovery

Publication records durable intent in `service.sqlite`, writes and syncs files
under `staging/`, writes a completion descriptor, verifies the full inventory and
checksums, then atomically renames the directory into its final location. Only
then does metadata become `ready`. A client disconnect does not cancel an
accepted publication; poll metadata or retry with the same ID and content.

On restart, complete staged directories and final directories awaiting their
metadata transition are recovered. Partial uploads become `failed` with
`BUNDLE_UPLOAD_INCOMPLETE`; resend the identical upload to retry. Retries persist
staging intent again before filesystem work. Failed/incomplete bundles cannot be
activated or served. Published files are checked before activation, and file
reads verify their declared size and checksum. Corrupt published content is not
silently overwritten; restore the correct files or publish a new bundle ID.

### Activation and rollback

Read the site's current `release_generation`, then send:

```http
PUT /internal/sites/<site UUID>/active-release
Authorization: Bearer <SITES_API_TOKEN>
Content-Type: application/json
Idempotency-Key: <unique operation key>

{"release_id":"<release UUID>","expected_generation":0}
```

The operation requires a ready site and a complete release. It atomically updates
`active_release_id`, increments `release_generation`, and stores an activation
receipt. A stale generation returns **409 RELEASE_GENERATION_CONFLICT**. This
also detects intervening A → B → A changes. To roll back, activate an older
release using a new key and the current generation.

Keys are site-scoped, 1–256 visible ASCII characters. Repeating the same key and
body returns the original successful site snapshot without activating again.
Reusing a key with a different body returns **409 IDEMPOTENCY_CONFLICT**. Use
GET site to read the current state after replaying an old receipt.

Activation and rollback never write to `data/site.sqlite`. They change code
selection only; there are no application database migrations in this stage.
Existing release-specific URLs continue to identify their original release.

## Isolated static hosting

The content listener exposes only frontend assets. Configure a dedicated content
origin distinct from both the dashboard and internal API origins. The service
enforces separation from the configured dashboard origin; the deployment must
also keep internal API routes off the content origin. Behind a reverse proxy,
preserve the public content `Host` header: it is validated against
`SITES_CONTENT_ORIGIN`. Forwarded headers are not trusted. Keep the private
storage directory outside all other web roots.

An authenticated Platform caller requests content access with `{}` or
`{"ttl_seconds":900}`. TTL is 30–3600 seconds, default 900. The response contains
`url` and `expires_at` (Unix seconds). The URL includes an HMAC-signed bearer
ticket bound to one site and release; it contains no API token. Any ready release
can receive a URL, including inactive releases for preview. Platform must check
the viewer's project permissions before issuing access.

The dashboard can assign the returned URL to an iframe with
`sandbox="allow-scripts"`. Responses enforce the same sandbox through CSP,
without `allow-same-origin`, and restrict embedding to the configured dashboard.
The iframe has an opaque origin, cannot access dashboard DOM or localStorage,
and cannot use fetch/WebSockets, workers, forms, or external scripts under the
provided policy. Inline scripts and bundled modules/styles/assets are supported.
CORS allows the opaque `null` origin for module/font loads, without credentials.
Backend code, source files, descriptors, and databases are never content routes.

Build assets and imports with **relative URLs** so they retain the ticket and
release path. Root-relative URLs, query strings, CDN dependencies, and SPA
fallback routing are not supported. No frontend `callBackend` bridge exists yet;
that belongs to dashboard/runtime integration in a later step. Future message
handling must validate the iframe window as the source; opaque origins alone do
not identify a site.

Content responses set MIME types, `nosniff`, `Referrer-Policy: no-referrer`, and
`Cache-Control: private, no-cache`. ETags permit conditional requests, but ticket
validity and current site state are checked before a 304 response. The bearer URL
grants its holder access until expiry; avoid logging full content paths at proxies
or analytics layers, and do not put a shared cache in front of this listener.
Service request logs use route templates. There is no individual ticket revocation;
suspension blocks subsequent content requests and rotating the API token invalidates
all existing tickets. Neither expiry nor suspension unloads an already loaded page.
Refresh a grant when reopening a site or when later asset requests need fresh access.

There is no build service, garbage collection, or deployment-wide
storage quota yet. All revisions, releases, and activation receipts are retained.

## Verification

From `platform/`:

```sh
cargo fmt -p platform-sites-service -- --check
cargo clippy -p platform-sites-service --all-targets -- -D warnings
cargo test -p platform-sites-service
```

Tests use temporary directories and local HTTP listeners, not development data or
hosted gateways. They launch the real executable, provision independent sites,
write distinct application data, abruptly stop/restart it, and verify persistence
and lifecycle replay. Additional cases cover concurrency, all provisioning crash
boundaries, storage loss, incorrect identity, unsafe paths, authentication, body
limits, readiness failure, and exclusive volume ownership.

Publication tests cover two releases, source retrieval, immutable retries,
manifest/path/checksum validation, crash recovery, stale/concurrent activation,
rollback without changing application data, and signed content isolation.

For the real-browser smoke check, build the binary, start the fixture, and open
the printed local dashboard URL in Chrome within two minutes:

```sh
cargo build -p platform-sites-service
python3 apps/sites-service/tests/browser_smoke.py
```

The fixture uses temporary service storage and checks CSS, JavaScript modules and
relative imports, opaque iframe origin, and blocked parent DOM/localStorage/fetch
access. It exits with `PASS` only after receiving and validating the browser result.

## Dashboard viewer and demo

See [dashboard Sites integration](../dashboard/SITES.md) for the Sites table,
full-page sandbox viewer, frontend `callBackend` bridge, and the runnable
[harness-browser example](examples/harness-browser/). The shared worker now includes
the [Sites authoring harness](../../packages/harnesses/sites/README.md).

[Direct two-file live authoring and user snapshots](AUTHORING.md) add a simple agent-facing workflow over this storage model.
