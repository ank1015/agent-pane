# Bash minimal

`tool-bash-minimal` registers `bash-minimal` using `NAME`, `DESCRIPTION`, and
`input_schema()`. The only model arguments are required `command` and optional
`timeout` (positive integer milliseconds, default 120000, maximum 600000) and
`workdir`. Unknown arguments are rejected. There is no background flag,
description argument, PTY, interactive stdin, or persistent shell session.

The harness injects an execution runtime obtained through `execution-client`,
the environment directory, shell/environment configuration, and operation IDs.
Each invocation starts a fresh shell with closed stdin. `cd`, exported variables,
and shell functions do not affect later calls. Filesystem changes persist.
The configured shell runs on the remote host, never on the worker. With no shell
override, the supervisor selects its default (`SHELL` or `/bin/sh` on POSIX,
`COMSPEC` or `cmd.exe` on Windows). Shell overrides must be compatible with the
supervisor's `-c`/`-lc` POSIX or `/C` Windows invocation. Login mode defaults off.
The model-facing name does not guarantee GNU Bash on every host; harnesses should
tell the model the selected host OS and shell.

`workdir` defaults to the injected directory. Relative values resolve against
it; absolute values must be within a registered root. Parent (`..`) segments
are unsupported by the shared resolver; use an absolute path instead. Directory
existence, symlinks, and execution permissions are checked on the host.
Root selection constrains the starting directory, **not shell authority**:
commands can access whatever the host process's OS permissions permit. This is
not a command sandbox, permission system, or shell-command safety parser.

## Integration

```rust,no_run
use execution_client::ExecutionClient;
use execution_core::{ExecutionHostId, ExecutionPath, OperationContext, OperationId, ExecutionId};
use tool_bash_minimal::{BashConfig, BashIds, BashInput, BashTool, PreparedBash};

# async fn example(client: ExecutionClient, host_id: ExecutionHostId,
# cwd: ExecutionPath) -> Result<(), Box<dyn std::error::Error>> {
let context = OperationContext::new();
let host = client.connect_host(&context, host_id).await?;
let tool = BashTool::new(&host, cwd, BashConfig::default())?;
let prepared = tool.prepare(BashInput {
    command: "cargo test".into(), timeout: Some(300_000), workdir: None,
}, BashIds {
    operation_id: OperationId::generate(),
    execution_id: ExecutionId::generate(),
    terminate_operation_id: OperationId::generate(),
})?;

// Persist before dispatch. The prepared request pins host, cwd, shell, env and IDs.
let saved = serde_json::to_vec(&prepared)?;
let prepared: PreparedBash = serde_json::from_slice(&saved)?;
let mut running = tool.start(&context, &prepared).await?;
// Persist running here, before waiting.
let result = tool.wait(&context, &mut running).await?;
let model_text = result.to_text();
let is_error = result.is_error();
# let _ = (model_text, is_error);
# Ok(())
# }
```

For durable checkpoints or streaming, replace `wait` with a loop calling `poll`.
Persist `RunningBash` atomically with each consumed page. `BashPoll.events` exposes
raw stdout/stderr events for UI streaming and optional full-output archival.
On recovery, deserialize `RunningBash` and continue polling: no start is issued,
and the original supervisor generation and cursor are retained. Partial output
remains accessible via `running.output()` if a read fails. Persistence and event
delivery deduplication belong to the harness, not this package.

`start` never automatically retries. If its response is lost, the command may
already be running. Retain the exact prepared request for reconciliation or a
deliberate replay while the supervisor retains its deduplication receipt. Never
generate a new execution ID just because an HTTP request failed. Starting against
a descriptor with a different generation is rejected. The gateway currently
cannot fence a start to a generation, so a restart after the descriptor was read
still requires reconciliation; there is no exactly-once guarantee across restarts.

## Output and termination

The retained tail defaults to 2000 lines / 50 KiB of raw bytes, whichever limit
is reached first. Very long lines are byte-truncated. UTF-8 split across pages is
preserved; invalid UTF-8 is rendered lossily. The formatted status/truncation
footer and replacement characters can add bytes beyond the raw-output limit.
Stdout/stderr are combined in journal order, which need not reflect exact
cross-stream wall-clock ordering. Raw poll events retain stream labels.

The tool drains output pages even when the supervisor already reports a terminal
state. Nonzero exit codes are completed tool results with `is_error() == true`,
not transport errors. Lost/failed/cancelled execution states are also errors.
The current execution protocol reports a remote timeout as `Failed` without a
distinct timeout reason; this tool preserves that state rather than guessing.

No permanent full-output file is created automatically. The execution supervisor
retains the output journal according to its retention policy; a harness can
archive raw poll events and expose an artifact through its own storage. A model
using only this minimal tool receives the bounded tail, not access to that
journal. Truncation is always marked. This differs from OpenCode's output-file
integration while keeping its minimal input shape.

Remote `timeout_ms` bounds process lifetime independently of HTTP request timeouts.
Process reads long-poll for one second and are capped to the host's read limit.
Configure the client's per-request timeout to allow that wait plus network overhead.
`wait` attempts remote termination with a fresh five-second cleanup context when
the caller cancels or its operation deadline expires. Cleanup failures are attached
to the original error; termination is not claimed if unconfirmed. A transport
timeout alone leaves the execution available for reconnecting. Low-level `poll`
does not perform cleanup: the harness decides whether to reconnect or terminate.
`terminate` uses a stable, separate operation ID and can be called explicitly.
Dropping the wait future, losing the worker, or losing an ambiguous start response
cannot perform asynchronous cleanup; the remote timeout still applies.

Prepared/running payloads are trusted harness storage, never model arguments.
They can contain sensitive command strings, environment values, and output;
they intentionally do not implement Debug. Respect Platform state-size limits
when choosing custom output limits. No credential lookup or automatic retry is
performed by this package.

From `platform/`:

```sh
cargo test -p tool-bash-minimal
cargo clippy -p tool-bash-minimal --all-targets -- -D warnings
```

Integration tests run the real execution client against an authenticated local
HTTP fixture backed by the real supervisor, using temporary directories.
