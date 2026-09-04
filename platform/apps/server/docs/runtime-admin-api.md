# Runtime administration and background work

These routes belong to Platform, backed by its existing PostgreSQL database.
They do not deploy harness code, execute agent loops, or call either gateway.

## Authentication

Set `PLATFORM_RUNTIME_ADMIN_TOKEN` to a separate random 32–256 character printable
ASCII credential (no spaces). Pass it as `Authorization: Bearer <token>`.
Missing configuration disables these endpoints. Invalid nonempty configuration
or reusing the registration credential fails server startup. Bootstrap and
per-worker credentials do not authorize administration. Keep this API on a trusted
network and use TLS outside localhost. Responses use `Cache-Control: no-store`.

## Harness catalogue

`PUT /internal/harnesses/{id}` creates or replaces a catalogue entry and returns
the stored entry with HTTP 200. The id must match `[a-z][a-z0-9_-]{0,127}`.

```json
{
  "name": "Experimental harness",
  "description": "A first-party worker implementation",
  "default_config": { "model": "my-model" },
  "config_schema": {
    "type": "object",
    "required": ["model"],
    "properties": { "model": { "type": "string" } }
  },
  "enabled": true
}
```

`name` is required, trimmed, and 1–128 characters. On PUT, omitted description
and schema become null; defaults become `{}`; enabled becomes `true`.
Creation time is preserved on replacement. Description is limited to 8192
characters; defaults and schema are each limited to 64 KiB. Unknown fields fail.

`PATCH /internal/harnesses/{id}` changes only supplied fields and returns HTTP
200. A missing entry returns 404. It accepts the same fields, requires at least
one, and serializes against concurrent updates. `description: null` and
`config_schema: null` clear these fields. Other fields cannot be null.
`default_config` and `config_schema` replace their whole objects, not a recursive
merge. Use PATCH for enable/disable so other metadata is preserved.

Schemas are validated as draft 2020-12, with remote/file retrieval disabled.
Defaults may be partial: the complete configuration is validated when starting a
run after applying overrides. Existing runs retain their resolved configuration.
Disabling a harness prevents new sessions/runs but does not stop or strand its
existing work. Catalogue registration does not install code: workers must ship
the matching implementation and advertise its id.

## Fleet inspection

`GET /internal/workers` returns `{ "items": [...], "next_cursor": null }`.

- `limit`: 1–200, default 50.
- `cursor`: opaque cursor returned by this endpoint.
- `status`: optional `accepting`, `draining`, or `offline`.
- `harness_id`: optional supported-harness filter.

Ordering is `started_at DESC, id DESC`. A cursor is bound to the filters used to
produce it. Heartbeats do not reorder workers. These are live observations, not
a frozen fleet snapshot across pages.

`GET /internal/workers/{id}` returns one worker or 404. Both endpoints include
the worker metadata plus:

- `active_assignments`: count of running assignments with unexpired leases.
- `available_capacity`: remaining declared capacity, floored at zero; zero for
  draining/offline workers.
- `stale`: last heartbeat/activity is older than 120 seconds.

Credential hashes and raw tokens are never returned. These observations do not
reserve capacity; the atomic claim operation is authoritative. The existing PUT
registration and PATCH self-management routes on the same path retain their
separate bootstrap/worker authentication.

## Background responsibilities

Every Platform process starts one reconciler and one PostgreSQL notification
listener. No leader election or external broker is required. The reconciler runs
once per second without overlapping itself; replicas coordinate with row locks
and `SKIP LOCKED`.

1. **Expired leases:** process up to 100 runs per pass. Recheck under lock, return
   the run to ready, clear ownership, append the lease-expired event, and publish
   a wake hint atomically. Preserve checkpoints, inputs, config, and abort intent.
   The next claim increments the lease epoch, fencing the previous owner.
2. **Durable waits:** process up to 100 eligible runs per pass. Resolve any/all
   dependencies on terminal child runs, persisted inputs, timers, and trusted
   `operation_result` inputs; resolve deadlines and wake eligible runs. A wait
   stays inside the same run. Operations are not polled or retried by Platform;
   the harness/gateway integration supplies durable results and owns recovery.
3. **Liveness:** mark up to 100 workers offline after 120 seconds of inactivity,
   only when they have no valid leases. Metadata liveness is not ownership.
4. **Receipt retention:** delete at most 500 expired `runtime_requests` per pass,
   skipping locked rows. Never delete unexpired or non-expiring receipts,
   run-scoped receipts, or worker claim receipts. No transcript/event/checkpoint
   pruning is introduced. Liveness and retention still run if run reconciliation
   fails; failures are retried on subsequent ticks.
5. **Event delivery:** committed mutations already publish a run-id hint through
   `pg_notify`. Each replica listens on `platform_runtime_run` and wakes its local
   SSE subscribers. The listener uses one additional dedicated DB connection,
   reconnects with bounded backoff, and ignores invalid payloads. Durable event
   sequence cursors and one-second DB polling remain the correctness path if
   hints are duplicated, missed, or unavailable. Workers use their existing
   claim/input/heartbeat APIs; no worker push transport is introduced here.

On SIGINT/SIGTERM, the HTTP server drains for up to ten seconds (including SSE),
then background tasks are aborted and awaited. Embedded users should likewise
abort and await handles returned by `spawn_reconciler()` and
`spawn_notification_listener()`.

Platform does not hard-kill workers, automatically cascade aborts to children,
interpret harness checkpoints, or implement an infrastructure autoscaler.

## Verification

From `platform/`, against a local PostgreSQL role that can create disposable
databases (never a gateway production database):

```sh
DATABASE_URL=postgresql://localhost/postgres cargo test -p platform-server --test runtime_admin --test worker_runtime --test runtime_api --test runtime_schema -- --include-ignored
DATABASE_URL=postgresql://localhost/postgres cargo test -p platform-server --lib cross_replica_notifications -- --include-ignored
```
