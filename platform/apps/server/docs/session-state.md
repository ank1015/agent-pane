# Private session state

Harnesses can persist tool state across runs in the same session without a
harness-specific database. Platform stores it in `session_state`, separately
from conversation history and per-run checkpoints. Payloads are opaque JSON
objects; Platform does not implement the tools that interpret them.

## Ownership and lifetime

- Each entry is identified by `(session_id, namespace, key)`.
- The session is derived from the authenticated run, never supplied by the caller.
  The session already belongs to one harness. Namespaces are harness-chosen tool
  groupings, not an authorization mechanism within trusted harness code.
- Reads and writes require a current run lease for that session. No private
  state is exposed through application history, public events, or `QueryClient`.
- Follow-up runs in the same session see the saved state. Worker replacement
  reloads it from PostgreSQL. Completing/failing/aborting a run retains it.
- New sessions and history forks start with empty private state, even when they
  inherit messages. Process handles and private state are never implicitly copied.
- This persists explicit data, not Rust futures, V8 isolates, or remote processes.

## Read

`GET /internal/runs/{run_id}/session-state`

Uses the existing worker bearer token, `X-Worker-ID`, and `X-Lease-Epoch` headers.
Queries:

- `namespace`: required; `[a-z][a-z0-9_.-]{0,127}`.
- `key`: optional exact lookup; 1–256 printable ASCII characters without spaces.
- `after_key`: exclusive byte-ordered pagination cursor; cannot accompany `key`.
- `limit`: 1–50, default 25.

Response: `session_id`, `items`, and `next_after_key`. Each entry includes its
namespace/key, version, JSON `value`, last-writing run/epoch, and timestamps.
An exact lookup for a never-written key returns no items. A deleted key returns
an entry with `value: null` and its retained version. Pagination includes these
tombstones; it does not silently fetch more pages or acknowledge any input.

This is a current-state view, not a historical snapshot across pages. Read run
context to obtain commit versions and use each entry's own version. Concurrent
changes must cause a conflict, not a blind overwrite or automatic rebase.

## Write atomically with a run commit

Add `session_state` to `POST /internal/runs/{run_id}/commits`:

```json
{
  "expected_run_version": 4,
  "expected_session_revision": 2,
  "checkpoint": {"expected_version": 1, "state": {"phase": "tool_saved"}},
  "session_state": [
    {
      "namespace": "codex.exec",
      "key": "12",
      "expected_version": 0,
      "mutation": {"op": "set", "value": {"execution_id": "gateway-handle", "last_sequence": 8}}
    }
  ],
  "disposition": {"status": "running"}
}
```

`set` replaces the entire object; it is not a JSON merge. For removal use
`"mutation": {"op": "delete"}`. Explicit operations prevent a missing value
from accidentally becoming a delete. At most one write per namespace/key is
allowed in a commit.

`expected_version: 0` means the key has never existed. Successful writes start at
1 and increment by one. A delete retains a tombstone and increments the version;
recreation must use that version, not zero. Deleting a never-written key with
version zero creates a version-1 tombstone. This prevents stale writers from
recreating deleted state (the delete/recreate or ABA problem).

State writes share the transaction with checkpoint updates, messages, input
acknowledgements, waits, and disposition. Any failure rolls back everything.
Writes increment the run version but do not advance the transcript revision.
They can accompany a waiting or terminal commit and happen before releasing the
lease. The response's `session_state` contains only the entries written by that
commit, including tombstones.

An entry mismatch returns `409 SESSION_STATE_VERSION_CONFLICT`. Run/session
version conflicts and lost leases retain their existing codes. Reload and let
the harness decide how to reconcile; do not silently retry with a newer version.
Receipt retries reuse the identical key/body and return historical entries.
Replaying a receipt does not overwrite newer state. Older commits with no storage
writes preserve their request hashes; older receipts omit the new response field.

Bounds: 200 state writes per commit, 256 KiB of compact JSON per value, and the
existing 1 MiB total request limit. Large datasets/blobs belong in dedicated
storage, with references saved here. Tombstones remain durable; no automatic
garbage collection resets their versions.

The SDK exposes `RunClient::session_state` and `Commit::session_state`; see the
[client example](../../../packages/platform-runtime-client/README.md#session-scoped-tool-state).
The server applies the additive migration on startup. No existing state is
rewritten or backfilled.
