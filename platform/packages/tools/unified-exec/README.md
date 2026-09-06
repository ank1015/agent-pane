# Unified exec tools

`tool-unified-exec` provides Codex-compatible `exec_command` and `write_stdin`
function tools over an injected `execution-core::ExecutionRuntime`.

The package deliberately does not own a process registry or storage client.
Instead, it exports serializable prepared, running, and session states:

1. Persist `PreparedExecCommand` before starting a command.
2. Persist `RunningExecCommand` after start and after every poll.
3. When the initial call becomes ready, atomically expose its tool result and
   save the returned `ExecSession` at session scope if the process is alive.
4. Persist `PreparedWriteStdin` before writing or interrupting a process.
5. Persist `RunningWriteStdin` after applying the interaction and after every
   poll. Atomically expose the result and replace or delete the saved session.

Caller-owned operation, execution, write, and signal identifiers make replay
safe after uncertain transport outcomes. The model-facing numeric session ID
is an alias for the execution handle; it is never an operating-system PID.

Session storage and tool-result messages should be committed together by the
harness. Do not independently save an advanced output cursor before committing
the corresponding output, or output can be lost from model history.

Only PTY commands accept ordinary input. For a non-PTY command, the exact
control character `\u{3}` is translated to an interrupt signal, matching Codex.
An empty `chars` value performs a poll.

The default hard collection cap is 1 MiB per tool call, preserving equal-sized
head and tail regions with an omission marker. Harnesses with smaller checkpoint
budgets should configure a lower cap.

The direct-shell behavior is compared against Codex commit
`ac192cd7937b0d73edc6dffe009940ae53782dd4`. This package intentionally omits
sandbox/approval arguments and does not install Codex's terminal, color, pager,
or locale environment overlay. The caller's configured environment is used.
It does not implement Codex shell snapshots or the zsh-fork executor.

`tool.definitions()` reflects disabled shell overrides/login shells and the
execution host's Windows yield range. `definitions()` exposes the default
catalog. Disabling login shells also disables the omitted-argument default;
an explicit request for a disabled capability fails. Shell scripts can be
empty, and workdirs use the shared normalized, root-contained path resolver.

The tool requests `CommandSpec::ShellScript`, which discovers the shell on the
execution host and chooses flags by shell type. Model shell paths select a
shell type, following Codex; discovery chooses the executable on that host.
Legacy `CommandSpec::Shell` callers retain their existing behavior. Starts also
include the prepared supervisor generation and a 50 ms post-exit output drain
limit. The supervisor rejects stale starts before executing any command. It
publishes child exit separately from output closure, then closes the journal
when output ends or the drain interval expires. It retains already-journaled
pages for polling. Other tools omit the drain limit and retain full draining.
Deploy the updated execution backend together with this package.

A prepared stdin interaction stores its requested wait duration. The live
collection window starts after dispatch (and the 100 ms reaction interval for
accepted PTY input). Initial command timing uses the caller's clock, never the
supervisor's `started_at` clock. Replaying a persisted running checkpoint keeps
its original collection deadline and collected bytes; it does not dispatch
input again.

`UnifiedExecOutput.output` retains the bounded raw text. Use the explicit
adapters when delivering it:

- `code_mode_result()` returns the existing JSON shape. An omitted token budget
  preserves the bounded raw output; an explicit budget truncates it.
- `to_text()` formats direct model history using the requested/default budget
  capped by `config.max_output_tokens`. It reserves history space for metadata
  and retains collection-omission notices when further truncating output.
- Serde on prepared/running/result values is for durable checkpoints. It includes
  internal output-budget metadata; do not use it as the model-facing adapter.

Whole-script `apply_patch`/`applypatch` heredocs, including the supported
`cd <path> &&` form, run through `tool-apply-patch`. Recognition uses the pinned
Codex Tree-sitter query, so surrounding commands are not silently discarded.
Raw patches without an explicit invocation fail. Patch verification happens in
`start_exec_command` without mutations; each subsequent poll performs one
recoverable patch step. **Persist every returned running state before polling
again**, including polls with no process events. `running.session()` is `None`
for intercepted patches. Their final result has no process/session ID, exit
code, or chunk ID. `wait_exec_command` remains an in-process convenience only;
it does not provide persistence between patch mutations.

Existing harness catalogs, worker registration, and server routes are unchanged.
A harness that later registers these tools must own session IDs, serialize
interactions on each session, commit cursor/output/session changes atomically,
and arrange cancellation and session cleanup.

The heredoc recognizer is adapted from OpenAI Codex under Apache-2.0; see
`LICENSE.codex` and `NOTICE.codex`.
