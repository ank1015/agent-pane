# Unified exec only harness

`UnifiedExecOnlyHarness::new(llm, execution)` implements the shared Harness
interface with exactly two function tools: `exec_command` and `write_stdin`.
The model performs all file reads, writes, edits, searches and other operations
through the execution host's shell. No image publisher or cloud-storage setup is
required. Patch interception in unified exec is disabled: even a command named
`apply_patch` runs as an ordinary shell command, requiring an installed executable.
No `apply_patch` or `view_image` tool is registered.

## Configuration and providers

Configuration matches the Basic Codex harness: `model`, `reasoning_level`,
`environment`, optional `account_id` and `system_prompt_append`. Environments are
machine descriptors or snapshot-backed sandboxes resolved once per session.
Use the catalog example for the full schema and supported model list.

OpenAI and ChatGPT use ordinary Responses, native assistant replay, stable session
cache keys, low verbosity, configured reasoning effort, no reasoning summary by
default, and `parallel_tool_calls: false`. Responses Lite is not enabled.
Fireworks uses the Basic CC policy: model-specific reasoning-effort mapping,
`prompt_cache_key` equal to the Platform session UUID, and `max_tokens` from its
provider catalog. Native Fireworks reasoning and tool-call messages are retained.
Raw provider options cannot be supplied through run configuration. All tools are
executed sequentially, including batches returned by Fireworks.

## Durability and process lifetime

Instructions, definitions, execution limits, provider options, model operation
identity, history revision and tool progress are pinned in the run checkpoint.
History is fully paginated, with no automatic compaction or trimming. Stable
identities are saved before process starts and stdin interactions. Every returned
running state is persisted before another poll. Tool results, committed output
cursors and tool-index advancement are saved atomically.

Private state under `unified-exec-only-harness` contains `execution-target`,
`exec-allocator`, and individual `exec:<id>` entries. Each process stores its
execution handle, supervisor generation, cwd, PTY mode, output cursor, termination
identity and creation run. Numeric aliases are never reused, including aliases
exposed in inherited fork history. Forks do not inherit private process sessions.

Live processes survive successful runs and worker drain. Abort stops the active
process and processes created by that run; unrelated earlier sessions survive.
Failed runs perform the same process cleanup before recording failure. Cleanup
progress and failure intent are durable. Lost supervisor generations or expired
processes are reported, never silently restarted. Gateway/supervisor retention
limits still apply; arbitrary shell effects are not exactly-once across restarts.
Session deletion/expiry has no cleanup hook; operators must dispose of remaining
host processes when disposing of a session.

## Limits and worker registration

At most 32 process sessions are retained. Poll completed sessions to release them.
Commands are capped at 32 KiB and stdin at 8 KiB; output collection is 64 KiB and
model output at most 10,000 tokens per call. Tool checkpoints are capped at
512 KiB, assistant/user messages at 512 KiB and commits at 900 KiB. These are
transport/storage bounds, not limits on the sizes of files manipulated by shell.

Set `UNIFIED_EXEC_ONLY_ENABLED=true`, plus the shared LLM/execution gateway
configuration. Apply Platform migrations and rebuild/restart the shared worker.
The catalog migration registers `unified-exec-only-harness` independently from
worker capability advertisement. Project harness enablement is managed through
the existing Platform configuration.

```sh
cargo run -p unified-exec-only-harness --example catalog
cargo test -p unified-exec-only-harness -p tool-unified-exec
DATABASE_URL=postgresql://localhost/postgres cargo test -p unified-exec-only-harness -- --include-ignored
cargo clippy -p unified-exec-only-harness -p platform-worker --all-targets -- -D warnings
```
