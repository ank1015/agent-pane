# agent-pane

A Cargo workspace for applications and shared Rust packages.

## Layout

```text
agent-pane/
├── apps/                     # Runnable binaries
├── packages/
│   └── llm-contracts/        # Shared LLM domain contracts
└── Cargo.toml                # Workspace configuration
```

Workspace-wide package metadata, dependencies, and lints live in the root
`Cargo.toml`. Applications belong in `apps/`; reusable library crates belong in
`packages/`.

## Commands

```sh
# Build every workspace member
cargo build --workspace

# Test and lint every workspace member
cargo test --workspace
cargo clippy --workspace --all-targets

# Format every workspace member
cargo fmt --all
```

## Add a member

The workspace discovers shared libraries under `packages/` automatically. Add
`apps/*` to the root `members` list when the first application is created:

```sh
cargo new --bin apps/my-app
cargo new --lib packages/my-library
```

To share a local package, declare it once under `[workspace.dependencies]` in
the root `Cargo.toml`, then opt in from a member:

```toml
[dependencies]
my-library.workspace = true
```
