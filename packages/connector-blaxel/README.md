# connector-blaxel

Direct Blaxel implementation of `execution-runtime`. It connects to an
existing sandbox through the sandbox API URL in the resource metadata and does
not install or run `machine-daemon`.

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

The connector currently uses short-lived inline Python processes for workspace,
mutation, artifact, and fallback filesystem semantics. This provides the exact
contract behavior for path containment, revisions, pagination, and staged
mutations while leaving room to add Blaxel's native filesystem search and tree
endpoints as exact-request fast paths. Each user process has a Python wrapper
only for that execution's lifetime; there is no resident connector service.

The process transport uses Blaxel's native named-process start, inspect, log
stream, stop, and kill endpoints. Because the REST process API has no stdin or
PTY-resize method, input, resize, and child signals use idempotent control files
consumed by the wrapper. Blaxel logs are line-oriented, but the wrapper
base64-encodes exact child byte chunks into newline-delimited JSON, keeping
output byte-accurate and resumable. It also persists the outer PID beside the
journal so a recreated connector can reattach while the process is running.

The caller owns lifecycle orchestration. `create` provisions the current Blaxel
default sandbox image, `create_snapshot` creates a restorable sandbox snapshot,
and `create_from_snapshot` forks a source sandbox at a selected snapshot into a
caller-provided target sandbox ID. `resolve_workspace` can infer the workspace
when an API key has access to exactly one.
Construct `BlaxelConnectionConfig` from the new sandbox metadata URL, API key,
and workspace, then provide workspace roots and a target-side state directory
through `BlaxelRuntimeConfig`.

Current provider constraints:

- Python 3 is required in the sandbox. Blaxel's current default sandbox includes
  Python 3; the connector uses `python3` by default and allows the command to be
  configured for custom images.
- Blaxel cannot enforce denied networking for only one child process, so
  `NetworkMode::Denied` is rejected. Configure network policy on the sandbox.
- Mutations validate every path before the first change, but Linux cannot
  guarantee cross-file atomicity; results report the actual outcome.
- Formatter and diagnostics post-actions are not advertised yet.
- Inline request payloads are limited to 1 MiB.
- Journals and artifacts survive connector disconnection, but not sandbox
  deletion.
- Native Blaxel PTYs and TCP tunnels are not exposed as runtime
  capabilities yet. Recoverable tool PTYs use the execution wrapper instead.

Run the self-contained suite:

```sh
cargo test -p connector-blaxel
```

The ignored live suite uses an already-running sandbox:

```sh
BLAXEL_SANDBOX_URL=https://sbx-<sandbox>-<workspace>.<region>.bl.run/ \
BLAXEL_API_KEY=... \
BLAXEL_WORKSPACE=<workspace> \
cargo test -p connector-blaxel --test live_blaxel -- --ignored
```
