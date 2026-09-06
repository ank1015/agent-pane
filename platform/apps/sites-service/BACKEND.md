# Site backend execution (Stages 3–5)

A release's `backend.js` is a bundled UTF-8 JavaScript ES module exporting one
handler. Source revisions still contain `backend.ts`; compilation/bundling happens
in the development workspace, not on the service. See [SDK types](src/sdk.d.ts).

```js
export default async function handle(request, ctx) {
  if (request.method === 'POST' && request.path === '/notes') {
    await ctx.db.execute('INSERT INTO notes(id, text) VALUES (?, ?)',
      [request.body.id, request.body.text]);
    ctx.log.info('Note saved', { id: request.body.id });
    return { status: 201, body: { id: request.body.id } };
  }
  return { status: 200, body: await ctx.db.query(
    'SELECT id, text FROM notes ORDER BY id LIMIT ?', [50]) };
}
```

## Runtime and isolation

This crate requires Rust **1.87+** for pinned `rquickjs` 0.12.2. Each invocation
runs in a fresh QuickJS process with a 64 MiB JS heap and 512 KiB JS stack limit.
There is no Node.js runtime, package loader, `process`, `require`, `fetch`, file
access, timers, subprocess access, or direct network access. Platform SDK calls use the trusted parent bridge. Bundle pure-JavaScript
libraries into the single release file. Unresolved promises with no runnable
microtasks fail; this is a bounded request handler, not a background event loop.

The parent starts the same executable in `--backend-runtime` mode **before**
dotenv, logging, storage, or Tokio initialization. The child has a cleared
environment and inherits only its IPC pipes. SQLite and SDK authorization remain
in the trusted parent. Child output and SDK requests are treated as untrusted.

OS sandbox support:

- macOS: `/usr/bin/sandbox-exec`, default-deny policy. Allow only executable/system
  library reads, root-directory read needed by dyld, and limited system runtime
  operations. No private volume access, file writes, or networking. A compiled
  native probe verifies those restrictions in the macOS test suite.
- Linux: install **bubblewrap** (`bwrap`) and enable the user namespaces it needs.
  The child runs with unshared namespaces/network, read-only system libraries and
  executable, and a temporary filesystem. No home directory or sites volume is
  mounted. Container environments must permit bubblewrap's namespaces.
- Other operating systems: backend execution fails closed. Missing/broken OS
  sandboxing never falls back to an unrestricted process. Provisioning/static
  serving remain usable; verify a backend invocation as a deployment smoke test.

The macOS path is covered by the local integration suite. Linux requires the same
suite on a Linux host with bubblewrap; a successful macOS check does not verify
Linux namespace availability. Process and language isolation do not constitute a
VM boundary or guarantee protection against every engine/kernel vulnerability.

## Invocation API

All endpoints use the existing internal bearer authentication, never browser
credentials. Platform/project authorization and forwarding are documented in [Stage 4](../server/SITES.md).

```http
POST /internal/sites/{site_id}/invocations
Content-Type: application/json
Authorization: Bearer <SITES_API_TOKEN>
```

```json
{
  "id": "<stable invocation UUID>",
  "release_id": "<release UUID>",
  "timeout_ms": 10000,
  "request": {
    "method": "POST",
    "path": "/notes",
    "query": {},
    "body": { "id": "note-1", "text": "Hello" }
  }
}
```

`id` is required. `release_id` may be omitted/null to select the active release
when first accepted. The resolved release is frozen on the invocation. Defaults:
`timeout_ms=10000`, `query={}`, `body=null`. Paths are application routes beginning
with `/`; query fields are supplied separately. The project-facing API accepts no
caller-controlled source, project, database path, code, or credentials. The trusted
Platform dispatcher alone adds callback context to internal delivery requests.

The HTTP response is **200 with an invocation record**, including its status and
nested `{status, body}` backend response, even if backend execution failed. It
must not be confused with an application HTTP success. Backend status must be
200–599; body must be JSON. Streaming, binary responses, and custom headers are
not supported in v1.

```json
{
  "id": "...", "site_id": "...", "release_id": "...",
  "status": "succeeded", "response": { "status": 201, "body": {} },
  "error_code": null, "logs": [], "created_at": "...", "finished_at": "..."
}
```

Statuses: `running`, `succeeded`, `failed`, `timed_out`, `interrupted`.
`GET /internal/sites/{site_id}/invocations/{id}` retrieves a record.
`GET /internal/sites/{site_id}/invocations` returns the latest 100 records;
this diagnostic view is bounded, not a complete paginated history API.

Reusing the same site/invocation UUID with an identical normalized body returns
its saved record; a changed body returns `409 IDEMPOTENCY_CONFLICT`. Terminal
failures are also retained. Replays resolve before current lifecycle/schema
checks, so they do not rerun code after publication or suspension. Receipts have
no automatic expiry. Persist the exact request across ambiguous HTTP outcomes.

A disconnected HTTP caller does not cancel accepted execution. Graceful shutdown
drains admitted work. On abrupt service restart, unfinished invocations become
`interrupted` with `SERVICE_RESTARTED`; they are never automatically rerun.
Standalone committed writes may have happened before failure. An invocation is
not a transaction around all its effects. Retrying an interrupted logical action
with a new invocation UUID requires application-level deduplication/reconciliation.
Logs are saved with the terminal outcome; abrupt process loss may lose in-flight
logs. Raw JS exceptions and host errors are not placed in HTTP/service logs;
error codes are recorded. Use explicit bounded `ctx.log` entries for diagnostics.

## SDK

`ctx.site`: frozen `id`, `projectId`, `releaseId`, `sdkVersion`.
`ctx.invocation`: frozen `id`, `source` (`"internal"` or `"callback"`), nullable
`eventId`/`subscriptionId`, Unix-ms `deadlineAt`, and
`throwIfCancelled()`. The host enforces its own scope and monotonic deadline.
There is no external cancellation endpoint. Platform completion delivery supplies trusted callback context; see [completion callbacks](../server/CALLBACKS.md).

Database methods are promises; always await them:

- `query(sql, params=[])`: one read-only statement, returning object rows.
- `execute(sql, params=[])`: one statement, returning `{changes, rows}`. Supports
  DML `RETURNING`; read-only statements return `changes: 0`.
- `batch([{sql, params?}, ...])`: 1–100 statements in one transaction, returning
  write-result objects. Any error or oversized aggregate result rolls back all.
- `transaction(async tx => ...)`: one short read/decide/write transaction. `tx`
  exposes `query` and `execute`. Resolve commits; throw rolls back. Nested
  transactions, use of the outer database handle while inside a transaction, and
  reusing an expired transaction handle are rejected. Two-second transaction
  deadlines are checked at operations/commit; the invocation deadline terminates
  code that stops cooperating. Abandoned open transactions are rolled back.

Each standalone statement uses a host-owned savepoint so rejected/oversized
`RETURNING` results do not leave behind their writes. Earlier successful
statements outside a transaction remain committed after a later failure.

Positional parameters support `null`, strings, and finite numbers within the JS
safe-integer magnitude. Booleans, objects, arrays, undefined, and binary values
are rejected. Encode JSON as TEXT. Large integer or BLOB result columns are
rejected rather than rounded; explicitly select `CAST(value AS TEXT)` or `hex`.
Use unique column aliases. Result limits are errors, never silent pagination.

SQLite authorization denies internal tables (`__sites_*`, SQLite catalogs),
ATTACH/DETACH, extensions, PRAGMAs, arbitrary transaction control, virtual tables,
and runtime DDL. Reads through views/triggers are subject to the same checks.
Parameterized SQL is still required; authorization is enforced independently.

`ctx.log.info|warn|error(message, data?)`: structured logs scoped by the invocation
record. At most 100 entries, 4 KiB per entry, and 32 KiB total. Avoid secrets or
sensitive application data. `ctx.platform` provides the session-oriented [Stage 4 SDK](../server/SITES.md); durable [completion callbacks](../server/CALLBACKS.md) resume work after runs finish.

## Limits and concurrency

- Eight admitted execution/storage operations across the service.
- One invocation or schema/storage-management operation at a time per site.
  This initial serialization also prevents migrations racing active SQL handles.
- Per-site lock acquisition: five seconds, then retryable capacity error.
- Request: 256 KiB; response: 256 KiB; IPC frame: 512 KiB; 1,000 SDK messages.
- Invocation: configurable per request, 100–30,000 ms; default 10,000 ms.
- Query: one-second progress deadline, constrained by transaction/invocation
  deadlines; 250 ms SQLite busy timeout.
- SQL: 64 KiB, 256 parameters, 100 columns, 1,000 result rows, 256 KiB result.
- Site SQLite growth: 64 MiB (existing larger files are not shrunk).

The private service database, backups, source/releases, and retained invocation
history also consume disk. Their retention is operator-managed in this stage;
monitor the dedicated volume. A site failure does not stop unrelated sites.

## Migrations and schema compatibility

Migrations are source files referenced by the release manifest; they are not
runtime SDK methods or extra files in the release's `public/` directory.

```json
{
  "source_revision_id": "...",
  "frontend_entrypoint": "public/index.html",
  "backend_entrypoint": "backend.js",
  "sdk_version": "1",
  "schema": { "min": 1, "max": 2 },
  "migrations": [{ "version": 1, "path": "migrations/001_notes.sql" }]
}
```

Include the complete ordered migration history starting at 1 (maximum 100 files).
Each file must exist in the referenced source revision. SQL is at most 64 KiB per
file and 1 MiB total. The schema range must contain the declared final version.
Omitted fields preserve existing releases as exact schema version 0 and serialize
unchanged, preserving their stored fingerprints.

1. Upload source and release while the site is ready.
2. Suspend the site. Suspension waits for any admitted site invocation to finish.
3. `POST /internal/sites/{id}/schema/migrations` with `{ "release_id": "..." }`.
4. Inspect `GET /internal/sites/{id}/schema`, then resume the site and activate
   the compatible release using the existing activation API.

Migration application is forward-only and atomic across all pending files. The
protected `__sites_migrations` ledger is updated in the same SQLite transaction.
Identical versions/checksums replay without executing SQL; changed history is a
conflict. Failures roll back DDL, data, and ledger. SQL may define application
tables, indexes, views, and triggers but cannot modify reserved objects, attach
other databases, configure SQLite, load extensions, or control transactions.

Both activation and invocation check the actual schema version and applied
migration checksums. Explicitly pinning an older release does not bypass those
checks. Compatible older releases can execute; incompatible ones return
`409 SCHEMA_INCOMPATIBLE`. Code rollback never downgrades the database. Use
additive migrations and overlap compatible ranges where older code must remain
usable. Resuming before activation may briefly expose the old frontend; its
incompatible backend invocations are rejected rather than run against wrong data.

## Backups

`PUT /internal/sites/{id}/backups/{backup_id}` creates an immutable consistent
snapshot using SQLite's online backup API. The caller supplies a stable UUID;
retrying it returns the original snapshot, not a newer copy. The route returns
`{id, site_id, size}`. It is available for healthy ready or suspended sites.

`GET /internal/sites/{id}/backups/{backup_id}` streams that snapshot to the trusted
caller. Files live under `sites/<id>/backups/`; partial files aren't downloadable.
The service creates private files and publishes completed snapshots by rename.
Each backup includes application data, identity, and the migration ledger.

Store downloaded backups off the volume. These snapshots do not include
`service.sqlite`, source, releases, or invocation history; those still need
separate service-wide backup. No live restore API is provided: an operator can
restore the matching site's snapshot while the service is stopped, replacing its
SQLite database and removing stale WAL/SHM sidecars, then restart and verify its
identity/schema/release compatibility. Never restore another site's identity.

## Verification

```sh
# From platform/
cargo fmt -p platform-sites-service -- --check
cargo clippy -p platform-sites-service --all-targets -- -D warnings
cargo test -p platform-sites-service
```

Backend integration tests use isolated temporary volumes and the real executable,
including the OS-sandboxed child. They do not access hosted gateways or development
data. Existing foundation/publication tests cover earlier-stage behavior.
