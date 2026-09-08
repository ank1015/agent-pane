# Sites harness

Trusted Rust harness `sites` runs in the shared worker. It owns no development
machine: the agent edits the bound site's live `index.html` and `backend.js`
through the scoped authoring SDK. `siteId` is an existing project site or null;
null creates a site only when authoring is first accessed. Research and project
orchestration need not create a site. Many sessions can edit one site. Snapshots
and rollback remain user operations; database changes are live and independent.

The model sees `code_mode`, `inspect_cell`, `reconcile_call`, and `wait`.
JavaScript receives `ctx.platform`, `ctx.sites`, tool metadata and optional web
research/browser verification tools. It has no worker credentials, local files,
network or persistent JavaScript globals. The full prompt and generated SDK
reference are frozen in each run's checkpoint. No history compaction is applied.

Cell plans and LLM operation identities are checkpointed before dispatch. Cells
are never replayed on recovery. Each accepted nested mutation has a durable
receipt; the model explicitly inspects/reconciles uncertain calls. Model job
submission uses the same operation key after an ambiguous response. Worker drain
interrupts cells and releases the activation; abort also aborts the model job.
Neither implicitly aborts separately created runs, sandboxes or remote commands.

`wait` atomically commits its tool result and a run-completion/timer dependency,
then releases the worker slot. It wakes on any named run, its bounded timer, or
incoming input. The next activation reads current status. Commands use timer
polling. If authoring reports a site ID, a successful run publishes an immutable
`site` JSON output with that identity, not a frozen version or an access grant.

Enable with `SITES_ENABLED=true` and LLM gateway configuration. Sites is a
Platform-owned harness and is available to every project while globally enabled.
Sites-only workers do not require execution-gateway credentials. Project Site
access and method grants are enforced by the existing leased transport.
`FIRECRAWL_API_KEY` enables search and scrape. Without it those tools are absent.
Optional browser setup is described in `browser/README.md`.

Run `cargo test -p sites-harness`. Postgres integration tests live alongside
Sites server fixtures and require built `platform-sites-service` and
`code-mode-runtime` binaries:

```sh
DATABASE_URL=postgresql:///postgres SITES_TEST_BROWSER_NODE=/absolute/path/to/node cargo test -p platform-server --test sites sites_harness:: -- --include-ignored --test-threads=1
```

The browser integration case also needs the optional browser npm setup.
