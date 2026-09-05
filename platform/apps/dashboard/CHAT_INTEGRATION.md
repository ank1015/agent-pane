# Dashboard chat integration

New Chat creates a session and its initial run atomically, then opens
`/projects/:projectId/:sessionId`. The session's persisted configuration supplies
the read-only model, account, reasoning, web-search setting and environment context.
Follow-ups send only input and expected revision; steering submits input to the
active run. Stop remains cooperative.

## Data flow

- TanStack Query owns session, history, runs and accepted-input caches. Message
  reads fetch every new revision page; cursor reads fetch every page. Existing
  history stays visible while refreshing.
- Named SSE events invalidate canonical reads, coalesced over 150 ms. Reconnects
  replay from the last sequence. Foreground polling (3–5 seconds while active,
  30 seconds while idle) covers missed events. Read retries are bounded and
  exclude ordinary client errors.
- Accepted inputs appear even before entering harness history. History IDs and
  common user-message IDs reconcile the two feeds without duplicate bubbles.
  Queued and unconsumed inputs are not presented as model-processed messages.
- Sends keep the exact payload and idempotency key in tab-scoped session storage
  until confirmed. Ambiguous failures offer an explicit retry, including after
  reload. Definitive conflicts refresh canonical state before another attempt.
- The old dashboard's AI Elements/Streamdown renderer, message actions, scroll
  restoration, code controls and animated run drawer are reused. Message rows
  and derived transcripts are memoized; elapsed-time updates stay inside the
  individual run row. The heavy conversation renderer is route-lazy-loaded.

## Current boundaries

The worker publishes complete assistant messages and tool results, not token
deltas or streaming terminal output. The drawer updates at those commit points.
Attachments remain disabled. Custom wait-resolution forms and session forking
are not part of this UI integration.

## Verification

`pnpm test`, `pnpm lint`, and `pnpm build` run from this directory.

For isolated browser checks, run `node tests/chat-preview.mjs` and open the URL it
prints (port 5174). This uses in-memory fixtures and never calls real gateways:

- Submit, follow up, steer, stop, reload, and open run details.
- GET `/api/fixture/drop-next` makes the next three mutation responses fail even
  though the mutation is accepted. Reload and retry to check durable deduplication.
- GET `/api/fixture/disconnect` closes current event streams to test replay.
- GET `/api/fixture/requests` shows captured test requests and their keys.

Stop the fixture with Ctrl-C. It is not part of the production application.
