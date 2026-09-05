# Write tool

`tool-write` exposes `write` with required `file_path` and `content` strings.
Use `NAME`, `DESCRIPTION`, and `input_schema()` when registering it. It writes
the complete UTF-8 string exactly, including CRLF, NUL, or an empty string,
and creates missing parent directories. There is no append mode or added newline.

The harness supplies a runtime obtained from `ExecutionClient::connect_host`,
the environment's working directory, and `WriteState`. The state contains an
operation ID and an optional observed revision bound to the host and execution
path. A previous `ReadOutput` provides the path and revision; take the host ID
from that read's runtime. A successful `WriteOutput::observation()` supplies the
next observation directly. Store observations under host ID, root ID, and path.

No observation means `MustNotExist`. An existing target then fails with a
read-first message. An observation means `MatchRevision`: a changed or deleted
file fails with `RevisionConflict`. The tool never substitutes a fresh stat's
revision or sends an unconditional write. These conditions are checked by the
supervisor as part of its atomic file replacement; external programs can still
race filesystem operations outside the supervisor's locking discipline.

```rust,no_run
use execution_client::ExecutionClient;
use execution_core::{ExecutionHostId, ExecutionPath, OperationContext, OperationId};
use tool_write::{WriteConfig, WriteInput, WriteState, WriteTool};

# async fn example(client: ExecutionClient, host_id: ExecutionHostId, cwd: ExecutionPath)
# -> Result<(), Box<dyn std::error::Error>> {
let context = OperationContext::with_timeout(std::time::Duration::from_secs(30));
let host = client.connect_host(&context, host_id).await?;
let tool = WriteTool::new(&host, cwd, WriteConfig::default())?;
let input = WriteInput {
    file_path: "src/new_file.rs".into(),
    content: "fn main() {}\n".into(),
};
let state = WriteState { operation_id: OperationId::generate(), observed: None };
// Persist input, state, and host/cwd identity before a recovery-critical effect.
let result = tool.execute(&context, input, state).await?;
let next_observation = result.observation();
let model_text = result.to_text();
# let _ = (next_observation, model_text);
# Ok(())
# }
```

For an overwrite, resolve the model path with `tool.resolve_path(...)`, look up
its stored observation, and place it in `WriteState::observed`. The package has
no Platform database dependency. A harness can keep these records in session
storage and atomically save the successful result with its messages/checkpoint.

Persist the original input, state, host, and cwd for recovery. A deliberate replay
must retain the original operation ID, revision, and content. Do not replace the
original observation with the successful result's revision when replaying that
operation. Supervisor deduplication can return the original receipt without
reapplying the write; receipts are not guaranteed to survive supervisor restarts.
An uncertain response or cancellation may follow a completed write. There are no
automatic retries, rollbacks, or claims of exactly-once effects. After a restart,
reconcile an ambiguous effect before issuing a new operation ID.

Paths use the same resolver as `read`, including absolute remote paths and
registered-root containment. `..` requires an absolute path instead; Windows
device paths and alternate streams are unsupported. Symlinks are followed within
the root by the execution filesystem. Read-only roots and root-directory targets
are rejected. Content size defaults to 16 MiB, further capped by the host limit.
Gateway HTTP body limits (including Base64 overhead) also apply.

Structured results include host ID, execution path, `created`, bytes written,
and revision; `to_text()` gives a short confirmation without echoing file contents.
Remote error codes and details are preserved, with helpful read-first/conflict
messages. Local errors use `source: "tool-write"`; shared path errors use
`source: "tool-filesystem"`.

Run `cargo test -p tool-write -p tool-read -p tool-filesystem` from `platform/`.
Tests exercise actual execution-client HTTP requests against a local supervisor
using temporary files, including replay and ambiguous responses. No hosted
machine or database is required.
