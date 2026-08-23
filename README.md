# agent-pane

A Cargo workspace for applications and shared Rust packages.

## Layout

```text
agent-pane/
├── apps/
│   ├── dashboard/            # React + Vite web client
│   ├── execution-gateway/    # Durable cloud router for machine execution
│   ├── llm-gateway/          # Stateless LLM gateway service
│   ├── machine-daemon/       # Deployable local machine execution process
│   └── platform/             # Dashboard backend and service orchestrator
├── packages/
│   ├── connector-e2b/        # Direct E2B execution-runtime adapter
│   ├── connector-tensorlake/ # Direct Tensorlake execution-runtime adapter
│   ├── connector-blaxel/     # Direct Blaxel execution-runtime adapter
│   ├── connector-daytona/    # Direct Daytona execution-runtime adapter
│   ├── execution-contracts/  # Serializable machine execution protocol
│   ├── execution-local/      # Local reference execution backend
│   ├── execution-protocol/   # Gateway/daemon transport envelopes
│   ├── execution-runtime/    # Async execution capability traits
│   ├── llm-contracts/        # Shared LLM domain contracts
│   ├── provider-anthropic/   # Non-streaming Anthropic transport
│   ├── provider-chatgpt/     # ChatGPT backend transport
│   ├── provider-deepseek/    # Non-streaming DeepSeek transport
│   ├── provider-fireworks/   # Non-streaming Fireworks transport
│   ├── provider-openai/      # Non-streaming OpenAI transport
│   └── provider-openrouter/  # Non-streaming OpenRouter transport
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

# Run the dashboard
cd apps/dashboard
pnpm install
pnpm dev

# Run the dashboard backend (llm-gateway must also be running)
cargo run -p platform

# Build and run llm-gateway, platform, and execution-gateway together
./scripts/dev-servers.sh
```

## Add a member

The Cargo workspace discovers Rust applications under `apps/` and shared
libraries under `packages/` automatically. Non-Rust applications are excluded
from Cargo explicitly:

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
