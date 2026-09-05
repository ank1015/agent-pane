# Environments harness

Package `environments-harness`, registered harness ID `environments`. A sequential
environment builder for the shared worker: session history → LLM → tools → LLM,
until a final response. No compaction, permissions flow, or dedicated database.

Config: `model: {provider, id, name?}`, `reasoning_level` (low/medium/high/xhigh/max),
optional LLM `account_id` and optional
`system_prompt_append`. Provider/cache policy is shared with the basic harness.
The run owns project scope; no project or fixed execution target is configured.

Tools: `read`, `write`, `edit`, `bash`, `search`, `scrape`, `list_environments`,
`list_execution_resources`, `list_snapshots`, `create_sandbox`, `snapshot_sandbox`,
and `create_environment`. Web tools are always enabled and cannot be disabled
through configuration. Inject `WebTools` with Firecrawl contexts; runs without
credentials fail before model dispatch. Gateway and Platform credentials never
appear in model config or tool arguments. `tool_definitions(web)` exposes the
model-facing schemas without opening a host connection.

Filesystem wrappers add only `host_id`. Sole roots resolve automatically;
multi-root hosts require absolute `file_path` or absolute Bash `workdir` inside a
registered root. Each prepared operation pins host/root/path for recovery. Bash
is stateless, defaults to two minutes, and supports up to 30 minutes. Fresh reads
are required before every edit/overwrite, including after a successful mutation.
Observations are run-scoped and bounded; writes/edits retain the 64 KiB limit.

Create sandbox from `source: {type: "base", e2b_account_id?, ram?}` or
`{type: "snapshot", snapshot_id}`. Optional top-level `network_access` defaults
to true for both sources. Base-only `source.ram` accepts 1024, 2048, 4096 or 8192
MiB (default 2048); vCPU is automatic. Snapshots retain their source hardware.
These options are persisted with the creation request for retry-safe replay.
Optional name and `timeout_seconds` map to the
gateway. Returned lifecycle resources are polled to readiness, with a ten-minute
per-operation readiness window; timeout results include the known resource ID
and do not delete it. Gateway filesystem requests resume paused builders on use.
Account discovery returns metadata only, never credentials. Lists return up to
200 items with explicit total/truncated fields and bounded rendered output.

Environment creation uses Platform's fenced, idempotent run command. Types are
`machine` (machine_id) or `sandbox` (snapshot_id), plus name, workspace_root ID and
root-relative path. No setup scripts, environment update/delete, or automatic
cleanup are included. Reference validity does not prove setup correctness: the
agent must inspect and verify the directory before publishing it.

LLM IDs, prepared filesystem operations, lifecycle keys and returned resource
IDs are checkpointed before the next effect. Transport ambiguity preserves the
plan for takeover; normal tool failures become model-visible results. Steering
is consumed after a model/tool batch, and completion handles pending-input races.
Abort cancels model work and terminates known Bash executions. Accepted lifecycle
jobs may continue; interrupted resource IDs/keys are retained in tool results.
Web calls have no provider-side deduplication guarantee and can consume credits
again if their response was lost before a checkpoint.

The loop intentionally remains owned by this harness. Only filesystem adapters
and provider policy are shared through `cc-harness-support`; future experimental
harnesses are not forced into this loop's semantics.

Enable in the worker with `ENVIRONMENTS_ENABLED=true`, gateway URL/token settings,
and `FIRECRAWL_API_KEY` for web-enabled runs. Platform migration registers the
catalog row; a worker still must advertise the implementation. Nothing in package
construction provisions compute. Run tests from the Platform workspace; ignored
integration tests use isolated SQLx databases and local simulated gateways.
# Registered model capabilities

`environments_harness::supported_models()` returns provider IDs mapped to model-ID
arrays from the same compiled catalogs used by the harness. Platform exposes this
as `supported_models` in its harness catalog. The registration migration is checked
against this function in tests; catalog changes require refreshing registration
metadata. This does not list configured accounts or guarantee upstream availability.
