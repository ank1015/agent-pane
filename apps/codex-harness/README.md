# codex-harness

`codex-harness` is the long-running broker server for the `codex` coding
harness. It executes exactly one logical Codex turn for each Agent broker
request.

It uses `packages/agent-harness-sdk` for:

- a shared durable JetStream turn consumer;
- per-instance command-result and cancellation consumers;
- bounded concurrent turn dispatch and progress acknowledgements;
- authenticated Agent transcript access;
- idempotent lifecycle command publication and result routing.

`CodexRuntime` plans the durable transcript, forms the request, performs native
compaction when required, calls the model through the LLM gateway, commits the
assistant, executes its tools, and returns `Continue` or `Complete` to Agent.
It supports `exec`, `wait`, `exec_command`, and `write_stdin`, including
code-mode nested calls, plus `apply_patch` and `view_image`. When the run's
`web_search_enabled` harness setting is true, code mode also exposes Codex's
namespaced `tools.web__run` extension.

Before the primary model call, the runtime commits a
`codex.primary_call_started` marker. A redelivery that sees that marker without
an assistant fails explicitly instead of sampling a second primary response
for the same `(run_id, turn_number)`. If an assistant is already committed,
redelivery either completes it again or executes only its missing tool calls.

The runtime validates resolved OpenAI/ChatGPT configuration
for `gpt-5.6-sol`, `gpt-5.6-terra`, and `gpt-5.6-luna`, and plans redelivered
turns from the durable Agent transcript without repeating a primary model call.
It also deterministically forms a provider-neutral model request from:

- Codex's shared Sol/Terra/Luna base instruction template, with the existing
  external prompt append/replace contract;
- a validated environment snapshot containing cwd, shell, date, timezone, and
  execution-runtime workspace roots, without AGENTS.md or permission context;
- revision-ordered session messages and a typed `codex.compaction` replacement
  checkpoint containing the provider's opaque native compaction item;
- the code-mode-only `exec` and `wait` definitions, embedding `apply_patch`,
  `exec_command`, `write_stdin`, and `view_image` as nested tools and, when
  enabled, Codex's full `web.run` description and command schema;
- Codex Responses Lite layout and header, including a namespaced
  `additional_tools` developer item, developer base-instruction message,
  all-turn reasoning context, encrypted reasoning replay, low verbosity,
  parallel tool calls, and session prompt-cache affinity.

The compaction subsystem mirrors Codex Responses Compaction V2. It checks the
last server-reported active token usage before sampling (falling back to a
deterministic estimate), uses the 90% auto-compaction threshold, appends the
provider-native `{"type":"compaction_trigger"}` input item, requires exactly
one opaque `compaction`/`compaction_summary` response item, retains recent real
user/developer history under a 64k-token budget, and persists that replacement
boundary as a `codex.compaction` session message. On replay, the checkpoint is
expanded into retained messages followed by the opaque native item. As in
Codex, a context-length error marks the context full and is returned without an
in-loop compaction retry; it forces compaction at the next pre-sampling
boundary. The persisted checkpoint prevents redelivery from repeating that
boundary.

The model client calls `POST /v1/complete` on the shared LLM gateway and buffers
one complete assistant response. Its retry behavior follows Codex rather than
Pi: request/transport failures receive four retries with a 200ms exponential
backoff and ±10% jitter; retryable response failures receive five retries;
response-level retry delays are honored, while raw HTTP retries use local
backoff. HTTP 429, context overflow, quota/usage limits, authentication,
configuration, invalid requests, and semantic overload failures are terminal.
Context overflow is classified distinctly so the turn runtime can mark the
window full and defer compaction to the next pre-sampling boundary. Cancellation
and the turn deadline interrupt both the HTTP request/body read and retry sleep.

`web.run` uses the gateway's separate `POST /v1/search` endpoint. Its request
matches Codex's default cached mode: direct callers only, external live web
access false, a 2,500 approximate-token output limit, and a native input tail
containing the last two visible user messages plus up to 1,000 approximate
tokens of assistant text between them. Only the search response's `output`
string is returned to JavaScript; provider result metadata is not injected into
the model-visible tool value.

Stateful tools use a deliberately small hybrid persistence boundary:

- `codex_exec_sessions` durably maps Codex's numeric `session_id` to the
  execution gateway's process ID and last consumed output sequence. The
  execution gateway owns the actual process.
- `codex_code_mode_state` durably allocates cell IDs and stores values written
  through code mode's `store()` helper. A new cell loads those values through
  `load()` even after a harness restart.
- Live V8 sessions and cells stay in a process-local registry because an active
  isolate cannot be serialized. They are released on explicit session cleanup,
  harness shutdown, or after `CODEX_HARNESS_CODE_MODE_IDLE_TTL_SECONDS` without
  harness interaction (24 hours by default). Durable code-mode state is not
  deleted by live-session cleanup.

There is intentionally no tool-call journal, process table, or durable
code-mode-cell table.

The stateful dispatcher resolves the configured machine through the execution
gateway. `exec_command` and `write_stdin` share the Agent-session-scoped
PostgreSQL mapping, so a process yielded in one broker turn can be polled in a
later turn. Code mode keeps one live V8 session per Agent session. Before each
`exec` or `wait`, its stable nested-tool delegate is rebound to the current
machine, workspace, and cancellation context; a resumed cell therefore does
not retain the operation context from the turn where it originally yielded.
Nested `apply_patch`, `exec_command`, `write_stdin`, and `view_image` calls use
the same execution target and state repositories as their direct adapters.

Environment snapshot resolution intentionally takes date and timezone as
inputs: the execution runtime descriptor supplies machine paths, roots, and the
default shell, but its current contract does not expose the target clock.

## Run locally

Start the Agent development PostgreSQL and NATS services, then Agent:

```sh
docker compose -f apps/agent/compose.yaml up -d
docker compose -f apps/agent/compose.yaml exec postgres \
  createdb -U postgres codex_harness
cp apps/codex-harness/.env.example .env.codex-harness
# Set CODEX_HARNESS_AGENT_TOKEN to the same value as AGENT_HARNESS_TOKEN.
set -a
source .env.codex-harness
set +a
cargo run -p codex-harness
```

When `CODEX_HARNESS_AGENT_CONTROL_TOKEN` is set, startup idempotently registers,
activates, and enables the packaged revision. Leave it unset when the
harness is provisioned separately.

## Broker topology

| Direction | Subject | Consumer behavior |
| --- | --- | --- |
| Agent → Codex | `agent.harness.codex.turn.requested.v1` | Shared durable pull consumer |
| Codex → Agent | `agent.run.{run_id}.command.v1` | Durable Agent command stream |
| Agent → Codex | `agent.harness.codex.result.v1` | Per-instance result router |
| Agent → Codex | `agent.harness.codex.run.cancelled.v1` | Per-instance cancellation listener |

## Verify

```sh
cargo test -p codex-harness
cargo clippy -p codex-harness --all-targets -- -D warnings
```

The live PostgreSQL round-trip test is opt-in because it needs an empty test
database:

```sh
CODEX_HARNESS_TEST_DATABASE_URL=postgres://localhost/codex_harness_test \
  cargo test -p codex-harness --test persistence -- --ignored
```
