# Direct live authoring

A Sites agent edits `index.html` and `backend.js` directly on the main site. It
needs no development machine, source checkout, package installation, build step,
working copy, or publication tool. The frontend contains HTML/CSS/browser JS;
the backend is a self-contained JavaScript module exporting a default handler.
The existing `callBackend` viewer bridge and backend `ctx.db`/`ctx.platform` SDKs
remain available.

Multiple sessions can edit the same site. Each session binds once to its configured
`siteId`; null creates a new project site when authoring is first used. Binding is
stored outside immutable harness configuration and survives subsequent runs and
worker replacement. The server accepts this capability only from a current
`sites` harness lease, with the harness enabled for the project and project Sites
access enabled. A site-backend caller has no authoring authority.

## Agent API

Opt into `AgentCodeMode::with_sites()` to add the generated `ctx.sites` namespace:

```js
const source = await ctx.sites.read();
const saved = await ctx.sites.applyPatch({patch: `*** Begin Patch
*** Update File: index.html
@@
-<h1>Sites</h1>
+<h1>Project results</h1>
*** End Patch`});
text(saved);
```

`applyPatch` uses the existing `tool-apply-patch` parser and text update logic.
It accepts updates to those exact two paths, without moves, additions, deletions,
or environment selectors. It has no `expectedVersion` input. Patch context is
matched against the current files. A changed context fails without modifying
anything. Save preparation captures the site's generation internally; an
intervening activation/rollback conflicts rather than overwriting that change.
This does not prevent a subsequently submitted matching patch from editing the
new current site. Sessions have no exclusive editing lock.

Limits are 48 KiB per file, 48 KiB patch text, 64 hunks, and 96 KiB serialized file
contents. Read returns starter files if no code has been activated yet. Existing
sites with extra assets or different entrypoints are rejected by the two-file
editor rather than silently dropping files. Existing multi-file APIs remain valid.

The backend module is checked in a fresh OS-sandboxed process without host APIs,
database access or Platform credentials. Validation checks module initialization
and a callable default export; it does not run the handler or prove application
correctness. Validated frontend/backend files activate together using the existing
recoverable bundle storage. There are no user-visible automatic snapshots. Existing declared migration files
and schema ranges are preserved internally when editing a compatible two-file site.

Other methods:

- `operation(id)`: inspect an edit's saved outcome.
- `preview()`: temporary frontend content access. Backend calls require the
  authenticated dashboard viewer's MessageChannel bridge.
- `invoke(request)` / `invocation(id)`: exercise the backend and inspect results.
  Large responses/logs are explicitly marked as truncated by the agent adapter.
- `logs()`: the latest 20 invocation summaries; inspect individual IDs for details.
- `query({sql, params})`: bounded read-only SQL; limit results to 96 KiB.
- `execute({sql, params})`: one data or schema statement. SQLite authorization
  prevents host/file access and access to internal records. Writes and their
  immutable retry receipt share one database transaction. Use query for results;
  execute does not support RETURNING. Existing backend SQL rules are unchanged.

Every mutation accepts optional `options.idempotencyKey`; the code-mode adapter
assigns one if omitted and journals it before dispatch. The source run and key
resolve to one durable downstream operation UUID. Recovery resends those exact
arguments. Pending edits retain their prepared files and base generation across
restart. Whole JavaScript cells and interrupted backend handlers are never replayed
automatically. An edit result with `status: conflict` requires a fresh read and a
new patch operation. Accepted work can finish after a request disconnect or lease
loss, while new requests always require current authority.

## User snapshot API

All project routes use the existing project Sites authentication and verify site
membership. They are not exposed in the agent or deployed backend SDK.

| Method | Project route suffix | Body |
| --- | --- | --- |
| GET | `/source` | — |
| PATCH | `/source` | `{id, patch}` |
| GET | `/authoring/{operation}` | — |
| POST | `/snapshots` | `{id, name}` |
| GET | `/snapshots?limit=20&after=…` | — |
| POST | `/snapshots/{snapshot}/restore` | `{id}` |

Prefix: `/api/projects/{project}/sites/{site}`. IDs are caller-persisted operation
UUIDs, reused for retry. A snapshot names the active immutable code pair. Restore
activates that pair atomically, with existing release schema checks. Snapshot
listing is bounded, with `nextAfter` pagination.

Snapshots and restores affect **code only**. They do not copy, reset, or restore
SQLite data and do not undo Platform operations. Authoring SQL changes to the
application schema can make older code incompatible; restore does not infer or
run reverse migrations. Existing versioned migration, suspension and backup APIs
remain available for managed schema transitions. Older releases remain retained
so in-flight requests and release-pinned completion callbacks can finish.

The dashboard snapshot controls and automatic viewer refresh belong to Step 7;
Step 5 provides their authenticated API. Step 6 adds the registered
[Sites harness](../../packages/harnesses/sites/README.md), its full system prompt,
code-mode loop, durable waits and operation recovery. No basic harness menus are changed.

## Validation

From `platform/`:

```sh
cargo build -p platform-sites-service -p tool-code-mode --bins
cargo test -p platform-sites-service -- --test-threads=1
DATABASE_URL=postgresql:///postgres cargo test -p platform-server --test sites -- --include-ignored --test-threads=1
cargo test -p platform-javascript-sdk -p platform-agent-code-mode -p tool-apply-patch
```

The new service migration stores edit preparation and named code snapshots. The
new server migration stores session/site bindings and leased mutation admission.
Both run through the existing migration pipeline.
