# Sites integration (Stage 4)

Platform owns logical sites and authorization. The sites service owns releases,
SQLite and isolated backend invocations. [Stage 5 completion callbacks](CALLBACKS.md)
continue work after a run terminates. The [dashboard Sites table and viewer](../dashboard/SITES.md) are available.
Agent authoring and preview-editing workflows remain deferred.

## Configure the two services

Platform (all three variables together; omission disables the new routes):

```dotenv
PLATFORM_SITES_SERVICE_URL=http://127.0.0.1:3102/
PLATFORM_SITES_SERVICE_TOKEN=<the sites service SITES_API_TOKEN>
PLATFORM_SITES_CAPABILITY_TOKEN=<a separate random service credential>
```

Sites service (omit both to retain standalone Stage 3 execution):

```dotenv
SITES_PLATFORM_URL=http://127.0.0.1:3100/
SITES_PLATFORM_CAPABILITY_TOKEN=<Platform's PLATFORM_SITES_CAPABILITY_TOKEN>
```

Tokens are 32–256 visible ASCII characters. URLs must be HTTPS origins, with HTTP
allowed only on loopback. Clients do not follow redirects; requests and responses
are bounded. Credentials stay in the trusted processes, not JavaScript, source,
run config, invocation records or browser URLs. The backend child still has no
network access. SDK requests travel over IPC to the parent and then to Platform.

These new APIs have their own authorization boundary. Existing dashboard `/api`
routes remain trusted local-development APIs; this is not a retrofitted login
system for the whole dashboard. Keep legacy routes, worker/admin routes and the
sites-service listener private. If deploying behind an external proxy, expose
only the explicitly authenticated site application routes, not the legacy API.
Project credentials are a v1 trusted-client mechanism, not end-user login or
browser token distribution; the local dashboard uses a [server-only bridge](../dashboard/SITES.md).

## Project access and grants

A trusted operator uses `PLATFORM_RUNTIME_ADMIN_TOKEN` to configure access:

```http
PUT /internal/site-access/<project UUID>
Authorization: Bearer <runtime admin token>
Content-Type: application/json
```

```json
{
  "token": "<separate random project credential>",
  "enabled": true,
  "harnesses": [
    { "id": "basic-cc-tools-harness", "environmentMode": "single" }
  ],
  "accountIds": ["<LLM account UUID>"]
}
```

This atomically replaces the project credential and complete grant lists. Tokens
are stored hashed and never returned. Set `enabled: false` to revoke new access.
Rotation invalidates the previous project credential. Already accepted effects
are not undone. A harness must also be globally enabled and enabled/required in
the project. No grants are seeded automatically, and backend SDK code cannot edit
grants. The operator is explicitly trusting the granted harness implementation.

`single` maps a selected project environment to the existing immutable
`environment` config descriptor (absolute native `workspace_root`, relative
`path`, and machine or snapshot ID). `none` does not accept an environment. These
are explicit adapters for v1, not a general multi-machine config language.
Environment/site builders should not be granted unless intentionally authorized.
LLM account grants reference gateway accounts; discovery/start additionally
requires active status and supported provider/model. There is no fallback to an
ungranted default account, or credential/account administration through the SDK.

## Project-facing HTTP API

All routes below require the credential for exactly the path's project.
Unknown JSON fields are rejected; all responses are no-store and carry request IDs.

| Method | Path | Behavior |
| --- | --- | --- |
| GET | `/api/projects/{project}/sites` | List nondeleted sites (v1 maximum 100; exceeding the bound errors) |
| PUT | `/api/projects/{project}/sites/{site}` | Provision using a stable caller-selected site UUID and `{ "name": "Evals" }` |
| GET | `/api/projects/{project}/sites/{site}` | Logical record and reconciled physical state |
| PATCH | `/api/projects/{project}/sites/{site}` | Rename or set `status: ready/suspended` |
| DELETE | `/api/projects/{project}/sites/{site}` | Tombstone and suspend; retain code and data |
| POST | `/api/projects/{project}/sites/{site}/content-access` | `{ "release_id": "...", "ttl_seconds": 900 }` → existing scoped content grant |
| POST | `/api/projects/{project}/sites/{site}/invocations` | Forward the existing Stage 3 invocation contract |
| GET | `/api/projects/{project}/sites/{site}/invocations/{id}` | Inspect the saved invocation |

Provisioning persists intent before contacting the sites service. Retrying PUT
with the same site/project/name is safe. A background reconciler walks records
in pages of 100 every ten seconds and retries physical provisioning/lifecycle
alignment. An upstream failure does not erase the logical site. PATCH uses last
accepted write order; after ambiguous responses, GET current intent before
reissuing an older change. DELETE is logical, idempotent, and immediately blocks
new Platform site calls; physical suspension may finish through reconciliation.

Forwarding saves the normalized request before the service call. Reuse the same
invocation UUID and body after uncertainty. Changed content conflicts, and the
sites service preserves its resolved release and saved terminal result. Backend
errors remain nested in the invocation record; outer HTTP 200 is not a claim
that the application's operation succeeded. No automatic invocation rerun occurs.

Only the distinct capability credential authenticates `POST
/internal/site-capabilities`. The sites-service parent supplies site, project,
release and invocation IDs; Platform checks the immutable site/project binding,
current access, and that the invocation was registered through forwarding. This
is a trusted service boundary, not an endpoint for guest/browser requests.

## Backend SDK

See `../sites-service/src/sdk.d.ts`. SDK arguments use camelCase; returned Platform
records retain existing snake_case fields. All methods return promises. Methods:

```ts
ctx.platform.environments.list()                 // {items}, maximum 200
ctx.platform.environments.get(environmentId)
ctx.platform.harnesses.list()                    // granted + enabled only
ctx.platform.harnesses.get(harnessId)
ctx.platform.sessions.startOptions(harnessId)
ctx.platform.sessions.start(input, {idempotencyKey})
ctx.platform.sessions.list({limit, cursor})
ctx.platform.sessions.get(sessionId)
ctx.platform.sessions.messages(sessionId, {limit, afterRevision, runId})
ctx.platform.sessions.metrics(sessionId, {runId})
ctx.platform.sessions.send(sessionId, input, {idempotencyKey})
ctx.platform.sessions.stop(sessionId, {expectedRunId, reason, idempotencyKey})
```

`start` accepts `harnessId`, optional `environmentId`/`title`, `prompt`, required
`model: {provider, id}` and `accountId`, plus optional `options: {reasoningLevel,
webSearchEnabled}`. It creates the session and first run atomically and returns
`{sessionId, runId, session, run, input}`. No raw config overrides, machine IDs,
forks or empty-session creation are accepted. Optional `onComplete` registers a
[durable completion callback](CALLBACKS.md) with the new run. The selected environment
is read/locked inside the same transaction and copied into immutable config.
Workers, not sites-service, allocate sandbox instances and execute harnesses.
Independent sandbox trials use independent sessions; machine environments still
share their underlying workspace.

`startOptions` gives permitted active account/model choices, environment
requirement, reasoning controls and web-search support. It does not hold a
transaction while calling the LLM gateway. Admission rechecks local grants and
harness model declarations in its mutation transaction. Account state at the
gateway can change after discovery; the harness/gateway handles such execution
failures normally. No online-worker guarantee is implied by catalog availability.

`send` takes `{prompt, expectedRevision, expectedRunId}`. Use the observed active
run ID, or explicit null if idle. It atomically checks that observation, then
steers that run or starts a follow-up using the session's immutable configuration.
A race returns conflict, never silently retargets. Existing single-environment
sessions must still match a project environment and their account must remain
granted. Stop targets an exact observed run and requests cooperative abort; it
does not immediately kill execution or cascade to children.

Successful start/send/stop receipts and site/invocation attribution are committed
with their runtime effects. Keys are scoped to the site and operation, not to an
invocation. Persist logical keys in application state and reuse the exact inputs
across retries. Successful receipt replay precedes mutable resource discovery,
but still requires current project/site access. No SQLite transaction spans a
Platform effect; both the SDK and trusted host reject Platform calls while a site
SQL transaction is open. The host's invocation deadline bounds network calls.

Session reads cover the current project, including sessions created outside the
site. Cross-project session, run-filter and environment IDs are rejected. List
and get include active/latest run summaries and an associated status. Messages
use the existing canonical history and exclusive revision cursor; pending inputs
are returned by accepted send/start responses until committed to history.

Metrics aggregate all canonical assistant messages in the selected session or
run, independent of message pagination. Session-history metrics include inherited
messages from history forks, exclude child-session messages, and describe recorded
usage rather than gateway billing. Cost and token fields are null when unknown;
`*_messages` counts expose coverage for each metric. `completeUsage` requires all
reported usage fields on every assistant message. Cache hit rate is a 0–1 ratio
when input/cache counts are available. `runWallSeconds` sums run wall durations
(including waits); it is not CPU time and includes elapsed time for live runs.
Metrics are a current aggregate, not a cross-service transactional snapshot.

No environment/harness mutations, provider administration, approvals, arbitrary
HTTP, raw worker APIs or SSE subscriptions are exposed. Completion callbacks
and their inspection SDK are documented in [CALLBACKS.md](CALLBACKS.md). Unknown
fields/methods fail explicitly.

## Verification

```sh
# From platform/, PostgreSQL role must have CREATEDB.
cargo build -p platform-sites-service
DATABASE_URL=postgresql:///postgres cargo test -p platform-server --test sites -- --ignored
cargo test -p platform-sites-service
cargo clippy -p platform-server -p platform-sites-service --all-targets -- -D warnings
```

The integration test uses disposable PostgreSQL/filesystem state, a local mock
LLM account inventory and the real sites-service binary and OS-sandboxed QuickJS
child. It starts actual Platform run records without spending provider credits
or provisioning execution hosts. Existing worker tests cover run execution.
