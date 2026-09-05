# Platform runtime contracts

Transport-independent Rust wire types for worker lifecycle, lease-scoped requests,
run/session history reads, response records and stable conflict codes. Used by
`platform-server` and re-exported from `platform-runtime-client::types`.

Requests reject unknown fields. Responses permit additive fields. Unknown error
code strings are preserved; recognized conflicts have `ErrorInfo::conflict()`.
Harness payloads remain open JSON objects; canonical messages use `llm-contracts`.

Private session storage uses `SessionStateQuery`, `SessionStateWrite`,
`SessionStateMutation`, and `SessionStateEntry`. Writes are part of `Commit`, with
an independent version per namespace/key and version-preserving deletion. Empty
state-write lists are omitted from serialization to preserve existing receipt
hashes. Reads are worker-owned; these records are not public conversation data.

`is_valid_harness_id` is the shared ASCII spelling validator for
`[a-z][a-z0-9_-]{0,127}`. Catalogue existence/enabled state, authorization and
database constraints remain server responsibilities.

No HTTP, SQLx, retries, scheduling or harness behavior belongs here. The server
retains validation and transactional invariants. Real-server integration tests
verify its response projections against these records.
