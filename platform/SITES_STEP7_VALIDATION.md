# Sites Step 7 validation — 8 September 2026

Validated locally against the configured **Harness evals** project, real ChatGPT
account, LLM/execution gateways, PostgreSQL, Sites service, worker and dashboard.
Sites is enabled in the project's existing harness settings. Local service and
project credentials live only in ignored configuration files.

## Implemented dashboard flow

- Sites uses the Environments table styling. The name opens management; every
  Open link opens the authenticated live viewer in a new tab.
- Create site / Edit with Sites use the existing harness, model/account and chat
  UI. New and existing site selection require no authoring sandbox. A new site's
  derived session link leaves frozen harness configuration unchanged.
- Site management provides a live preview, rename, recent authoring conversations,
  user snapshots/restore and bounded backend diagnostics. Multiple conversations
  can edit the same live pair. Preview refreshes when the active code changes.
- Snapshot/restore identity persists before submission. Inspection resolves a lost
  reply; restore changes code while preserving database data and accepted work.

## Real service-boundary results

| Workflow | Evidence / result |
| --- | --- |
| Create and edit | Sites chat created a counter with persistent SQLite data. A second independent conversation edited the same site. |
| Browser/user controls | Playwright verified rename, increment, a lost accepted snapshot POST reply, second-chat editing, live preview refresh, user rollback, preserved count, desktop/narrow rendering and new-tab Open. |
| Independent evaluations | Two basic Codex sessions wrote actual JSON files in separate `/home/user` sandboxes: n=7/square=49 and n=11/square=121. |
| Same-workspace verification | Sites read harness-published run outputs and ran Python assertions against those actual files through durable execution handles. Both passed. Their results appear in the live site. |
| Shared sandbox SDK | Sites created an additional sandbox from an authorized snapshot, ran a command successfully and confirmed termination. Both evaluation sandboxes were also terminated with `terminationConfirmed:true`. |
| Callback application | The site's backend created a research-only Sites child. Repeating the logical launch returned the same session/run. Platform and Sites were restarted after acceptance while the worker was stopped; a restarted worker completed the child and its callback updated the persisted job. |
| Reload and deduplication | Reopening the viewer recovered the completed job from SQLite. Repeating launch after restart still returned the same child. Read-only checks found one child session/run, one job and one callback event. This is application deduplication; callback transport remains at least once. |
| No execution outputs | The callback child's `runs.outputs` returned `items: []`; this remained a successful, supported case and created no site. |
| Recovery | The two evaluation runs recovered after worker restart and replacement of their test supervisors. Dedicated service tests also cover interrupted JavaScript, accepted effects, steering, abort and saved waits. |

Retained local demo: **Sites Step 7 E2E**, site
`01a080fb-a7e4-7153-b000-675d076ad013` in project
`01a06afd-ede1-79c1-ba89-c373cb4d717a`.
Authoring session: `01a080fb-868e-7a12-a92c-e4525c9664dd`.
Evaluation runs: `01a08101-d224-7431-aa6f-04eee8d7df9d` and
`01a08101-d236-73d1-baf4-7ee214456eea`.
Callback child: `01a08121-1575-73a2-aa40-8a3263ad2239`.
The site, user snapshot, transcripts and compatible environment remain available;
test sandboxes have been terminated.

The model-generated callback prototype needed a manual correction before the
successful browser launch: stable persisted input, exact callback event shape,
whole-line patches and honest pending/retry states. This check demonstrates the
Platform/backend workflow; it does not claim the first generated app was correct.
The harness prompt now documents those rules, authoring-only schema changes and
the opaque iframe's lack of browser storage. Invalid authoring patches now return
static corrective guidance to the agent without exposing server response bodies.
Backend failures retain a generic frontend error and bounded author-visible logs.

## Automated checks

- Dashboard: 47 tests, lint and production build passed. Existing large-chunk
  build advisory remains.
- Platform server: all test groups passed, including 31 Sites integration tests
  with local PostgreSQL, real Sites/code-mode processes and browser probe.
- Worker: 12 tests passed, including the opt-in PostgreSQL tests.
- Sites service: 26 tests passed across library, backend, foundation and
  publication/live-authoring suites.
- Sites harness: 5 tests passed; generated shared SDK consistency test passed.
- Browser helper: native content isolation, absence of synthetic page errors,
  genuine page-error reporting and missing-policy rejection passed.
- Strict Clippy passed for server, worker, Sites service, Sites harness and the
  Platform code-mode adapter. `git diff --check` passed.

The foundation fixture now uses its temporary directory as its working directory
so a developer's Sites `.env` cannot change fixture listener configuration.
Reproducible browser commands and prerequisites are in
[dashboard Sites](apps/dashboard/SITES.md). The callback check has separate launch
and verify phases so an operator can restart services between acceptance and
completion. Deterministic service tests use simulated model output; live browser
checks used the configured real account and incurred model/sandbox usage.

## Runtime compatibility and enablement

The existing saved Python snapshot contained an old execution supervisor that
rejected `expected_generation` and `output_drain_timeout_ms`. Keeping generation
fencing was necessary. Only the two disposable test instances were refreshed using
the current local Linux supervisor; original environments/snapshots were preserved.
A clean compatible snapshot was saved as **Python 2GB - current runtime**
(environment `01a0810f-f7ea-7653-a0be-712bbb1656f8`) for future work.
Older snapshots still need an operator refresh before using these capabilities.

This is local, single-project enablement, not a production deployment. The trusted
local dashboard BFF holds project credentials server-side; static `dist/` hosting
needs an equivalent authenticated backend. See [server setup](apps/server/SITES.md)
for configuration and staged enablement. Firecrawl research requires its optional
worker configuration; the live checks did not exercise an external research query.
