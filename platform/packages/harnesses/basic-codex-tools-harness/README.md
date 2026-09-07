# Basic Codex tools harness

`BasicCodexToolsHarness::new(llm, execution, publisher)` implements the shared
Harness interface with `exec_command`, `write_stdin`, freeform `apply_patch`, and
`view_image`. Uses ordinary Responses with OpenAI and ChatGPT, native assistant
replay, stable session cache keys, low verbosity, configured reasoning effort,
and sequential tool execution. Responses Lite and Fireworks are not enabled.

Configuration matches the Basic CC environment descriptor: `model`,
`reasoning_level`, `environment`, optional `account_id` and `system_prompt_append`.
Machine and snapshot-backed sandbox environments resolve once per session.
Private state is not copied to forks. The model catalog is explicitly restricted
to models with a reviewed tool/reasoning/image capability policy.

## Durability

The driver pins instructions, definitions, tool policy, provider options, model
operation identity, history revision, and current tool phase in a run checkpoint.
History is fully paginated; no automatic compaction or trimming is performed.
Every process start/input/patch mutation is prepared and saved before effects.
Every polling/patch step is saved before the next step. Session output cursors,
tool results and tool-index advancement commit atomically.

Session state under `basic-codex-tools-harness` contains `execution-target`,
`exec-allocator`, and individual `exec:<id>` entries. Numeric aliases are never
reused, including aliases exposed in inherited fork history. A process entry
contains its execution handle, supervisor generation, cwd, PTY mode, output
cursor, stable termination ID, and creation run. At most 32 entries are retained;
poll completed sessions to release them. Live processes survive successful runs
and worker drain. Failed runs clean up their processes before recording failure. Abort stops the active process and processes created by that
run; earlier unrelated sessions survive. Cleanup progress is checkpointed.

Supervisor restart/lost execution is reported, never silently restarted. Patch
failures may have partial effects and preserve structured execution details.
Transport ambiguity resumes saved operations. Gateway/supervisor retention
bounds recovery; this is not cross-restart exactly-once execution.

## Limits and images

Output collection: 64 KiB, at most 10,000 model tokens per result. Commands: 32 KiB;
stdin: 8 KiB. Patches: 16 KiB, affected file contents: 64 KiB, at most 64 operations.
Direct and shell-intercepted patches use the same saved policy. Tool checkpoints
are bounded at 512 KiB, assistant/user messages at 512 KiB, commits at 900 KiB.

Images are read up to 20 MiB and processed by tool-view-image. The publisher
uploads prepared bytes to immutable content-addressed GCS objects. Only image
URLs enter history. Objects are public and retained for history replay; do not
add bucket expiration rules while conversations still reference them.

`GcsImagePublisher` requires the gcloud CLI on the worker host and an authenticated
identity with object-create permission on the configured bucket. It obtains an
access token from `gcloud auth print-access-token` for each publication (honoring
gcloud impersonation), uploads through the GCS media API using
`ifGenerationMatch=0`, and never stores credentials in checkpoints or messages.
Bucket provisioning and IAM are deployment operations, not model tool actions.
The provisioned bucket is `agent-pane-codex-images-2c02a9f6`, in project
`project-2c02a9f6-1ff5-461d-ae0`, region `asia-south1`. To create another dedicated
bucket, run `platform/scripts/provision-codex-image-bucket.sh PROJECT BUCKET LOCATION`.
No inline fallback is used when publication fails.

## Worker

Set `BASIC_CODEX_TOOLS_ENABLED=true` and `BASIC_CODEX_IMAGE_BUCKET=<bucket-name>`,
plus the shared LLM/execution gateway configuration. Rebuild the shared worker and
apply Platform migrations. The catalog example prints the administrative body.
Deploy compatible execution gateways and supervisors with the patch/unified-exec
packages. Session deletion/expiry does not currently invoke a cleanup hook;
operators must terminate remaining host processes when disposing of a session.

## Verification

```sh
cargo test -p basic-codex-tools-harness -p tool-unified-exec -p tool-apply-patch
DATABASE_URL=postgresql://localhost/postgres cargo test -p basic-codex-tools-harness -- --include-ignored
cargo clippy -p basic-codex-tools-harness -p platform-worker --all-targets -- -D warnings
```
