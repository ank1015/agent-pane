# agent-pane

This repository is organized as three independent systems:

- [`execution/`](execution/README.md) — execution runtimes, supervision, and gateway services.
- [`llm/`](llm/README.md) — LLM contracts, providers, clients, and gateway services.
- [`platform/`](platform/README.md) — the platform server, dashboard, and shared platform packages.

Each system owns its workspace configuration, dependencies, build artifacts,
and development instructions. Run Cargo commands from the relevant system
directory rather than from the repository root.
