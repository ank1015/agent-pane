# Durable site completion callbacks (Stage 5)

Callback registration commits in PostgreSQL with the session's new run and its
idempotency receipt. A database trigger creates one durable delivery when that run
becomes `completed`, `failed`, or `aborted`. It rolls back if the terminal runtime
transaction fails. Workers and the sites service do not need to be online at the
same time. The dashboard can be closed after the initial request is accepted.

The Platform process starts four dispatch loops when the existing Sites
integration is configured. No additional credentials or queues are required.
Migrations run through the existing Platform startup migration path. See
[SITES.md](SITES.md) for service configuration and project grants.

## SDK contract

```js
const accepted = await ctx.platform.sessions.start({
  harnessId, environmentId, model, accountId, prompt,
  onComplete: {
    path: '/callbacks/trial-finished',
    payload: { batchId, trial: 3 }
  }
}, { idempotencyKey: `batch:${batchId}:trial:3` });
// accepted.callback.subscriptionId is stable across successful receipt replays.
```

`onComplete` is also accepted in the input to `sessions.send` when the session is
idle and the operation creates a new follow-up run. It is rejected when steering
an active run. There is one completion subscription per new run. Registration
cannot be attached later, target a different site, specify an external URL, or
select another release. Payloads are JSON, limited to 16 KiB; paths are local
backend paths, limited to 2048 bytes, without queries/fragments.

Registration captures the **actual invoking release**, including if the caller
used the currently active release implicitly. The receipt replay returns the
original subscription even if a later retry comes from a different release.

The same site's backend receives a POST at the registered path:

```js
{
  id: 'stable-event-uuid',
  type: 'run.completed', // also run.failed and run.aborted
  subscriptionId: 'subscription-uuid',
  sessionId: 'session-uuid',
  runId: 'run-uuid',
  status: 'completed',
  finishedAt: '2026-09-06T12:00:00+00:00',
  payload: { batchId: 'eval-42', trial: 3 }
}
```

The trusted host supplies `ctx.invocation.source === 'callback'`, `eventId`, and
`subscriptionId`. Ordinary invocations have source `internal` and null callback
IDs. The project-facing invocation API rejects callback metadata supplied by the
caller. Check the trusted context before processing a callback endpoint; its URL
and JSON body alone are not proof that a run finished. Gateway credentials and
service scope remain outside JavaScript.

A callback is acknowledged only when the saved invocation succeeded **and** the
handler returned a 2xx status. Throwing, timing out, interruption, or returning a
non-2xx status causes retry. Return 2xx after durable application work is recorded
and required immediate Platform effects have been accepted. Reading messages and
metrics uses the existing SDK; pass `runId` when inspecting this specific run.

## Recovery and duplicate handling

Delivery is **at least once**, with no cross-site or cross-run ordering guarantee.
Handlers are short invocations (30-second deadline), not resumable JavaScript
stacks. On retry the endpoint begins again with the same event ID and payload.
SQLite writes and Platform requests are separate transactions.

Persist application intent in SQLite before calling Platform. Reuse a stable
logical idempotency key for every follow-on run, and save its returned IDs after
acceptance. If the process dies between acceptance and that save, the next
execution retrieves the original Platform receipt. Do not use the invocation ID
as a logical run key: invocation IDs can change between delivery attempts.

For a batch barrier, apply the following pattern:

1. In one SQLite transaction, insert the event with a unique event ID, record the
   trial's terminal state, and, when all ten trials are terminal, insert the next
   ten run intents with unique batch/stage/trial keys and immutable inputs.
2. Commit that transaction before calling any Platform API.
3. Drain unfinished intents using those keys with `sessions.start`, saving each
   returned session/run ID in SQLite.
4. On every retry, including an already-seen event, drain unfinished intents
   again before acknowledging. An early return for a duplicate event can strand
   work if the previous execution died after step 1.

Create application tables during live authoring with `ctx.sites.execute`; the
legacy release migration API remains supported. Backend `ctx.db` intentionally
cannot execute schema changes.
A minimal drain looks like this (table names are application-defined):

```js
if (ctx.invocation.source !== 'callback' ||
    ctx.invocation.eventId !== request.body.id) {
  return { status: 403, body: { error: 'Expected a completion callback' } };
}
await ctx.db.transaction(async tx => {
  // INSERT OR IGNORE the event, record the result, and persist any stage
  // transition and new run intents here. Inputs and keys must be stable.
});
for (const intent of await ctx.db.query(
  'SELECT logical_key, input_json FROM run_intents WHERE session_id IS NULL'
)) {
  const result = await ctx.platform.sessions.start(
    JSON.parse(intent.input_json), { idempotencyKey: intent.logical_key }
  );
  await ctx.db.execute(
    'UPDATE run_intents SET session_id = ?, run_id = ? WHERE logical_key = ?',
    [result.sessionId, result.runId, intent.logical_key]
  );
}
return { status: 200, body: { ok: true } };
```

Bound work per batch so it can make progress within the deadline; a retry can
continue draining partially completed intent. The initial browser request still
needs normal invocation/idempotency retries if its acceptance is uncertain.
Callbacks begin once a subscribed run terminates; this is not a general timer or
background-job facility. If failures exhaust delivery retries, an operator must
repair the cause and retry the delivery.

## Delivery state and operation

PostgreSQL stores subscriptions, immutable terminal event data, next retry time,
attempt counts, version, lease, and current/last invocation IDs. Claims use row
locks with `SKIP LOCKED`. The 60-second delivery lease outlives the 40-second
service HTTP timeout and 30-second handler deadline. A late dispatcher cannot
acknowledge or overwrite a replacement lease.

A transport failure or expired lease reuses the same invocation ID because the
request may already have executed. The sites service returns its durable receipt
rather than executing again. Only a confirmed terminal execution failure allocates
a new invocation ID on the next attempt. After a sites-service restart, a saved
`interrupted` receipt permits a fresh execution with the same logical event.

Retries use exponential backoff (about 2 seconds initially, capped at about
8.5 minutes, with small jitter). Twelve unsuccessful dispatch attempts put the
delivery in `failed`; repeated process crashes can recover an expired lease
before that failed outcome is recorded. No run or application effect is rolled
back by delivery failure. Requests accepted just before suspension/revocation can
finish; new claims pause while a site is suspended or project access is disabled.
Re-enabling access resumes pending delivery. Site deletion cancels pending work.

```js
await ctx.platform.callbacks.list({ limit: 20, after: previousPage.next_after });
await ctx.platform.callbacks.get(subscriptionId);
```

Lists default to 20 records (maximum 50), omit payload/event bodies, and use an
exclusive `next_after` subscription cursor. Individual inspection includes payload
and event data. Project-authenticated HTTP routes are also scoped to the selected site:

| Method | Path suffix under `/api/projects/{project}/sites/{site}` | Purpose |
| --- | --- | --- |
| GET | `/callbacks?limit=20&after=<subscription UUID>` | Bounded subscription/delivery list |
| GET | `/callbacks/{subscription}` | Waiting, paused, pending, delivering, delivered, failed, or cancelled status and delivery details |
| POST | `/callbacks/{subscription}/retry` | Retry a failed delivery with `{ "expectedVersion": <delivery.version> }` |

Manual retry preserves the event and pinned release, resets the attempt budget,
and rejects stale versions. A retained ambiguous invocation is reconciled before
any new execution. `delivery.last_error` is a sanitized category; use
`delivery.last_invocation_id` with the existing invocation inspection endpoint
for saved response, error code, and logs. There is no SDK method for changing
permissions, retry budgets, or delivery ownership.

## Releases and schema changes

Every subscription retains its release ID. Delivery always invokes that exact
immutable release, even after a new active release is published or rolled back.
The current sites service retains **all** immutable releases and source revisions;
there is no release garbage collector or release deletion API. Thus pending and
failed callbacks cannot lose their code through normal site lifecycle operations.
Any future collector must treat these references as retention roots, including
failed deliveries that operators may retry.

Release pinning does not pin a copy of SQLite. Every callback uses the site's
current database. While old callbacks remain, schema migrations must preserve
compatibility with their release's declared schema range. An incompatible release
is rejected rather than silently redirected to new code; restore compatibility
and retry. Suspended sites allow operators to perform such maintenance.

## Verification

`tests/sites.rs` runs the real sandboxed backend and Platform APIs against
disposable PostgreSQL and SQLite state with a mock provider catalog. It covers:

- Atomic registration and terminal-event rollback; completed/failed/aborted runs.
- Ten first-stage trials triggering ten next-stage trials with no browser calls,
  backend failure after run acceptance, service restarts, receipt replay, and
  actual duplicate callback execution without additional trials.
- Release pinning, competing dispatchers, expired leases and stale acknowledgements.
- A killed sites-service process after a SQLite write, then interrupted-receipt recovery.
- Authorization, validation, suspension/revocation, failed-delivery visibility,
  version-checked manual retry, follow-up registration, and deletion cancellation.

```sh
cargo build -p platform-sites-service
DATABASE_URL=postgresql:///postgres cargo test -p platform-server --test sites -- --ignored
```

The interruption test also uses the `sqlite3` CLI for a read-only readiness probe.
No provider credits or execution machines are used by these tests.
