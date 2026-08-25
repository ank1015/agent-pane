# execution-contracts

Transport-neutral Rust contracts for executing agent tools against local,
remote, and sandbox-backed machines.

The crate defines five layers:

- machine discovery and versioned capability negotiation;
- bounded workspace queries for reads, listings, and search;
- validated workspace mutation plans with prepare/commit and one-shot apply;
- durable process sessions with attachable output and idempotent input writes;
- an artifact store plus primitive filesystem operations for adapters that
  cannot implement the higher-level workspace capabilities.

It deliberately contains no HTTP, WebSocket, database, operating-system, or
sandbox-provider implementation. Those belong in transports and execution
adapters. Contract structs accept additive JSON fields for forward
compatibility; behavior is negotiated through protocol and capability major
versions.

All paths are explicit [`PathSpec`](src/path.rs) values. Portable workspace
paths are root-relative and reject traversal, absolute paths, Windows drive
prefixes, and backslashes. Native paths require an authorization grant.

Use `Validate` (or `parse_json`) after decoding untrusted requests. Serde alone
checks the JSON shape; validation checks semantic constraints such as non-zero
limits, valid pagination, and non-empty mutation plans.
