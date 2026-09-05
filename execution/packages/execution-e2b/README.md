# execution-e2b

E2B lifecycle and transport adapter for the execution system.

The crate has three boundaries:

- `E2bControlClient` creates, connects/resumes, inspects, and snapshots E2B sandboxes.
- `E2bEnvdClient` uses the documented envd health, filesystem upload, and Process APIs.
- `E2bExecutionRuntime` implements `execution-core` by invoking short-lived
  `execution-supervisor rpc` processes. Filesystem and durable process behavior
  remains inside the supervisor.

The gateway-facing convenience functions read `E2B_API_KEY`:

```rust,no_run
use execution_e2b::{create_base_sandbox, create_snapshot_sandbox, snapshot_sandbox};

# async fn example() -> Result<(), execution_e2b::E2bError> {
let sandbox_id = create_base_sandbox(None, None).await?;
let snapshot_id = snapshot_sandbox(&sandbox_id).await?;
let clone_id = create_snapshot_sandbox(&snapshot_id, Some(false)).await?;
# Ok(())
# }
```

The optional network-access argument defaults to `true` when it is `None` and
is sent to E2B as `allow_internet_access` for both base and snapshot templates.
The second base-creation argument is RAM in MiB; it defaults to `2048` and
accepts `1024`, `2048`, `4096`, or `8192`.

Sandbox and snapshot creation are not blindly retried because their POST
outcomes can be ambiguous and a replay can create duplicate resources. Connect
is retried with bounded exponential backoff because E2B documents it as the
operation that resumes a paused sandbox and only extends TTL. Supervisor RPCs
reuse the same request and operation IDs when retried, so the supervisor's
idempotency records preserve exact semantics.

Envd `Process.Start` is considered complete when its terminal process event is
received; the adapter does not wait for the surrounding Connect stream to close.
Handshake and dispatched operation retries share one total request deadline.

An E2B memory snapshot can contain the source host's running supervisor. When a
clone reports a different logical host ID, the adapter uses envd's process list
to identify that exact `serve` command, stops it with E2B's supported
`SIGNAL_SIGKILL`, and starts a fresh supervisor generation for the clone. The
workspace filesystem is not modified during this ownership handoff.

`create_base_sandbox` resolves RAM to these public templates:

| RAM (MiB) | vCPU | Template ID |
| ---: | ---: | --- |
| 1024 | 1 | `h5178y3chdmnimc6bk78` |
| 2048 (default) | 2 | `gls86opr20ek0ijjzvn7` |
| 4096 | 2 | `ujcvzqxszftbr47tx76e` |
| 8192 | 4 | `0zce89ggh7g74rred802` |

Each template contains the compatible supervisor at
`/usr/local/bin/execution-supervisor`. `SupervisorBinary::Upload` remains
available for testing locally built Linux binaries through envd.

An ignored live test exercises creation, envd, snapshot restore, and cleanup:

```text
E2B_API_KEY=... cargo test -p execution-e2b --test live -- --ignored --nocapture
```
