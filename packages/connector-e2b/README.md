# connector-e2b

Direct E2B implementation of `execution-runtime`. It connects to an existing
E2B sandbox through envd and does not install or run `machine-daemon`.

Implemented capabilities:

- bounded reads, typed listings, inspection, walking, and path/content search;
- primitive conditional filesystem reads and writes, directories, removal,
  move, and copy;
- one-call `apply` mutations plus approval-friendly prepare/commit/abort;
- piped and PTY processes, input, resize, signals, termination, timeouts, and
  resource limits;
- sequence-numbered process events with sandbox-side journaling and replay
  after connector disconnection or recreation;
- artifact metadata and chunked reads, including complete process output.

Filesystem and mutation operations run as short-lived inline Python commands.
Each requested user process has a Python wrapper only for that process's
lifetime so it can own the child process, PTY, control channel, and output
journal. There is no resident connector service in the sandbox.

The caller owns lifecycle orchestration. `create_from_snapshot(api_key,
snapshot_id)` creates an E2B sandbox through the control API and returns its
sandbox ID. Construct `E2bConnectionConfig` from that ID and the returned envd
access token once connection metadata is resolved, then provide target-side
workspace roots and a state directory through `E2bRuntimeConfig`.

Current provider constraints are explicit:

- Python 3 must be present in the sandbox.
- E2B cannot enforce a denied-network policy for one child process, so
  `NetworkMode::Denied` is rejected. Configure network policy on the sandbox.
- Mutations validate all paths before the first change, but E2B/Linux cannot
  guarantee cross-file atomicity; results report `atomic: false` and can report
  a partial commit.
- Formatter and diagnostics post-actions are not advertised yet.
- Inline request payloads are limited to 1 MiB. Large existing content should
  be passed through an artifact rather than embedded in a request.
- Journals and artifacts survive connector disconnection, but naturally cease
  to exist after the sandbox is deleted.

The normal test suite is self-contained:

```sh
cargo test -p connector-e2b
```

The ignored live suite requires a running sandbox and current envd access
token. It covers mutations, queries, artifact streaming, output recovery, PTY
resize, and idempotent input:

```sh
E2B_SANDBOX_ID=... \
E2B_ENVD_ACCESS_TOKEN=... \
cargo test -p connector-e2b --test live_e2b -- --ignored
```
