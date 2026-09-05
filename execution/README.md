# Execution system

This directory contains the clean-slate execution architecture while the legacy execution code remains in its current locations.

- `packages/execution-core` contains provider-neutral execution semantics and traits.
- `packages/execution-supervisor-core` implements local filesystem and durable process ownership.
- `packages/execution-wire` contains versioned request/response framing and shared runtime dispatch.
- `packages/execution-conformance` contains reusable black-box behavioral checks for every runtime implementation.
- `packages/execution-api` contains stable hosted-gateway HTTP resource contracts.
- `packages/execution-client` provides an authenticated gateway-backed execution runtime for tools and applications.
- `apps/execution-supervisor` exposes the supervisor core through private local IPC.
- `packages/execution-e2b` controls E2B lifecycle and forwards the wire protocol through envd to the supervisor.
- `apps/execution-gateway` stores E2B accounts and Registered Host credentials, manages host lifecycle, and routes execution operations.
- `apps/execution-host-daemon` connects a Registered Host outbound to the gateway and embeds the supervisor core.

Registered Hosts are supported on Linux, macOS, and Windows. The standalone
`execution-supervisor` executable remains Unix-only because it is the private
IPC artifact used inside Linux E2B environments; Registered Hosts do not use
that executable.

`execution/` is an independent Cargo workspace. Run its complete local verification from this directory:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

The repository's `execution-cross-platform.yml` workflow runs the Registered
Host components and production daemon end-to-end test natively on all three
operating systems.
