# Platform system

This directory contains the standalone platform architecture. The existing platform and dashboard applications are being rebuilt here as the `server` and `dashboard` apps.

- `apps/` contains independently deployable platform applications.
- `apps/dashboard` contains the React and Vite management interface.
- `apps/server` contains the Rust API used by the dashboard.
- `packages/` contains reusable platform crates.

`platform/` is an independent Cargo workspace. After the first workspace member is added, run its complete local verification from this directory:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets
cargo test --workspace
```
