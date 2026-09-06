# execution-core

Provider-neutral filesystem and durable process execution semantics.

This crate is the innermost execution-system dependency. It contains:

- Strongly typed host, supervisor, operation, process, write, root, revision, and cursor IDs.
- Root-relative filesystem paths and bounded filesystem requests.
- Conditional writes with atomic-replacement or generation-fenced in-place semantics.
- Durable process-session requests and sequence-numbered events.
- Cursor-based process output reads.
- Process input, PTY resize, signal, and termination contracts.
- Structured runtime errors and semantic validation.
- Object-safe async `FileSystem`, `ProcessRuntime`, and `ExecutionRuntime` traits.
- Process-local cancellation and deadlines through `OperationContext`.

It intentionally contains no:

- Wire envelope or codec.
- Provider API.
- HTTP or WebSocket client.
- Filesystem, process, or PTY implementation.
- Hosted persistence.
- Tool-specific behavior.

Implementations must treat `process.read` as the authoritative process-output interface. Output events are retained and addressed by monotonically increasing sequence numbers so callers can recover after a transport interruption.

Every mutating filesystem or process-control request carries a stable operation identifier. Process input uses a dedicated `write_id` so an ambiguous retry cannot type the same bytes twice.

Filesystem writes default to `WriteStrategy::AtomicReplace`. `InPlace` requires
`expected_generation` and `follow_symlinks: true`; it preserves existing inode
semantics and may partially modify a file on failure. With `create_parents`, it
attempts the write first and retries a missing parent after creating the logical
parent path. `RemoveTargetKind::File` checks the followed target is not a directory
before unlinking the leaf, while the default `Any` keeps ordinary removal behavior.

`expected_generation` on writes/removes is checked by the host before deduplication
or mutation. Generation-scoped receipts must prevent ambiguous retries from
reapplying IO; a new generation must reject old requests with `ExecutionLost`.
These fields are omitted at their legacy defaults. New requests require upgraded
peers; strict old deserializers reject the new fields rather than silently falling
back to atomic writes or unfenced removals. The wire envelope version and operation
set are unchanged.

Process starts optionally accept `expected_generation` (checked before spawn)
and `output_drain_timeout_ms` (publish child exit immediately and bound subsequent
output draining). Both default to absent and are omitted when serialized, keeping
existing callers' behavior. With a drain timeout, `Exited` can precede additional
`Output` events; `Closed` marks the end of the journal. Without one, process exit
is published together with `Closed` after all output readers finish.

`CommandSpec::ShellScript` selects launch flags by shell type and discovers the
executable on the host, accepting empty scripts. The existing `Shell` variant
retains its original platform-based launch convention.
