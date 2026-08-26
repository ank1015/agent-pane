# connector-tensorlake

Direct Tensorlake implementation of `execution-runtime`. It connects to an
existing sandbox through its ingress management API and does not install or
run `machine-daemon`.

Implemented capabilities:

- bounded reads, typed listings, inspection, walking, and path/content search;
- primitive conditional filesystem reads and writes, directories, removal,
  move, and copy;
- one-call `apply` mutations plus approval-friendly prepare/commit/abort;
- piped and PTY processes, input, resize, signals, termination, timeouts, and
  resource limits;
- sequence-numbered events with sandbox-side journals and process-output replay
  after connector disconnection or recreation;
- artifact metadata and chunked reads, including exact complete process output.

Tensorlake's file endpoints intentionally expose a small surface. Workspace,
mutation, artifact, and fallback filesystem semantics therefore run as
short-lived inline Python processes. Each requested user process has a Python
wrapper only for that execution's lifetime; the wrapper owns the child process,
nested PTY, control channel, output artifact, and sequence journal. There is no
resident connector service in the sandbox.

The process transport uses Tensorlake's native start, captured-output SSE,
stdin, inspect, and signal endpoints. Tensorlake output is line-oriented, but
the wrapper base64-encodes exact child byte chunks into newline-delimited JSON,
so agent process output remains byte-accurate and resumable. The wrapper also
persists its Tensorlake PID beside the journal so a new connector instance can
reattach while the process is running.

The caller owns lifecycle orchestration. `create(api_key, name)` creates a
named sandbox that suspends after ten idle minutes. `create_snapshot` creates a
reusable filesystem snapshot, and `create_from_snapshot(api_key, snapshot_id,
name)` restores it into another named sandbox. `ensure_started` resumes an idle
sandbox before use. Construct `TensorlakeConnectionConfig` from the sandbox's
ingress endpoint and API key, then provide workspace roots and a target-side
state directory through `TensorlakeRuntimeConfig`.

Current provider constraints:

- Python 3 is required in the sandbox.
- Tensorlake cannot enforce denied networking for only one child process, so
  `NetworkMode::Denied` is rejected. Configure network policy on the sandbox.
- Mutations validate every path before the first change, but Linux cannot
  guarantee cross-file atomicity; results report the actual outcome.
- Formatter and diagnostics post-actions are not advertised yet.
- Inline request payloads are limited to 1 MiB.
- Journals and artifacts survive connector disconnection, but not sandbox
  deletion.
- Native Tensorlake PTYs and TCP tunnels are not exposed as runtime
  capabilities yet. Recoverable tool PTYs use the execution wrapper instead.

Run the self-contained suite:

```sh
cargo test -p connector-tensorlake
```

The ignored live suite uses an already-running sandbox:

```sh
TENSORLAKE_SANDBOX_URL=https://<id-or-name>.sandbox.tensorlake.ai/ \
TENSORLAKE_API_KEY=... \
cargo test -p connector-tensorlake --test live_tensorlake -- --ignored
```
