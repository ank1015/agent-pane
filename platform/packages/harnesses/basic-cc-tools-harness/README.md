# Basic CC tools harness

`BasicCcToolsHarness::new(llm_client, execution_client)` implements the shared
`harness_runtime::Harness` interface. Register it under `basic-cc-tools-harness`.
It uses the model-facing tools `read`, `write`, `edit`, and `bash` (implemented by
`tool-bash-minimal`). No compaction, permission prompts, shell sandbox, subagents,
or code mode is added. Commands inherit the execution host's OS authority.

Configuration is exposed by `config_schema()`:

```json
{
  "model": {"provider":"openai", "id":"gpt-5.6-terra"},
  "reasoning_level":"high",
  "execution": {"host_id":"<host UUID>", "workspace_root":"work", "path":"project"}
}
```

Optional fields: `account_id` and `system_prompt_append`. Reasoning levels are
`low`, `medium`, `high`, `xhigh`, `max`. The harness constructs provider options
using Pi-style policy: stable session cache identity, provider-owned output
limits, Responses reasoning/native replay, and model-specific Fireworks effort
mapping. All models in the current OpenAI, ChatGPT and Fireworks provider
catalogs are accepted. New catalog entries need an appropriate reasoning mapping.
Provider-native settings are not accepted from run config. Gateway credentials
are injected through clients, not stored in the run.

The execution target is explicit: an already connected host, registered root ID,
and root-relative directory. Environment lookup/sandbox provisioning are not part
of this harness. The initial system prompt includes host OS/root/directory and
correct instructions for the four tools, and can have text appended.

## Loop and durability

Session history is fully paginated and sent without trimming or compaction.
Native assistant messages, usage and reasoning content are preserved. Context
overflow is a reported provider failure, not silent truncation. New user inputs
are appended/acknowledged atomically at model/tool-batch boundaries. Tools execute
sequentially in emitted order. Tool argument/execution errors become tool results.
Normal final answers/refusals finish the run; length/filter/empty/unsupported
stops fail explicitly. Completion rechecks inputs and handles Platform's pending
input conflict. Unsupported input kinds are explicitly rejected.

Checkpoints pin instructions, tool schemas, provider options, history revision,
LLM idempotency key/run ID, tool index/prepared operation, and Bash progress.
The history is referenced, not duplicated in checkpoints. Stable external
identities are persisted before effects. Recovering an activation reconnects or
replays the same saved operation, never generates a new identity for an ambiguous
failure. Terminal retryable LLM failures get at most three generations with
bounded backoff; successful loop iterations have no artificial count limit.
Gateway transport ambiguity yields to worker recovery with the saved operation.
The clients/gateways' retention and generation limitations still apply; this is
not exactly-once execution across arbitrary gateway/supervisor crashes.

Read observations are bounded per-run checkpoint data (128 entries / 64 KiB).
Each successful edit/write consumes the matching observation; it never records a
replacement observation. Revision conflicts invalidate observations. A new run
starts empty. No session-scoped state or separate database is used. Eviction
requires another read. Revisions still protect changes between read and mutation.

Write contents and edit source/results are limited to 64 KiB so prepared mutations
fit durable JSON checkpoints, even with escaping. Reads and Bash retain their
50 KiB output defaults. Assistant records are limited to 512 KiB; commits are
bounded below Platform's 1 MiB limit. Large-output/artifact storage is not added.

User abort calls `llm-client.abort` or terminates the active remote Bash execution,
then records cancelled tool results and `Aborted`. An ambiguous LLM submit must
be reconciled with its original key first (this can briefly submit work if the
original submit never arrived). Abort cannot undo edits or guarantee stopping
provider billing. Worker drain releases the lease as ready without cancelling
external work; ownership loss stops local effects. There are no detached tasks.

## Registration and verification

The shared worker registers this implementation when `BASIC_CC_TOOLS_ENABLED=true`.
Supply execution/LLM gateway URLs and tokens from the worker's `.env.example`.
Platform's registration migration seeds a harness catalog record with this ID
and configuration schema, preserving an existing row. The package's `catalog`
example prints the body for the administrative
PUT endpoint; it does not mutate a deployment.

```sh
cargo run -p basic-cc-tools-harness --example catalog
cargo test -p basic-cc-tools-harness
DATABASE_URL=postgresql://localhost/postgres cargo test -p basic-cc-tools-harness -- --include-ignored
cargo clippy -p basic-cc-tools-harness --all-targets -- -D warnings
```
