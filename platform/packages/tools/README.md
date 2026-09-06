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
- [view-image](view-image/): validated original images for code mode, separate
  model-image preparation, and configurable inline or hosted URL delivery.
- [apply-patch](apply-patch/): Codex-compatible freeform patches with a Lark
  custom-tool definition and checkpointed, sequential remote mutations.
- [unified-exec](unified-exec/): recoverable Codex-compatible `exec_command`
  and `write_stdin` tools with caller-persisted process sessions and cursors.
- `filesystem/`: shared remote path resolution used by the filesystem tools.
- [firecrawl-search](firecrawl-search/): `search({query})`, with ten web results and model-facing schemas.
- [firecrawl-scrape](firecrawl-scrape/): `scrape({url})`, extracting bounded webpage/PDF Markdown.

The Platform workspace discovers tool crates under `packages/tools/*`.
