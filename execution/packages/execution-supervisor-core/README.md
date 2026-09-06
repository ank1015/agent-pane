# execution-supervisor-core

Operating-system implementation of `execution-core`.

This crate owns exact root-scoped filesystem behavior and durable local process sessions. It is embedded by both the managed-host `execution-supervisor` executable and the registered-host daemon.

It intentionally contains no provider API, wire protocol, public network server, host registration, hosted persistence, or tool-specific behavior.

Key guarantees:

- Filesystem paths cannot escape configured roots through lexical traversal or resolved symlinks.
- Writes default to conditional atomic replacement; opt-in in-place writes preserve inode semantics.
- Mutation receipts deduplicate `operation_id` within a supervisor generation.
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

In-place writes require an expected supervisor generation and follow symbolic
links, including dangling links with existing target parents. Missing-parent
retry creates the logical parent; root containment is checked before creation
and writing. File-only removals reject followed directory targets and unlink
the leaf entry. Other tools retain atomic-replacement and ordinary removal defaults.

Fenced writes/removes check generation before IO or receipt lookup, retain both
success and error responses, and install an unknown-outcome receipt before the
first IO await. If a future is dropped, replay returns `ExecutionLost` rather
than repeating a possibly completed write/unlink. In-place write errors may
report `mutation_outcome: possibly_partial`; cancellation can report `unknown`.
These receipts are in memory. After restart, old fenced requests are rejected,
including requests from clients with stale cached descriptors. Callers must
inspect/reconcile outcomes before issuing new identities. This does not provide
seamless cross-generation filesystem recovery.

Unified-exec callers opt into shell-type-aware launch with `ShellScript`, start
restart fencing with `expected_generation`, and bounded post-exit draining with
`output_drain_timeout_ms`. Legacy shell requests and omitted drain limits keep
their previous behavior. Early exit does not close the output journal: readers
may append until EOF or the drain deadline, then `Closed` is published and the
process slot is released. Retention starts at journal closure. On Unix, bounded
PTY readers use readiness polling so a descendant retaining the terminal cannot
leave a blocking reader stuck after the drain deadline.
