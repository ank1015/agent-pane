# Shared Platform capabilities and Sites implementation phases

The Sites agent and deployed site backends will use the same Platform capability
vocabulary. Each has a trusted transport adapter: an agent is scoped by its run
and lease, while a backend is scoped by its project/site/release/invocation.
Client arguments never supply the authorization scope or service credentials.

Harnesses own provisioning. Platform validates declared configuration references
and stores explicit outputs; it does not assume a harness needs one environment,
allocate a sandbox from a session request, or inspect private harness state.

## Seven implementation phases

1. **Shared contracts and harness outputs (complete).** Typed capability
   requests/records, optional harness input/output declarations, session-frozen
   contracts, fenced immutable output publication, bounded scoped reads, and
   worker/backend adapters for output reads.
2. **Scoped session/run capabilities (complete).** Account discovery, harness start
   options and explicit session/run APIs use shared operations, preserving
   grants, receipts, immutable configuration and existing SDK compatibility.
   New Sites creation accepts harness-specific declared inputs; legacy adapters
   remain compatible. Session + initial run creation is atomic.
3. **Durable sandbox and command operations (complete, polling delivery).** Track project sandbox instances,
   expose lifecycle handles, wrap bash-minimal start/poll/cancel, and have basic
   harnesses publish their actual workspaces. A verifier uses the evaluated
   run's output, never a fresh copy of its original snapshot. Add command
   completion delivery when needed for unattended verification.
4. **Shared JavaScript SDK and generic code mode (complete).** Generate declarations/documentation,
   expose the same ctx.platform methods in both contexts, trace nested calls and
   persist individual effect identities. Interrupted cells do not automatically
   replay arbitrary JavaScript. Backend requests remain short; long work uses
   operation handles and continuations.
5. **Direct live Sites authoring (complete).** Two-file patches, atomic activation,
   scoped data/diagnostics, durable edit recovery, and user-only named code snapshots
   and restoration. No development sandbox, draft, working copy, or agent publishing.
   See [AUTHORING.md](apps/sites-service/AUTHORING.md).
6. **Sites harness.** Register the recoverable agent loop, research/browser tools,
   both SDKs, session-bound site selection (`siteId` or null), durable waits,
   steering/abort, and live editing. Multiple sessions may target the same site.
7. **Dashboard and end-to-end rollout.** Create/edit entry points, previews,
   live source/snapshot/operation visibility, evaluation → published workspace → verifier
   flows, and restart recovery across actual service boundaries.

## Shared contract surface

`packages/platform-runtime-contracts/src/capabilities.rs` is the version 1 type
vocabulary. The delivery column distinguishes implemented methods from future
contracts; types alone do not enable an HTTP route or method.
Existing record types are reused rather than duplicating sessions/history/runs.
New capability arguments use camelCase; existing Platform records retain snake_case.

| Method family | Requests/options and records | Delivery |
| --- | --- | --- |
| environments.list/get | PageOptions, Environment | Implemented, paginated |
| accounts.list | PageOptions, Account (metadata only) | Implemented |
| harnesses.list/get/startOptions | Harness, StartOptions, HarnessContract | Implemented |
| sessions.create/list/get/messages/stats | CreateSession, SessionCreated, Session, MessageOptions, Statistics | Implemented; legacy start/send/metrics remain compatible |
| runs.create/list/get/steer/abort/stats | CreateRun, RunCreated, SteerRun, AbortRun, Run, Statistics | Implemented |
| runs.outputs | OutputOptions, RunOutputsPage | Implemented for worker and site SDK |
| sandboxes.createFromSnapshot/get/terminate | CreateSandboxFromSnapshot, Sandbox | Implemented, durable lifecycle |
| execution.bash/get/output/cancel | Bash, Execution, ExecutionOutput | Implemented, durable polling |

All mutations use MutationOptions/idempotencyKey (worker APIs use Command<T>).
Resource lookup methods take the selected resource ID; list/get/cancel methods
need no distinct request wrapper beyond IDs and shared options. A caller-selected
accountId is canonical at session creation; conflicting account configuration
is rejected and the caller's configurable-field policy is enforced.
CreateRun cannot change account/configuration. Steering targets an exact live
run and acknowledges accepted input, not immediate model consumption.
CompletionCallback is meaningful only with a valid backend callback scope and
a newly created run; it does not enable callbacks for arbitrary callers.

Statistics distinguish absent metrics from zero and include coverage counts.
Cost represents recorded assistant usage, not a gateway billing receipt.
runWallSeconds sums started-run wall time including waits, excluding queue time;
forked history and individual-run metrics retain their different scopes.
The existing `sessions.metrics` wire shape is unchanged in this phase.

## Scoped session/run SDK (phase 2)

Both adapters dispatch to the same operations in `apps/server/src/runtime/capabilities.rs`.
The worker endpoint is `POST /internal/runs/{source}/capabilities`, with the same
worker bearer token, X-Worker-ID and X-Lease-Epoch headers as other RunClient calls.
The body is `{method, args}`. Mutation args are `{input, options: {idempotencyKey}}`;
read args contain the selected resource ID and optional `options`. Unknown fields,
including attempted scope overrides, are rejected. No new public QueryClient
routes are introduced. In Rust, `run.platform()` exposes typed equivalents:
`list_environments`, `get_environment`, `list_accounts`, `list_harnesses`,
`get_harness`, `start_options`, `create_session`, `list_sessions`, `get_session`,
`messages`, `session_stats`, `create_run`, `list_runs`, `get_run`, `steer_run`,
`abort_run`, `run_stats`, and `run_outputs`. Mutations take persisted `Command<T>`.

Sites use their existing authenticated parent bridge. JavaScript signatures:

```ts
ctx.platform.environments.list({limit, cursor})
ctx.platform.environments.get(environmentId)
ctx.platform.accounts.list({limit, cursor})
ctx.platform.harnesses.list({limit, cursor})
ctx.platform.harnesses.get(harnessId)
ctx.platform.harnesses.startOptions(harnessId)
ctx.platform.sessions.create(input, {idempotencyKey})
ctx.platform.sessions.list({limit, cursor})
ctx.platform.sessions.get(sessionId)
ctx.platform.sessions.messages(sessionId, {limit, afterRevision, runId})
ctx.platform.sessions.stats(sessionId)
ctx.platform.runs.create(input, {idempotencyKey})
ctx.platform.runs.list(sessionId, {limit, cursor})
ctx.platform.runs.get(runId)
ctx.platform.runs.steer({runId, input: userMessage}, {idempotencyKey})
ctx.platform.runs.abort({runId, reason}, {idempotencyKey})
ctx.platform.runs.stats(runId)
ctx.platform.runs.outputs(runId, {limit, afterSequence})
```

A new session accepts `{harnessId, accountId, title?, config, initialInput?, onComplete?}`.
`config` is a harness-specific merge patch over catalog defaults (including the
existing null-removal semantics), validated against config_schema and declared
environment references. accountId is always written to `config.account_id`; a
conflicting caller value is rejected. Provider/model compatibility and active
account status are checked at admission. Account inventories expose only account
ID, name, provider and status, never credential metadata or provider settings.

```ts
const created = await ctx.platform.sessions.create({
  harnessId: 'evaluation-harness',
  accountId,
  config: {model, training: {environmentId}, evaluationEnvironments},
  initialInput: {
    role: 'user', id: inputId, timestamp: inputTimestamp,
    content: [{type: 'text', content: 'Run the evaluation'}],
  },
  onComplete: {path: '/evaluation-finished', payload: {jobId}},
}, {idempotencyKey: operationKey});
```

The example assumes a registered harness with that schema/contract and permitted
fields. Save the full input, including message ID/timestamp, with its operation
key before dispatch. Omitting initialInput creates an idle session. Including it
atomically creates the session, first run, inbox input and optional callback.
Callbacks require a new run and a site invocation; leased agents cannot register
site callbacks. `sessions.create` returns `{session, run, input, callback}`, with
nullable run/input/callback. `runs.create` accepts `{sessionId, expectedRevision,
input, onComplete?}` and returns `{run, input, callback}` (additional session data
may be present). Both run controls return `{run, input}` plus additive fields.
They acknowledge durable acceptance; they do not wait for the model or executor.

A run always uses its session's frozen account/configuration. Starting a new run
checks the current revision and absence of an active run under the session lock.
Steer/abort operate on the exact requested live run and cannot affect a later
replacement run. Abort remains cooperative. Session get/list retain active_run,
latest_run and status; messages are a separate paginated history read. Pending
inputs are not reported as consumed transcript messages.

### Caller authority and configuration grants

Sites retain site/project access, enabled project harnesses, site harness grants
and site account grants. Session/run history reads cover the current project;
grants restrict discovery and mutations, not historical visibility. New site
access grants can use:

```json
{
  "id": "evaluation-harness",
  "environmentMode": "declared",
  "configurableFields": ["model", "training", "evaluationEnvironments"]
}
```

`environmentMode` defaults to `declared`; `configurableFields` defaults to an empty
list. It is an allowlist of at most 64 **top-level** configuration keys, including
nested values under those keys. Schema validation still applies. account_id is
server-controlled and cannot appear in that allowlist. Defaults remain available
without being caller-editable. Declarations support zero/one/multiple UUID refs;
no single environment is inferred or provisioned. Existing single/none grants
and `sessions.start/startOptions/send/stop/metrics` remain supported. Their legacy
configuration adapter retains its original restrictions; newly declared harnesses
use sessions.create and harnesses.startOptions. Changing a grant does not rewrite
old sessions, and starting/steering them rechecks their frozen references.

Agents are trusted harness callers with the project authority of existing worker
coordination APIs. They may choose any globally and project-enabled harness and
active gateway account; site-specific grant lists do not apply. Their source lease
must remain valid at commit. Created runs carry parent_run_id, steering/abort
inputs carry source_run_id, and a source event records each operation. Site
mutations retain invocation attribution even for session-only creation.

Receipts are scoped by caller identity, method and logical key. Same-key retries
return original results before mutable account/environment discovery; changed
arguments conflict. Original authenticated worker issuers and current replacement
owners can recover accepted receipts. Stale owners cannot make new mutations or
read live capability state. Sites must retain current site/project access to read
receipts. Removing an account blocks new execution/steering, while an authorized
abort can still stop work using a removed account. Provider discovery happens
outside database locks, and admission rechecks grants, catalog policy and leases.

### Pagination and statistics

Inventory/session/run lists use `{items, next_cursor}` (default 50, maximum 200).
Inventory order is by ID; session order is most recent activity first; run order
is creation order within the selected session. Cursors are scoped to collections
and selected resources; inventories with caller-dependent visibility also bind
the caller. These are live views, not frozen snapshots. Messages use the exclusive
revision cursor and outputs use the exclusive sequence cursor. Pages also have
a 192 KiB record budget; always follow the returned cursor. An individual oversized
record returns an explicit error, rather than an empty page that skips it.

Stats aggregate all canonical assistant messages, independent of history page
size. Each metric is `{total, contributingMessages}`; missing totals remain null,
recorded zero remains zero. Session metrics include inherited history; run metrics
include only that run's session-message links. Durations sum started-run wall time
and include waiting, not queue time. Existing sessions.metrics retains its wire
shape and uses the same aggregation. Capability arguments are capped at 192 KiB
and complete responses at 240 KiB to fit the existing backend transport.

## Harness declarations

Catalog admin PUT/PATCH accepts `harness_contract`. Omitted PUT fields preserve
existing declarations, so older catalog writers cannot accidentally erase them.
PATCH rejects null; `{}` explicitly declares no inputs/outputs. Existing harnesses
and sessions default to an empty contract. No existing harness gains output
publication or provisioning behavior automatically.

```json
{
  "environment_inputs": [
    {"config_pointer": "/training/environmentId", "cardinality": "single", "required": true},
    {"config_pointer": "/evaluationEnvironments", "cardinality": "multiple"}
  ],
  "outputs": {
    "evaluationWorkspace": {
      "kind": "execution_workspace",
      "description": "Files modified by the evaluated run"
    },
    "report": {
      "kind": "json",
      "value_schema": {
        "type": "object",
        "required": ["score"],
        "properties": {"score": {"type": "number"}},
        "additionalProperties": false
      }
    }
  }
}
```

Pointers are exact RFC 6901 pointers into resolved session config; `~0`/`~1`
escapes and array indexes work. Single selects a UUID string; multiple selects
an array of at most 64 UUID strings. Missing optional fields are allowed; an
explicit null or wrong shape is invalid. A required multiple field may be empty;
use config_schema minItems if a nonempty array is required. References are checked
against the session's project in the admission transaction for ordinary creation,
forks, child sessions, and Sites starts. No reference is rewritten or provisioned.
The existing config_schema still validates the full configuration.

Contracts are capped at 64 KiB, 64 input declarations, 64 distinct referenced
environments and 64 named output declarations. Output value schemas are local
draft 2020-12 schemas, checked at registration and used at publication.

The catalog contract is copied into every newly created session by a database
trigger and cannot be changed afterward. Follow-ups use that session contract.
A fork is a new session and gets the current target harness contract; its new
configuration is validated again. Outputs themselves are not copied by forks.
Legacy sessions keep the empty contract rather than acquiring new obligations.

## Output publication and reads

Worker publication:

```http
POST /internal/runs/{run}/outputs
Authorization: Bearer <worker token>
X-Worker-ID: <worker UUID>
X-Lease-Epoch: <current epoch>
Idempotency-Key: <saved logical operation key>
```

```json
{
  "name": "evaluationWorkspace",
  "output": {
    "kind": "execution_workspace",
    "value": {
      "host_id": "<UUID>",
      "workspace_root": "/home/user",
      "path": "task",
      "environment_id": null
    }
  }
}
```

`RunClient::publish_output(&Command<PublishRunOutput>)` returns a RunOutput with
server-derived project/session/run IDs, sequence and timestamp. Values are at
most 64 KiB including request framing. Names are ASCII letters followed by
letters/digits/underscore/dot/hyphen, up to 128 bytes. Names must be declared by
the frozen session contract and have the declared kind/value schema.

Outputs are immutable per (run, name). An identical new-key publication returns
the existing record under a live lease; changed content returns
`RUN_OUTPUT_IMMUTABLE`. Reusing an operation key with a changed request returns
`IDEMPOTENCY_KEY_CONFLICT`. Receipt and output/event writes commit together.
The original authenticated issuer or current replacement owner may replay an
accepted receipt; a stale owner cannot publish anything new. Publication does
not renew a lease or advance the session transcript revision. Output values are
not inserted into checkpoints, messages or events; the event contains name and
sequence only. Persist publication commands before sending them.

Publish workspaces when ready and separate reports/artifacts as they become
available. A terminal run cannot add new outputs, but readers can inspect its
existing ones, including outputs from a failed/aborted run. Output existence is
not proof that the run succeeded. Every output is optional; zero outputs is normal.

Supported payloads:

- `execution_workspace`: host UUID, native absolute workspace_root and portable
  root-relative path. Optional environment_id is checked for project membership.
  This is a harness assertion about a target. It neither attests that the host
  is live nor grants command access. Remote operations authorize/resolve that host on
  use and track instance lifetimes. No global primary workspace is assumed.
- `json`: any bounded JSON value, additionally checked by value_schema.
- `artifact`: artifact_id, lowercase sha256, JS-safe size_bytes, media_type.
  It describes immutable bytes without a filesystem path or bearer URL. Artifact
  storage/resolution and integrity verification are later capabilities; phase 1
  does not claim that those bytes exist or provide a download/authorization path.

Worker reads use `RunClient::run_outputs(target_run_id, &RunOutputsQuery)`:
`GET /internal/runs/{source}/outputs/{target}?after_sequence=...&limit=...`.
The source lease must be live and both runs must belong to its project. The
target may be terminal. Unlike older public history reads, this route is never
exposed as an unauthenticated QueryClient method.

Site backends use `ctx.platform.runs.outputs(runId, {afterSequence, limit})`.
Current site/project access checks apply, and cross-project target runs return
not found. Guest scope overrides are rejected.

Both return `{items, next_after_sequence}` in publication order. Default page
size is 20, maximum 50; a 192 KiB record budget may shorten a page further to fit
the backend's 256 KiB bridge response limit. Follow the returned exclusive cursor.
An empty run returns an empty page, while a nonexistent/foreign run is not found.
For a live run, callers can poll after their last observed sequence for new
outputs even after reaching the current end. No implicit history download occurs.

## Durable sandbox and command SDK (phase 3)

Both callers use `apps/server/src/runtime/remote_operations.rs`. The server runs
a separately leased reconciler; JavaScript IPC/HTTP requests only accept durable
intent or read saved records. Embedders must configure `with_execution(client)`
and start `spawn_remote_reconciler()` once per server. Multiple servers can run
it concurrently. The normal server does both using its existing gateway settings.

```ts
ctx.platform.sandboxes.createFromSnapshot(
  {environmentId, name, timeoutSeconds}, {idempotencyKey})
ctx.platform.sandboxes.get(sandboxId)
ctx.platform.sandboxes.terminate({sandboxId}, {idempotencyKey})
ctx.platform.execution.bash(
  {hostId, command, workdir, timeoutMs}, {idempotencyKey})
ctx.platform.execution.get(executionId)
ctx.platform.execution.output(executionId, {cursor, limitBytes})
ctx.platform.execution.cancel({executionId}, {idempotencyKey})
```

Rust `run.platform()` equivalents are `create_sandbox`, `get_sandbox`,
`terminate_sandbox`, `bash`, `get_execution`, `execution_output`, and
`cancel_execution`. Mutations use persisted `Command<T>` keys. Replaying a
mutation returns its original acceptance snapshot; use `get` for current state.

Site callers require the explicit `executionEnabled: true` project-access grant,
in addition to ordinary live site/invocation authorization. It defaults to false,
including on a grant replacement that omits the field. Agent callers require a
live project-scoped lease. Accepted work continues if that lease ends or the
backend invocation finishes; revoking a grant does not undo accepted effects.
Attribution retains either the source run or the site and invocation.

Commands can target a machine referenced by the authenticated project or a
tracked, ready, unexpired sandbox in that project. `workdir` is absolute and must
fall within the authorized environment's root plus relative path. Preparation
also resolves that root against the host's actual descriptor. This checks the
starting workspace; arbitrary shell commands retain the host OS user's permissions,
so it is not a filesystem jail. A raw output reference supplies no authorization.

Sandbox creation accepts only an authorized snapshot environment and returns a
`provisioning` record before contacting the gateway. Poll until `ready`; its
`workspace` then includes the actual `host_id` and a `sandbox_id` lifecycle handle.
The saved native root must be advertised by the restored host. Timeout defaults
to 3,600 seconds and is bounded to 1–86,400 seconds from acceptance. Expiry revokes
new command admission immediately and requests gateway deletion. `terminating`
or `terminationRequested` does not mean the host has been deleted; only
`terminationConfirmed` records observed deletion (or cancellation before any
possible submission). Failures remain inspectable while cleanup retries.

Execution returns a `pending` handle before dispatch. Timeout defaults to
120,000 ms, maximum 1,800,000 ms; commands are limited to 64 KiB. The supervisor
enforces process timeout, with server cancellation after the acceptance deadline
plus a recovery allowance. The reconciler commits the prepared start, execution
ID, cancellation ID and supervisor generation before any possible dispatch.
Uncertain starts replay the same request. A supervisor generation change reports
`lost`; it never starts a replacement command implicitly. Database processor
epochs fence late replies after server takeover.

Output and its remote journal cursor commit together. The service retains a
1 MiB UTF-8 text prefix (stdout/stderr interleaved in journal order), drains the
remaining output, counts original bytes, and reports `truncated`. Pages default
to 32 KiB, maximum 64 KiB; cursors are bound to the execution and UTF-8 boundaries.
`complete` means reconciliation reached a terminal state; continue paging while
`nextCursor` is non-null to drain the retained text. A live empty page returns
a cursor so callers can poll again. `get` distinguishes completion, failure,
cancellation and loss; a lost execution can expose partial output only.

`cancel` records intent. `cancellationConfirmed` is set only after observing a
terminal process (ordinary exit may win the race), or when no start was possible.
An ambiguous start is cancelled/drained by its saved identity without submitting
a new process merely to cancel it. If the supervisor cannot establish that
identity's outcome, cancellation remains explicitly unconfirmed. Transport errors
and a termination acknowledgement alone do not prove termination.

The basic CC and Codex harnesses publish `workspace` after resolving their actual
target. Each run has its own output; multiple runs may share a session sandbox.
Their publication plan survives activation loss in private session state. When
a project environment matches, the leased `RunClient::bind_workspace` bridge
validates host kind, snapshot source, project/session metadata and existing
associations, then adopts the already-created sandbox without allocating one.
The returned optional `sandbox_id` lets consumers inspect or terminate it. Basic
harness provisioning decisions remain unchanged. Legacy descriptors with no
matching project environment publish reference-only outputs; old sessions whose
frozen contracts omit `workspace` continue without publication.

A verifier reads the evaluated run's status and named outputs, polls the selected
sandbox if applicable, and submits a command using that output's host and workspace.
It does not create a fresh sandbox from the original snapshot. Preserve the
execution handle across backend requests and poll it in subsequent invocations.
Command completion callbacks and durable JavaScript continuations remain deferred;
existing run completion callbacks are unchanged.

Migrations `20260908030000` and `20260908040000` add durable operations, the execution
grant and basic harness output declarations. Frozen existing session contracts
remain unchanged. Apply them through the normal server migration pipeline before
enabling the new SDK methods.

## Shared JavaScript SDK and code mode

Phase 4 is implemented in three reusable packages:
[generic code mode](packages/tools/code-mode/README.md),
[the shared JavaScript SDK](packages/platform-javascript-sdk/README.md), and
[the leased agent adapter](packages/platform-agent-code-mode/README.md).
The generic registry accepts arbitrary JSON-compatible function/freeform tools;
Platform is an adapter and SDK extension. Both guests use the same common factory.
Cell source is never automatically replayed. Each nested call is journaled before
dispatch; explicit individual-call reconciliation retains its original identity.
The worker hosts the isolated guest entry point for future harness integrations.
This phase does not change the basic harnesses' model-facing tool menus; the Sites
harness loop is phase 6. Linux requires `bwrap`; unsupported isolation fails closed.

## Verification

From `platform/`, with a local PostgreSQL role allowed to create test databases:

```sh
cargo test -p platform-runtime-contracts -p platform-runtime-client
DATABASE_URL=postgresql:///postgres cargo test -p platform-server --test run_outputs -- --include-ignored
cargo build -p platform-sites-service -p tool-code-mode --bins
DATABASE_URL=postgresql:///postgres cargo test -p platform-server --test sites -- --include-ignored --test-threads=1
DATABASE_URL=postgresql:///postgres cargo test -p basic-cc-tools-harness -- --include-ignored
DATABASE_URL=postgresql:///postgres cargo test -p basic-codex-tools-harness -- --include-ignored
cargo test -p tool-bash-minimal
cargo test -p tool-code-mode -p platform-javascript-sdk -p platform-agent-code-mode
DATABASE_URL=postgresql:///postgres cargo test -p platform-server --test sites code_mode_ -- --include-ignored
```

These tests use disposable databases and local services; no development database,
LLM provider or execution host is required. The migration is embedded by the
existing server migration pipeline; deploy it before clients start publishing.
Remote tests use real local shell supervisors behind a fault-injecting gateway.
They cover evaluated sandbox files, project bindings, expiry, lost acknowledgements,
agent and server takeover, generation changes, bounded cursors and honest
cancellation. Live E2B provisioning and the separately gated Windows-host test
require external infrastructure and are not exercised by the local suite.

## Step 6: Sites harness

`packages/harnesses/sites` implements the registered `sites` harness in the shared
worker. The leased loop freezes its instructions/provider options/tools and saves
model operations and tool plans before effects. Code mode exposes the shared
Platform and live Sites SDKs, optional Firecrawl search/scrape, and an optional
Chromium frontend probe. Outer inspection/reconciliation tools recover accepted
operations without replaying JavaScript. Durable wait commits release worker
capacity and wake on run completion, timers or input.

Configuration uses `siteId` (existing or null); binding is lazy and shared across
that session's runs. No development machine is provisioned for authoring. Multiple
sessions can edit the same main site. Snapshots/rollback remain user-only.
Registration is project opt-in with no environment inputs and an optional `site`
JSON output. Existing project grants still constrain Platform and Sites calls.
The full natural-language prompt lives in `src/system_prompt.md`; generated SDK
declarations are appended at activation and saved in the checkpoint.

The optional browser probe validates current frontend DOM and runtime errors in
isolated Chromium, with backend access tested separately through `sites.invoke`.
It does not claim authenticated end-to-end coverage or visual screenshot review.
