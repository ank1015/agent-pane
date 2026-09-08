# Generic code mode

`tool-code-mode` runs one disposable JavaScript cell against an explicit tool
registry. It has no Platform, Sites, provider, database or remote-host dependency.
Any JSON-compatible tool can be adapted: use an object schema for function tools
or a string schema for freeform tools. Names are exact keys, including namespaced
names and names that are not JavaScript identifiers.

The embedding harness supplies three pieces:

| Component | Responsibility |
| --- | --- |
| `Registry` | Names, descriptions, input schemas and read/mutation classification |
| `Dispatcher` | Pure preparation and trusted asynchronous execution of registered calls |
| `Journal` | Durable compare-and-set records, owner fencing and bounded trace pagination |

The engine checks arguments against the trusted schema after preparation. It
commits each call's identity and normalized input **before** dispatch, then saves
the result before delivering it to JavaScript. A custom mutation adapter should
forward `Call.operation_key` to the target's idempotency mechanism. The engine
never retries arbitrary tools automatically; non-idempotent effects require their
own inspection/reconciliation mechanism.

```rust,no_run
use futures_util::future::BoxFuture;
use serde_json::json;
use tool_code_mode::*;

struct Echo;
impl Dispatcher for Echo {
    fn invoke<'a>(&'a self, call: &'a Call)
        -> BoxFuture<'a, std::result::Result<serde_json::Value, ToolError>> {
        Box::pin(async move { Ok(call.input.clone()) })
    }
}
async fn example(journal: &dyn Journal) -> Result<Cell> {
    let mut registry = Registry::default();
    registry.register(Tool {
        name: "echo-freeform".into(), description: "Return supplied text".into(),
        input_schema: json!({"type":"string"}), effect: Effect::Read,
    })?;
    // Persist/reuse this ID in the harness tool-call plan.
    let input = Input {
        id: uuid::Uuid::now_v7(),
        source: "const reply = await tools['echo-freeform']('hello'); text(reply);".into(),
    };
    let (_keep_alive, cancelled) = tokio::sync::watch::channel(false);
    Engine::new("/opt/platform/code-mode-runtime")
        .execute(input, &registry, journal, &Echo, cancelled).await
}
```

Build the guest with `cargo build -p tool-code-mode --bin code-mode-runtime`.
Alternatively, a host binary can handle `--code-mode-runtime` by immediately
calling `tool_code_mode::guest::main()` **before loading configuration or
credentials**. The Platform worker already implements this entry point.
`Registry::code_mode_tool()` supplies model-facing metadata that a harness can
convert to its provider's tool format.

JavaScript receives `tools[name](input)`, `ALL_TOOLS`, `text(value)`, and a frozen
`ctx`. Trusted `Extension` factories can add SDK namespaces such as `ctx.platform`
or a future `ctx.sites`; factories dispatch through the same registry. They cannot
expand the parent's allowlist. SDK factories and tool descriptions must contain
only public data. Credentials and transport configuration stay in the dispatcher.

The guest has no Node APIs, module loader, filesystem/network APIs or environment
access. Each cell gets a fresh QuickJS runtime in a fresh OS-sandboxed process.
macOS uses Seatbelt; Linux requires `bwrap`. Missing or unsupported isolation fails
closed. The process environment is cleared, memory is capped at 64 MiB and stack
at 512 KiB. Parent deadlines and child interruption bound CPU loops; cancellation
kills and reaps the guest. Sites uses this same process-isolation implementation.

Default limits: 30 seconds, 100 nested calls, 64 KiB source, 64 KiB combined
emitted/final output, 64 KiB arguments per call and 128 KiB result per call. The
registry supports 128 tools with definitions bounded to 64 KiB each; complete
bootstrap input is bounded to 2 MiB. Nested calls dispatch **sequentially**, even
when source uses `Promise.all`. Long-running tools should return durable handles.
There are no persistent JS globals or live-cell continuation handles.

## Recovery

Keep the cell UUID stable. Repeating a cell ID returns its saved state; different
source, registry, SDK factories or limits conflict. It never reruns the source.
An unfinished cell from a replaced owner is marked `interrupted`.
`Engine::recover_abandoned` can seal it even if the new harness's SDK or limits
changed. Same-owner active cells are stopped through their cancellation channel.

`Engine::inspect` reads the cell plus a bounded, cursor-based call trace. Each call
has an operation key, sequence, tool, normalized input, status, result and error.
`prepared` means dispatch **may** have occurred: a process can disappear between
an external acceptance and saving its reply. `uncertain` is also not a rejection.
Oversized tool results preserve the call identity and report uncertainty rather
than pretending the effect failed. Previously emitted output and accepted calls
remain in the journal if later JavaScript throws or reaches a limit.

Loss of a journal acknowledgement stops source execution. The engine reads back
and seals the cell when storage is reachable. If storage/ownership remains
unavailable, it returns a storage error; the next owner inspects the durable
records before proceeding. Whole-cell replay is never a recovery strategy.

Use one cell at a time for a leased Platform run. Journal commits advance runtime
versions, so reload context before the harness's next checkpoint/park/finish
commit. A new activation starts a new cell to continue from saved operation
handles. The embedding harness owns that continuation and tool authorization.
