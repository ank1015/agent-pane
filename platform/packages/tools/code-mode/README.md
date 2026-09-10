# Generic code mode

`tool-code-mode::live::Session` is the general exec/wait runtime. It has **no
Codex dependency**: no Codex crate, executable, service, configuration, or checkout
is needed at build or run time. Its implementation uses this package's existing
QuickJS dependency and OS-sandboxed child processes. Sites uses this live runtime.

## Outer protocol

Expose only these two tools. Provider-neutral grammar, descriptions, argument
parsing, and result types are in `live::protocol`; Sites' `src/tools.rs` shows
the conversion to the LLM contract. Tool results are content items, not a JSON
object containing a script's return value.

| Tool | Input | Result |
| --- | --- | --- |
| `exec` | Raw JavaScript async module, with optional first-line `// @exec: {"yield_time_ms":10000,"max_output_tokens":1000}` | Status header plus emitted content; running results include the cell ID |
| `wait` | `{cell_id: string, yield_time_ms?: number, max_tokens?: number, terminate?: boolean}`; no extra properties | Only new content, or final completion/failure/termination; collecting terminal state closes the cell |

Both yield time and output budget default to 10,000. Wait is a JSON function;
exec is a custom freeform tool using `EXEC_GRAMMAR`, not `{source: ...}` JSON.
Use `ExecInput::parse` before `Session::exec`. An exec/wait `Report::render()`
returns `Vec<ContentItem>` with this serialized union:

```ts
type ContentItem =
  | {type: "input_text"; text: string}
  | {type: "input_image"; image_url: string; detail?: string}
  | {type: "input_audio"; audio_url: string};
```

The first item is `Script running with cell ID <id>`, `Script completed`,
`Script failed`, or `Script terminated`, followed by
`\nWall time <seconds to one decimal> seconds\nOutput:\n`.
Errors append `Script error:\n<message>` before output truncation.
`Report::success()` supplies tool-success metadata separately.

## Registering arbitrary tools

Use the existing `Registry` and `Dispatcher` traits, with no Platform adapter
required. Object-schema tools receive objects; string-schema tools receive raw
strings. A dispatcher returns arbitrary JSON-compatible values, which remain in
JavaScript unless emitted. Nested inputs are schema-validated after preparation.

```rust,ignore
let mut registry = Registry::default();
registry.register(Tool {
    name: "my.echo".into(), description: "Echo text".into(),
    input_schema: serde_json::json!({"type":"string"}), effect: Effect::Read,
})?;
let (mut session, mut notifications) = live::Session::new(
    "/opt/platform/code-mode-runtime", registry, std::sync::Arc::new(MyDispatcher), vec![],
)?;
let report = session.exec("outer-call-id".into(), live::protocol::ExecInput::parse(
    "text(await tools.my_echo('hello'));"
)?).await?;
```

Keep the session and drain its bounded notification receiver while executing
and between tool calls. Notifications contain `{call_id, cell_id, text}`; the
embedding harness appends each as an additional output for the original exec
call. Supply tool schemas/descriptions to the model as part of its instructions.
`ALL_TOOLS` contains only `{name, description}`. Names are normalized to ASCII
JavaScript identifiers; normalized collisions and `exec`/`wait` are rejected.
Optional trusted `Extension` factories can provide SDK namespaces under `ctx`.

Each cell is a fresh async module with top-level await, not a REPL or function.
Helpers are `text`, `image`, `audio`, `generatedImage`, `store`, `load`, `notify`,
`setTimeout`, `clearTimeout`, `yield_control`, and `exit`. No Node, console,
filesystem, network API, or module loader is installed. Image/audio helpers
accept inline base64 data URLs or MCP content blocks. Ordinary globals do not
persist; explicit JSON store/load values are shared live across session cells.
Promise.all dispatches concurrently. Cells continue after timed yields, and an
awaited yield_control creates an output boundary. When the module completes,
unawaited timers and tool futures are discarded. Dropping a session cancels cells.
External effects already accepted by a tool can survive cancellation.

Build `code-mode-runtime`, or handle `--live-code-mode-runtime` in your host
binary by calling `live_guest::main()` before loading configuration/credentials.
The worker handles both the live and legacy guest entry points.

### Compatibility and lifecycle limits

The exec/wait input contracts and content/status shapes follow the reference
protocol; this is **not a V8 implementation** or a claim of complete engine
equivalence. QuickJS language details, resource limits, and conservative
data-URL-size audio budgeting can differ. Text uses UTF-8-safe head/tail
truncation at approximately four bytes per token. Images retain their order.

The embedding host owns session lifetime. In Sites it lasts for one active run
activation: completion, crash, drain, or worker replacement ends live cells and
their in-memory store. There is no automatic source replay or resumable process
snapshot. Sites checkpoints admission before dispatch and reports interruption
after recovery. Keep durable state/operation handles in downstream services and
use explicit idempotency keys for mutations that may need retrying.

Live limits: 32 uncollected cells, 10,000 calls per cell, 1,024 pending operations,
256 KiB per bridge frame and per uncollected output batch (at most 1,000 items),
128 KiB session store, 64 KiB arguments, 128 KiB nested results, and 2 MiB bootstrap.
Existing sandbox heap/stack and registry limits below also apply. There is no
fixed execution timeout; the host must collect or cancel live work.

## Legacy journaled engine

The original `Engine` API below remains for existing callers. It is separate
from `live::Session` and is **not** the outer tool interface used by Sites.

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
