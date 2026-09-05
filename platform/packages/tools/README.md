# Shared tools

Each tool package is a Rust library used by harnesses. Tool inputs describe the
model's request; execution hosts, working directories, credentials, and limits
are supplied by the harness.

- [read](read/): line-window text reads, structured results, and numbered output
  through an injected execution runtime connected with `execution-client`.
- [write](write/): whole-file writes with observed-revision checks and caller-owned operation IDs.
- [edit](edit/): exact text replacements with revision checks and persistable prepared writes.
- [bash-minimal](bash-minimal/): foreground shell commands with optional timeout/workdir,
  bounded output, and caller-persisted execution progress.
- `filesystem/`: shared remote path resolution used by the filesystem tools.

The Platform workspace discovers tool crates under `packages/tools/*`.
