# Sites harness end-to-end validation — 2026-09-10

Follow-up: implementation fixes and successful continuation of the original oversized session are recorded in [SITES_FIXES_2026-09-10.md](SITES_FIXES_2026-09-10.md). The findings below describe the original pre-fix test.

Status: completed at 2026-09-09 20:12 UTC (2026-09-10 IST), with defects found—not an all-green result. This report distinguishes real hosted-model execution from deterministic local fixtures. Existing project environments, grants, and unrelated data were not modified.

Summary: 75 public model tool calls audited across three test sessions; all six inner tools plus exec/wait exercised. Five runs completed and one long-session follow-up failed before a model response. Four tool-call errors were recovered during earlier runs. 145 deterministic tests passed. The highest-priority implementation findings are index creation rejection, unhelpful runtime error details, and long-session request-size failure. The generated test Site's Add task bug was repaired by its agent; its uncertain-submit retry defect remains recorded and unfixed.

## Setup

- Dashboard: http://localhost:5173 (must match the configured Site dashboard origin).
- Platform API: http://127.0.0.1:3100.
- Worker health/status: http://127.0.0.1:3101.
- Sites internal service: 127.0.0.1:3102; content listener: http://127.0.0.1:3103.
- Hosted LLM gateway: https://llm.acentric.dev/.
- Hosted execution gateway: https://execution.acentric.dev/.
- Project: Harness evals, `01a06afd-ede1-79c1-ba89-c373cb4d717a`.
- Live authoring account/model: `ak15` / ChatGPT / `gpt-5.6-terra`, high reasoning.
- Test session: `01a087b1-1d39-7942-9a0b-835baebd171c`.
- Separate test Site: Harness Lab — E2E 2026-09-10, `01a087b1-3e7c-7ee3-b72c-502ab0b27979`.
- Credentials stay in existing ignored configuration; no credentials are copied into this report or test artifacts.

The binaries were rebuilt before startup. A recorded live model request was inspected: its instructions exactly equal `packages/harnesses/sites/src/system_prompt.md`, SHA-256 `cc329a9e5b97856a805f00f5892b22228dbefdbc7fef16be12d6b27762248c12`. It exposes only `exec` (custom Lark grammar) and `wait` (function).

## Harness overview

The Sites harness is a trusted Rust library registered in the shared worker. Its independent JavaScript code-mode runtime exposes six tools: metadata, line-window source reading, raw-string patching, SQL, backend invocation, and a three-action browser. There is no agent-side Platform SDK or Codex runtime dependency.

The Site contains `index.html`, `backend.js`, and persistent managed SQLite data. Source edits validate and activate the frontend/backend pair. Backend requests run in fresh isolated JavaScript runtimes and use `ctx.db` and scoped `ctx.platform` capabilities. The browser evaluates code in the real Site frame, emits viewport screenshots, and explicitly reloads releases. Code-mode cells and browser state are temporary; application data and accepted Platform operations are durable. Registered completion callbacks continue workflows in fresh backend requests.

## Real-model scenarios

### 1. State-based investigation

Run `01a087b1-1d47-75a2-852a-4f5ec5ebf821` completed.

The model independently read source/metadata, patched a diagnostic backend endpoint, invoked it, and used the backend SDK to discover environments, harnesses, accounts, execution resources, sessions, runs, and start options. It did not attempt direct agent-side SDK access or launch workspace work.

Independent dashboard API checks confirmed five saved environments, all sandbox-backed. The Site correctly saw only its two granted harnesses (`environments`, `sites`) and two granted provider accounts, despite the dashboard having an additional OpenAI account and harness. Start options further restricted Sites to the ChatGPT account. Hosted execution-resource discovery returned two registered machines (Mac ready, Windows unavailable) and one E2B account. The existing Environments session had six completed runs and one aborted run.

The final natural-language answer contained one incorrect copied UUID: the sql-formatter environment was reported with `7b32`, while the tool result and actual environment ID contain `7a32`. State grounding succeeded, but exact identifier transcription did not fully pass.

### 2. Application construction and interaction

Run `01a087b3-aa4c-7b71-b704-1f64c85286c8` completed. It created the two task tables and frontend/backend handlers; backend tests confirmed initial empty data, task creation, same-key replay returning the same row, changed-input replay returning 409 (including a trailing-space change), whitespace title rejection (400), and nonexistent task rejection (404). It used all six authoring tools across the first two runs. Browser screenshots were emitted as images and reload retained the open/completed test tasks.

However, independent browser verification found that the real Add task control did not work: see finding 6. The agent's original end-to-end success claim therefore does not fully pass.

### 3. Callback, hosted execution, and corrective investigation

Run `01a087ba-5096-7991-abe3-64dac1825852` completed. It launched exactly one harmless Sites child with `ak15` / `gpt-5.6-luna`, registered and reconciled a completion callback, and created one 300-second sandbox from the existing httpx snapshot with `networkAccess:false`. Commands were limited to `pwd` and `git status --porcelain` in that sandbox's app workspace, with no file changes, installs, connected-machine execution, or permission changes.

The independent UI failure was sent through the dashboard's active-run steering control. The agent was asked to diagnose/repair only the generated test Site and verify a real button click, while leaving Platform code and browser restrictions unchanged.

Steering was consumed at transcript revision 68. The agent reproduced the failure with `.click()` (70), confirmed that a valid form never emitted submit (74), added explicit click/Enter handlers (79–80), and created task 3 through the repaired control (82, confirmed by SQL at 84). A short post-reload check initially observed an empty loading list (86); a later check found all three persisted tasks (88). Independent dashboard browser control then successfully created “E2E real click verified” with an actual button click. Keyboard submission, task completion, reload, and callback state were subsequently verified.

Independent keyboard submission also passed: “E2E Enter verified” persisted through reopening. An authenticated read-only backend invocation confirmed five tasks, including IDs 4 and 5 created by the independent click/Enter tests, and the original completed task.

The child session `01a087c1-b0cc-7421-a3f8-09783bde24c5` had exactly one run, `01a087c1-b0cf-7652-b2e4-4b493e7caa93`, which completed with exactly `callback-ready` and no tool calls. The main agent's repeated launch returned the same IDs with `replay:true` (100). Independent backend observation confirmed the persisted completion event `83938a92-b7a5-4195-b193-32a5111d348e`, subscription `01a087c1-b0d5-7591-8114-4a811b0990ca`, completed state, and transcript text. This verifies one real callback delivery and duplicate launch prevention; it does not by itself verify repeated callback delivery or crash recovery in the generated app.

Hosted sandbox `01a087c2-7426-7ee2-946b-487da069e233` reached ready with workspace host `01a087c2-7e39-7460-ae60-5f3a7e6f93b9`, directory `/home/user/app`, and expiry `2026-09-09T20:06:03.014950Z`. The model passed `networkAccess:false` and 300 seconds to the SDK; no external-network probe was performed. Commands `pwd` (`01a087c2-a047-7761-a6a3-e959aa64cf24`) and `git status --porcelain` (`01a087c2-a050-7570-9022-6112960e9bc1`) both completed with exit 0. The former returned `/home/user/app\n`; the latter returned empty output. No replacement sandbox or additional commands were needed. Automatic expiry was independently confirmed below.

The model naturally exercised a yielded exec cell (114) and called the actual `wait` tool with that cell ID (115). Wait returned completion and the new backend result (116), without rerunning commands. Both public tools and all six inner tools have now been exercised live. ALTER TABLE also succeeded (118).

At `20:06:21Z`, an independent read-only status invocation observed the hosted sandbox as `expired`, with both termination requested and termination confirmed true. Both command records still reported completed/exit 0. The saved application row still said ready, correctly separated from the fresh `live_sandbox` observation. No manual terminate call was made.

The callback delivery API reported `delivered`, one attempt, and no last error (128/146). A direct non-callback invocation of the generated handler returned application HTTP 403 `Invalid callback context` while the invocation itself succeeded (148), as expected.

### 4. Long-session follow-up failure and fresh-session verification

Read-only follow-up run `01a087c8-92a6-7eb2-872b-fe48bb680523` failed before any model/tool response, with `llm_request: Failed to buffer the request body: length limit exceeded`. This is a significant long-session integration failure, not a model tool mistake. The original failed session/history is preserved.

A fresh read-only authoring session against the same existing Site completed independent state verification: session `01a087cb-f6b5-7a50-bf06-bd1c8bdf1e7a`, run `01a087cb-f6b8-7e12-b33a-89761b1f1d6e`, same ak15/Terra/high configuration. Its three exec calls used metadata, SQL SELECTs, source windows (1–240 and 241–436), and the existing read-only diagnostic endpoints. It made no code/data changes and launched no work. It correctly reported six tasks (four incomplete, two completed), the exact corrected environment ID, completed child run, delivered callback with one attempt, both command exit codes/output, and fresh sandbox expiry versus stale saved ready state. It also independently identified the missing retry fields by source review. A new session can therefore recover application context from durable state, but this does not fix the original history-size failure.

## Findings

1. **Authoring SQL rejects documented index creation.** Tables were created, but `CREATE INDEX IF NOT EXISTS lab_tasks_created_idx ON lab_tasks(created_at DESC, id DESC)` failed twice with HTTP 400 `INVALID_RUNTIME_REQUEST` (transcript revisions 21 and 25). `apps/sites-service/src/database.rs` allows `CreateIndex` but falls through to deny `Reindex`. A separate in-memory SQLite check confirmed that CREATE INDEX emits `SQLITE_REINDEX`; denying that event produces `not authorized`. No implementation fix has been applied.
2. **Provisioning errors are too generic for the agent.** Concurrent first-call metadata/source reads returned HTTP 409 `RUNTIME_CONFLICT` (revision 3). Subsequent metadata and sequential reads succeeded. The server checks readiness after lazy provisioning, but the nested error omits its useful provisioning explanation. The agent recovered without intervention.
3. **Exact-ID answer typo.** See scenario 1. This is model output fidelity, not incorrect SDK data.
4. **Unsupported patch form attempted and rejected.** Despite explicit Update File-only instructions, the model tried Delete File + Add File for backend replacement (revision 26). The tool rejected it (revision 27); the next attempt used Update File. Source was protected; the instruction was not consistently followed on the first attempt.
5. **Minor dashboard copy mismatch.** In an existing Sites conversation, the follow-up composer is exposed as “Environment instructions.” The new-session Sites composer correctly says “Site instructions.”
6. **High-impact generated UI bug plus false-positive interaction test.** The frontend uses `<form>` with a submit handler and a `type="submit"` Add task button. The Site iframe disallows forms. Independent browser actions filled a valid title and clicked Add task; pressing Enter also produced no task or notice. The input remained populated and its validity was true. The model's test (revision 38) bypassed the actual button/default submission by directly dispatching a synthetic `submit` event; that handler created a row, so the model reported the UI as working. This is a generated application/test-method issue, not evidence that the backend create endpoint failed. Platform isolation must not be relaxed to accommodate the app.
7. **Generated recovery-path defect found by source review, not triggered live.** `storedCheck()` initially selects only display/status columns, while `submitAgentCheck()` needs the saved account, config JSON, message fields, and mutation key. On the first launch it receives a full candidate record and works. After an uncertain first submission, a later request reconstructs a partial record and cannot replay the original request correctly. The successful duplicate-launch test skips submission because IDs already exist, so it does not test this path. Review the final source before treating this test app as a reusable recovery example.
8. **Long authoring conversations exceed the hosted LLM request body limit.** At revision 152, serialized message objects total 8,328,399 bytes; native response payloads alone total 7,995,198 bytes (about 96%). Without native payloads, the message objects total only 331,851 bytes. The prompt adds 75,848 UTF-8 bytes before JSON escaping, and tools/request envelope add more. `packages/harnesses/sites/src/driver.rs:404` clones full stored messages into every gateway request. The ChatGPT adapter only extracts `native_message.output` for replay, but that projection happens after gateway ingress. Retained native response records also repeat instructions and other response metadata. `llm/deploy/gce/compose.yaml:49` configures `LLM_GATEWAY_MAX_REQUEST_BYTES: 8388608` (8 MiB), while the gateway's local default/example is 64 MiB. The observed rejection matches the deployment limit being crossed. Hosted runtime configuration was not independently read. No request-size preflight, compaction, or recovery was observed. Raising a limit alone would postpone the problem; outbound replay data and history growth need deliberate handling while retaining necessary native tool/reasoning items.

Independent UI checks also confirmed empty-input feedback and persisted callback state after reopening. A later attempt to click Launch check timed out because the button was disabled after completion; that is expected UI state, not a tool/harness defect. The local audit probe initially assumed every call had string arguments and failed formatting the first JSON-shaped `wait` call; its formatter was corrected. Transcript/audit persistence happened before that display error. This was a test-probe bug, not a Sites code-mode failure.

### How to read the call audit

The recorded success/error outcome belongs to each outer exec/wait call. Inner tool calls may already have succeeded before an execution throws: for example, the schema loop created both tables before index creation failed. Application HTTP 400/404/409 responses inside a successful `invoke` are expected negative-test results, not harness failures. The audit preserves submitted code and emitted results so these distinctions can be checked rather than inferred from the outer status alone.

## Deterministic validation

These tests use local fixtures, not hosted LLM/execution gateway calls:

- Sites browser runtime: 6 passed.
- Dashboard: 50 passed.
- Sites harness server integration: 10 passed, including live frontend/backend/SQL, screenshot transport, pinned reload, wait, steering, abort, crash and drain recovery without replaying accepted effects.
- Callback server integration: 3 passed, including two-stage continuation across restarts, duplicate handling, interrupted execution, and bounded retry behavior.
- Additional service/code-mode/read suites: 67 passed (Sites service 28; Sites harness 8; live code mode 12; legacy code mode 6; shared read 13).
- Remote execution lifecycle server integration: 8 passed, including sandbox readiness/expiry, durable command output, cancellation, takeover fencing, and replacement behavior.
- Execution resource discovery authorization/credential-filtering integration: 1 passed.

Total deterministic tests passed: 145. Passing fixtures did not catch the live CREATE INDEX or hosted long-session request-size gaps described above.

## Final records, scope, and handoff

- Main call audit: `SITES_TOOL_CALL_AUDIT_2026-09-10.md` — 72 calls (71 exec, 1 wait), 4 errors.
- Fresh-session audit: `SITES_TOOL_CALL_AUDIT_2026-09-10-fresh.md` — 3 successful exec calls.
- Child audit: `SITES_TOOL_CALL_AUDIT_2026-09-10-child.md` — no calls, as requested.
- Final test data: 6 tasks. Incomplete: `E2E O'Reilly`, `E2E independent UI task`, `E2E real click verified`, `E2E final actual button click`. Completed: `E2E UI task`, `E2E Enter verified`.
- Final release: `01a087c7-3944-7661-a51f-897bd3b6e9d4`. Fresh read-only verification did not change it.
- Gateway-recorded model cost across these test sessions: approximately $1.6099. This is recorded catalog usage accounting, not a verified invoice or sandbox cost.
- Local dashboard/server/worker/Sites services are left running with the hosted execution and LLM gateways. The Site and three test conversations are retained for inspection; the single hosted sandbox expired automatically.

Temporary probe scripts, prompts, normalized transcripts, and accepted request IDs are under `/tmp/sites-final-e2e.zuTRqB`. Audits record each observed model tool call, its submitted source/arguments, result revision, outcome, and emitted result (image payloads replaced with a marker; signed content-view tokens redacted). Private reasoning/provider-native records are excluded. Detailed root causes above refer to main-session revisions unless explicitly marked fresh/child.

This is representative end-to-end coverage, not exhaustive proof. Real-model tests used the ChatGPT account with Terra authoring and Luna child execution; other providers were discovered but not run. Repeated callback delivery, crash recovery, cancellation races, output bounds, and worker takeover were exercised in deterministic fixtures, not induced against the hosted test Site. Snapshot restore, concurrent human/agent edits, exhaustive mobile/accessibility testing, and forced uncertain-submission recovery in the generated app remain untested. Desktop screenshots and actual controls were checked; the independent browser's full-page capture/scroll attempts were inconclusive, so lower-page visual coverage is not claimed from those attempts. No production implementation fixes were applied during this validation pass.
