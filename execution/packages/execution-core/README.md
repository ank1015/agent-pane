# execution-core

Provider-neutral filesystem and durable process execution semantics.

This crate is the innermost execution-system dependency. It contains:

- Strongly typed host, supervisor, operation, process, write, root, revision, and cursor IDs.
- Root-relative filesystem paths and bounded filesystem requests.
- Conditional atomic-write contracts.
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
