# Read tool

`tool-read` provides the model-facing `read` tool for UTF-8 text files on an
execution host. It exposes `file_path`, optional one-based `offset`, and optional
`limit`. Use `ReadTool::input_schema()` and `description()` when registering it.

The harness creates an `ExecutionClient`, connects a host by ID, and passes the
resulting runtime and working directory to the tool. Neither gateway credentials
nor machine IDs are model arguments. The same implementation works with any
`execution_core::ExecutionRuntime`; production remote calls go through
`execution-client`. There is no dependency on Platform storage or the worker.

```rust,no_run
use execution_client::{ExecutionClient, ExecutionClientConfig};
use execution_core::{ExecutionPath, OperationContext, RootId};
use std::time::Duration;
use tool_read::{ReadConfig, ReadInput, ReadTool};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = ExecutionClient::new(ExecutionClientConfig::new(
    std::env::var("EXECUTION_GATEWAY_URL")?.parse()?,
    std::env::var("EXECUTION_GATEWAY_API_TOKEN")?,
))?;
let context = OperationContext::with_timeout(Duration::from_secs(30));
let host = client.connect_host(
    &context,
    std::env::var("EXECUTION_HOST_ID")?.parse()?,
).await?;
// Use a root ID advertised by host.descriptor() and the environment's cwd.
let cwd = ExecutionPath::new(RootId::new("workspace")?, "my-project")?;
let tool = ReadTool::new(&host, cwd, ReadConfig::default())?;
let output = tool.execute(&context, ReadInput {
    file_path: "src/main.rs".into(), offset: Some(1), limit: Some(100),
}).await?;
let model_text = output.to_text();
let structured_result = serde_json::to_value(&output)?;
# let _ = (model_text, structured_result);
# Ok(())
# }
```

## Results and limits

`ReadOutput` contains the resolved execution path, file revision, original UTF-8
content, start/end lines, EOF, truncation reason, and next line offset. Empty
files succeed with empty content and no end line. No synthetic empty line is
added after a trailing newline. CRLF and final unterminated lines are preserved.
`to_text()` adds cat-style line numbers and an explicit EOF/continuation notice.
Keep `content` free of display prefixes when using it for edits or code mode.

Defaults are 2,000 lines, 50 KiB rendered output, 64 KiB read chunks, and 64 MiB
scanned bytes. All are harness-configurable; caller limits above `max_lines` are
clamped. The output cap includes prefixes and reserves 256 bytes for its notice.
A line is never partially returned. If the first selected line cannot fit,
the tool returns an error directing the caller to inspect it with bash. If later
lines cannot fit, it returns the complete preceding lines and a continuation.
An offset beyond EOF is an error, except offset 1 on an empty file.

The tool scans from byte zero to resolve line offsets, using bounded chunks and
the host's advertised read limit. Selected lines are decoded as UTF-8 after
assembly, including code points crossing chunk boundaries. NUL bytes in scanned
content and invalid UTF-8 in selected lines are rejected. This is not a full-file
encoding check. Directories and non-regular files are rejected. There is no media
conversion or notebook-cell rendering; an ordinary textual `.ipynb` reads as JSON.

Every chunk must match the initial stat's revision and size; a mismatch returns
`RevisionConflict` without partial output. This detects observed changes, but
does not provide a filesystem snapshot or lock between calls. The supervisor may
hash more file data than the tool requests to compute a revision. Small chunks
can therefore cost additional host IO. There is no persistent cursor or cache;
repeated reads observe the current file and pagination may span revisions.

Errors preserve execution-client codes and diagnostics. Tool errors add
`source: "tool-read"`; shared path errors use `source: "tool-filesystem"`.
No retries are automatic. Pass an operation context with a
deadline to bound the entire read; host read requests also retain the client's
per-request timeout. Cancellation is checked between chunks and while scanning.

## Remote paths

Relative paths resolve under the supplied cwd/root. Absolute Unix, Windows drive,
and UNC paths map to the most specific advertised root using component matching.
Paths never resolve on the worker's local filesystem. Windows separators are
converted to the portable execution path; root components use their advertised
spelling to avoid misrouting case-sensitive directories. Drive/share prefixes are
compared without ASCII case sensitivity. Ambiguous equal roots are rejected.

`.` and repeated separators are accepted. `..` segments are rejected because
collapsing them locally can change meaning when a component is a remote symlink;
use an absolute path to another location within a registered root. Windows device
paths, drive-relative paths, and alternate data streams are unsupported. Symlinks
are followed by the execution filesystem, which enforces root containment.
Registered roots are filesystem access scopes, not shell process sandboxes.

## Verification

From `platform/`, run `cargo test -p tool-read` and
`cargo clippy -p tool-read --all-targets -- -D warnings`.
Tests connect the real `ExecutionClient` to a local authenticated HTTP fixture
dispatching to an actual supervisor in a temporary directory. They also inject
revision changes and cancellation to exercise read failures. No hosted machine
or database is required.
