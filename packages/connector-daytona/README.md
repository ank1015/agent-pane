# connector-daytona

Direct Daytona implementation of `execution-runtime`. It connects to an
existing sandbox through the Daytona Toolbox API and does not install or run
`machine-daemon`.

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

Daytona's file endpoints intentionally expose a small surface. Workspace,
mutation, artifact, and fallback filesystem semantics therefore run as
short-lived inline Python processes. Each requested user process has a Python
wrapper only for that execution's lifetime; the wrapper owns the child process,
nested PTY, control channel, output artifact, and sequence journal. There is no
resident connector service in the sandbox.

The process transport uses Daytona's named shell sessions, asynchronous exec,
WebSocket logs, stdin, inspection, and session termination endpoints. The
connector gives each durable execution a deterministic Daytona session ID, so
a retried start can find the existing command instead of launching it twice.
The transport accepts both the documented stdout/stderr-prefixed log frames and
the unlabeled combined stream currently returned by hosted Daytona. Command
completion is polled independently because the log socket is not itself an
exit notification; a final HTTP log snapshot fills any unobserved suffix.

Daytona output framing is provider-specific, but the wrapper base64-encodes
exact child byte chunks into newline-delimited JSON. Agent process output is
therefore byte-accurate and resumable across connector disconnections while
the sandbox and its state directory remain available. Interactive controls are
acknowledged by the wrapper before their connector calls return.

The caller owns lifecycle orchestration. `create_from_snapshot(api_key,
snapshot)` creates a Daytona sandbox through the control API and returns its
sandbox ID. Construct `DaytonaConnectionConfig` from the sandbox's Toolbox URL
and API key, then provide workspace roots and a target-side state directory
through `DaytonaRuntimeConfig`. `DaytonaConnectionConfig::for_sandbox` builds
the hosted Toolbox URL from a sandbox ID.

Current provider constraints:

- Python 3 is required in the sandbox.
- Daytona cannot enforce denied networking for only one child process. The
  connector accepts `NetworkMode::Denied` only when the caller declares that
  networking has already been blocked for the entire sandbox.
- Mutations validate every path before the first change, but Linux cannot
  guarantee cross-file atomicity; results report the actual outcome.
- Formatter and diagnostics post-actions are not advertised yet.
- Inline request payloads are limited to 1 MiB.
- Journals and artifacts survive connector disconnection, but not sandbox
  deletion.
- Native Daytona PTYs are not required by the runtime adapter. Recoverable tool
  PTYs are nested inside the execution wrapper and controlled through the
  ordinary session input endpoint.

Run the self-contained suite:

```sh
cargo test -p connector-daytona
```

The ignored live suite uses an already-running sandbox:

```sh
DAYTONA_TOOLBOX_URL=https://proxy.app.daytona.io/toolbox/<sandbox-id>/ \
DAYTONA_API_KEY=... \
cargo test -p connector-daytona --test live_daytona -- --ignored
```
