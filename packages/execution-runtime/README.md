# execution-runtime

Async Rust interfaces implemented by execution backends.

This crate composes the versioned contracts from `execution-contracts` into:

- `ExecutionRuntime`
- `WorkspaceQuery`
- `WorkspaceMutation`
- `ProcessRuntime`
- `ArtifactStore`
- `BasicFileSystem`
- optional `CodeIntelligence` and `MediaProcessing` capability boundaries

It contains no transport or target implementation. A local machine daemon, SSH
worker, cloud VM connector, or sandbox-provider adapter implements these same
traits. HTTP, WebSocket, stdio, and provider SDK layers translate to and from
them.

Every operation receives an `OperationContext` for cooperative cancellation and
process-local deadlines. Durable process output is returned as a sequence-numbered
stream, while artifacts may be streamed in bounded chunks.

`validate_capability_consistency` ensures a runtime's executable capabilities
match its advertised descriptor. The `conformance` module contains reusable
response checks; fixture-driven behavioral suites will grow with the first
concrete backend.
