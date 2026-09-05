# Platform packages

Reusable crates in the standalone platform architecture belong in this directory.

Shared model tools live in [tools](tools/), with one library crate per tool.

- [harness-runtime](harness-runtime/): shared `Harness`, `Execution`, and `Signals` interface for independently implemented harness libraries.
- [platform-runtime-contracts](platform-runtime-contracts/): shared worker wire types and conflict codes.
- [platform-runtime-client](platform-runtime-client/): typed worker/harness transport, stable request identity and bounded retries.
