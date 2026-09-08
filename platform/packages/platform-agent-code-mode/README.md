# Platform agent code mode

The reusable [code-mode engine](../tools/code-mode/README.md) remains independent
of Platform. This package adds:

- `RunJournal`: private session-state records in `code_mode.<source run UUID>`,
  using existing transactional commits, version checks and lease fencing.
- `PlatformDispatcher`: the leased capability transport; credentials stay in
  `RunClient`. Mutations receive a durable default key if the helper omitted one.
- `register_platform` and `platform_extension`: the common registry and
  `ctx.platform` factory from [platform-javascript-sdk](../platform-javascript-sdk).
- `AgentCodeMode`: a ready-to-use combination with worker signal cancellation,
  inspection, abandoned-cell recovery and explicit individual-call reconciliation.

```rust,no_run
use platform_agent_code_mode::{AgentCodeMode, code_mode::{Input, Cell, Result}};
use platform_runtime_client::RunClient;
use harness_runtime::Signals;
use tokio::sync::watch;

async fn execute(
    client: RunClient,
    persisted: Input,
    signals: watch::Receiver<Signals>,
) -> Result<Cell> {
    // The worker handles --code-mode-runtime before loading credentials.
    let binary = std::env::current_exe().map_err(|_| platform_agent_code_mode::code_mode::Error::Invalid("Missing worker executable"))?;
    AgentCodeMode::new(client, binary)?.execute(persisted, signals).await
}
```

Cell example:

```js
const environments = await ctx.platform.environments.list({limit: 20});
text(environments.items.map(e => ({id: e.id, name: e.name, type: e.type})));
const job = await ctx.platform.execution.bash({
  hostId: selectedHostId,
  workdir: selectedAbsoluteDirectory,
  command: 'run-verifier',
});
return {executionId: job.id};
```

The harness supplies those selected IDs/paths in its cell source; they are not
extra globals. The same helper can execute in a site handler if it supplies
`{idempotencyKey}` explicitly. Shared TypeScript uses `Platform` for that common
surface and `AgentPlatform` for the automatic-key convenience. An explicit key is
preserved; intentionally repeated keys retain normal Platform receipt semantics.
The trace's unique `operation_key` and the actual key in `input.options` are both
retained.

To add another harness's tools, construct a `Registry`, register the desired
definitions (optionally including `register_platform`), and supply a dispatcher
that routes the exact registered names. Reuse `RunJournal` and `Engine`; add
trusted SDK extensions only if useful. No generic runtime change is required.

## Interruption and continuations

Abort, draining, loss of ownership and a closed worker signal channel stop the
cell. An accepted run, sandbox or command keeps its own lifecycle. Killing the
JavaScript process does not claim to undo an accepted effect.

After takeover, call `recover_abandoned(cellId)` or execute the same saved input
to obtain the interrupted state. `inspect(cellId, cursor, limit)` reads bounded
traces. `reconcile_call(cellId, sequence)` operates only on an interrupted/failed
cell's saved Platform mutation. It resends the **exact saved arguments/key** to
the existing receipt-aware endpoint; if the first request never arrived, this
can submit it for the first time. It never reruns JavaScript. Calls already saved
as succeeded or rejected return their records without dispatch.

Inspection and reconciliation require the source run's current lease. The
private journal survives in session state; accepted resources also remain
available through normal project-scoped getters. Journal writes emit small
`code_mode.journal` event references atomically with the private records. Source,
arguments and results are not copied into public event payloads.

For long work, return operation handles from the cell, persist them with the
harness checkpoint, and use existing `Commit.waits`/`Disposition::Waiting`:
`Dependency::RunCompletion` for runs, or `Dependency::Timer` for polling remote
commands/sandboxes. Reload context after the cell before constructing the commit.
When the worker is activated again, inspect status in a new cell. Site backends
instead use short invocations, saved handles and existing run callbacks. Neither
context persists a suspended JavaScript stack or automatically replays source.

Use `AgentCodeMode::new(client, binary)?.with_sites()?` for both `ctx.platform` and `ctx.sites`. Sites methods are authorized only for the Sites harness and resolve a durable session/site binding on the server. Explicit call reconciliation also supports Sites edit, invocation and SQL receipts; it never replays interrupted backend source.
