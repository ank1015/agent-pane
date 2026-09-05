# execution-supervisor-core

Operating-system implementation of `execution-core`.

This crate owns exact root-scoped filesystem behavior and durable local process sessions. It is embedded by both the managed-host `execution-supervisor` executable and the registered-host daemon.

It intentionally contains no provider API, wire protocol, public network server, host registration, hosted persistence, or tool-specific behavior.

Key guarantees:

- Filesystem paths cannot escape configured roots through lexical traversal or resolved symlinks.
- Writes are conditional, atomic, and retry-safe by `operation_id`.
- Process starts and control operations are idempotent.
- Process input is deduplicated by `write_id`.
- Output is journaled locally with monotonic sequence numbers.
- `process.read` can resume after a transport interruption.
- Pipes, PTYs, resizing, signals, process groups, timeouts, and cleanup share one implementation.
- Every runtime start creates a new supervisor generation.

Linux and macOS use native process-group signals. Windows maps interrupt,
terminate, and kill requests to native child termination, matching the previous
local runtime's Windows behavior. Filesystem paths remain forward-slash-separated
on the wire and are resolved using the host's native path implementation.
