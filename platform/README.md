# Platform system

This directory contains the standalone platform architecture. The existing platform and dashboard applications are being rebuilt here as the `server` and `dashboard` apps.

- `apps/` contains independently deployable platform applications.
- `apps/dashboard` contains the React and Vite management interface.
- `apps/server` contains the Rust API used by the dashboard.
- `apps/sites-service` contains site storage, lifecycle, source/release publication,
  and isolated static hosting; see its
  [setup and internal API](apps/sites-service/README.md).
- `packages/` contains reusable platform crates.

`platform/` is an independent Cargo workspace. After the first workspace member is added, run its complete local verification from this directory:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

## Local development

After configuring the `.env` files from the examples in each app and installing
the dashboard dependencies, start the API server, Sites service, worker, and
dashboard together from any directory:

```sh
platform/start.sh
```

The launcher waits for the API before starting the worker. Press Ctrl-C to stop
the complete stack; if any app exits, the launcher stops the others. The API
startup wait defaults to 300 seconds and can be changed with
`PLATFORM_START_TIMEOUT_SECONDS`.
