# Sites implementation fixes — 2026-09-10

Update: the Sites-specific outgoing native-message filter was subsequently removed at the user's request, together with its checkpoint flag and projection-specific tests. Provider packages remain unchanged. Outgoing messages now retain full native records; the oversized-request issue can recur pending provider-level cleanup. The filtering results and live run below describe the earlier implementation, not the current behavior. Other implementation fixes remain in place.

Follow-up to `SITES_FINAL_E2E_2026-09-10.md`. The original report remains a record of the failures before these fixes. No model system-prompt changes were made in this fix pass.

## Implementation changes

- Authoring SQL now permits SQLite's index-population authorization action when creating application indexes. Backend requests still cannot perform schema operations. Regression coverage includes populated tables, receipt replay, indexed reads, unique indexes, and dropping indexes.
- First non-metadata authoring access waits up to five seconds for initial Site provisioning. It polls readiness only; it does not replay mutations. Pending, suspended, and failed states produce specific errors.
- Bounded public downstream 4xx error envelopes now reach the authoring model with actionable messages. Generic transport logging remains body-free; malformed envelopes and server-error bodies are not forwarded.
- Sites LLM requests omit redundant native response-envelope metadata for OpenAI/ChatGPT assistant messages, retaining native output items unchanged. Persisted history is untouched. Provider-adapter tests assert identical provider wire input before and after projection, including opaque reasoning and custom tool calls. Legacy checkpoints retain their original serialization for already-admitted operations; projection starts at the next new operation.
- Existing Sites sessions use the composer label “Site instructions.”

The request-size fix is lossless transport projection, not history summarization or unlimited context. Genuinely large useful histories can still encounter request or model limits.

## Verification

- Sites harness and Sites service library tests: 18 passed.
- Server public-error client regression test: 1 passed.
- Sites harness server integration suite: 12 passed on final serial rerun. An earlier run had three fixture startup HTTP 502 failures while builds were running concurrently; the standalone abort test and full rerun passed. The startup failures' cause was not conclusively established.
- Integration regressions cover initial parallel metadata/read/read calls, detailed patch rejections, index creation, and more than 8 MiB of saved native history while all 31 outbound model requests remain below 256 KiB and all 30 assistant turns remain stored.
- Dashboard tests: 50 passed. Dashboard build and server/worker/Sites binary builds passed. Dashboard build retains its existing large-chunk warning.
- `git diff --check` passed.

## Live hosted-gateway regression

The updated local Platform services continued using the hosted LLM and execution gateways. No gateway deployment limit was changed.

Original failing authoring session: `01a087b1-1d39-7942-9a0b-835baebd171c`.

Post-fix run: `01a089a3-2f9a-7191-bacc-a7f77eb40974`, completed at `2026-09-10T04:46:26.118406+00:00` using the existing history, account, and model. Its one exec call performed three SQL operations successfully:

1. Created `lab_tasks_created_idx` on `lab_tasks(created_at DESC, id DESC)`.
2. Confirmed the index in `sqlite_schema`.
3. Read task counts: four incomplete and two completed.

No source edits, additional tasks, child agents, sandboxes, commands, or permission changes were requested in this regression. See `SITES_TOOL_CALL_AUDIT_2026-09-10-post-fix.md` for the continuation audit. Local services were left running.

## Model behavior and guidance proposals — not implemented

- The current form guidance is misleading under an iframe that disallows native form submission. Teach explicit button-click and Enter handlers, and test the actual controls rather than dispatching a synthetic submit event. Do not weaken browser isolation.
- Put the two-file, Update-only patch restriction beside the tool description as well as in the system prompt. Preserve raw-string input. Precise rejection details now make recovery easier.
- Prefer bounded waits for observable DOM or backend state to fixed sleeps.
- Copy resource IDs from returned data instead of retyping them.
- Demonstrate one durable submission record containing the complete retry payload. Keep status/display projections separate from that record, and test recovery in a fresh request after an uncertain submission. The generated test backend's missing retry fields remain an application defect for the next discussion, not a fixed harness issue.
