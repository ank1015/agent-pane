# Edit tool

`tool-edit` exposes `edit` with `file_path`, `old_string`, `new_string`, and
optional `replace_all` (default false). Use `NAME`, `DESCRIPTION`, and
`input_schema()` to register it. Matching is exact, including whitespace, Unicode,
and LF/CRLF. There is no fuzzy matching, indentation correction, escape handling,
or line-ending conversion. Replacement strings are literal: `$1`, backslashes,
and dollar signs are written unchanged. Empty replacements delete matched text.
Empty searches, identical strings, missing matches, and ambiguous single edits
fail. Single-edit ambiguity includes overlapping matches; replace-all replaces
every non-overlapping occurrence.

The harness supplies the connected execution runtime, cwd, operation ID, and
an `ObservedFile` obtained from read or a successful mutation. Host/root/path
must match. Preparation reads the whole file in bounded chunks and validates its
revision throughout, then builds the replacement. Apply reuses `tool-write` and
the supervisor's `MatchRevision` write. A change between preparation and apply
fails rather than being overwritten. Only existing regular UTF-8 files are
supported; NUL bytes in the original file are rejected. Unchanged bytes, BOM,
indentation, and line endings are preserved. No formatter runs.

## Harness integration and recovery

Use `prepare` followed by `apply` for one model tool call. There are two Rust
steps so the harness can persist the exact write before dispatching it.

```rust,no_run
use execution_client::ExecutionClient;
use execution_core::{ExecutionHostId, ExecutionPath, OperationContext, OperationId};
use tool_edit::{EditConfig, EditInput, EditState, EditTool, ObservedFile, PreparedEdit};

# async fn example(client: ExecutionClient, host_id: ExecutionHostId,
# cwd: ExecutionPath, observed: ObservedFile) -> Result<(), Box<dyn std::error::Error>> {
let context = OperationContext::with_timeout(std::time::Duration::from_secs(30));
let host = client.connect_host(&context, host_id).await?;
let tool = EditTool::new(&host, cwd, EditConfig::default())?;
let prepared = tool.prepare(&context, EditInput {
    file_path: "src/main.rs".into(),
    old_string: "let retries = 2;".into(),
    new_string: "let retries = 3;".into(),
    replace_all: false,
}, EditState { operation_id: OperationId::generate(), observed }).await?;

// Persist this payload before apply, using storage suitable for its size.
let saved = serde_json::to_vec(&prepared)?;
let restored: PreparedEdit = serde_json::from_slice(&saved)?;
let result = tool.apply(&context, &restored).await?;
let next_observation = result.observation();
let model_text = result.to_text();
# let _ = (next_observation, model_text);
# Ok(())
# }
```

Resolve the model path using `resolve_path()` when looking up its observation.
`prepare` never mutates files. The prepared plan contains the resolved host/path,
original revision, operation ID, replacement count, and complete resulting file.
On an uncertain apply outcome, replay the same saved plan. Do not prepare again:
the first write may have succeeded, so the original text may no longer exist.
Replay does not re-read the file or use a changed cwd. A successful result can be
saved with the tool message/checkpoint, and its observation used for future edits.

Persistence remains harness-owned. Prepared payloads can be much larger than a
session-state value; respect the Platform storage/request limits or store large
plans in suitable durable storage and persist a reference. A prepared plan is
trusted harness state, not another model-facing argument. Its `Debug` output
omits file contents.

The gateway can deduplicate the original write while its operation receipt is
retained. Supervisor restarts can lose receipts; reconcile ambiguous effects
before issuing a new operation ID. No automatic retries or exactly-once claim is
made. Cancellation/deadlines can follow a completed remote write; an acknowledged
success is returned without a post-write cancellation check.

## Limits and results

The original/resulting file size limit defaults to 16 MiB; reads default to 1 MiB
chunks and obey host read limits. Result size also obeys the host write limit.
UTF-8 code points may span read chunks. Growth is checked before allocating the
replacement buffer. Context cancellation is checked during reading and matching;
individual substring searches are synchronous and bounded by the file size cap.
The supervisor may read/hash the whole file for every chunk to supply revisions.
Revision checks detect observed changes, but do not provide a filesystem snapshot
or locking against external programs. Remote HTTP limits also apply.

Structured results contain host, path, replacement count, bytes written, and new
revision. `to_text()` reports a short confirmation without echoing the file.
Root containment, symlinks, and remote path conventions are shared with read/write.
Local edit errors use `source: "tool-edit"`; execution and write/path errors retain
their codes and diagnostics. Missing files are not created and there is no
unconditional-write fallback.

From `platform/`, run `cargo test -p tool-edit` and
`cargo clippy -p tool-edit --all-targets -- -D warnings`. Tests use the real
execution client against a local HTTP fixture and supervisor with temporary
files, including revision changes, replay, and malformed read replies.
