# apply-patch

`tool-apply-patch` exposes the Codex `apply_patch` single-environment custom tool
and applies freeform patches through `execution-core`. Reference behavior is
pinned to Codex commit `ac192cd7937b0d73edc6dffe009940ae53782dd4`.

`definition()` preserves the Codex name, description, and Lark grammar.
`prepare_arguments` accepts `ToolArguments::String`. Success text from
`ApplyPatchOutput::to_text()` uses Codex's grouped `A`/`M`/`D` summary, including
move destinations; `code_mode_result()` returns `{}`.

## Application and checkpoints

1. `prepare` parses the entire patch, resolves paths, rejects duplicate source
   paths, and verifies all update/delete sources without mutating files. Add
   destinations are not statted. Save the resulting `PreparedPatch`.
2. `start(&prepared)` creates a `RunningPatch`. Save it before stepping.
3. Call `step(context, &mut running)`. When it returns `CheckpointRequired`,
   persist the updated value **before calling step again**. A step either stages
   the next hunk's exact requests without dispatching, or dispatches one saved
   mutation and advances progress. It never stages a later hunk in the same step
   that acknowledges a mutation.
4. `Complete(output)` supplies the model response. Commit completion and tool
   history together using the caller's normal checkpoint transaction.

Updates are reread and recomputed at application time, so later hunks observe
prior writes/moves and retain unrelated external edits. Pending requests retain
stable operation IDs and exact bytes across serialization, including the source
unlink after a move. Following transport uncertainty, resume the saved
`RunningPatch`; do not restart from `PreparedPatch` or generate a new identity.
Only one owner may advance a patch checkpoint at a time.

`apply(context, &mut running)` is a convenience loop for in-process callers. It
has no persistence callback; durable harnesses must use `step` and save each
checkpoint. Reusing a completed running value returns the success result without
repeating filesystem effects within the same supervisor generation.

A patch is not a transaction. Errors report the acknowledged mutation prefix in
`applied_operations` and the full `total_operations`. Writes can fail after
truncation or partial output; `mutation_outcome` identifies uncertain/possibly
partial host outcomes. The acknowledged count does not prove later writes had
no effects.

## Filesystem behavior

Writes use `WriteStrategy::InPlace` and unconditional mutation conditions, as
Codex does: overwrites retain inode identity, hard links, open handles, and file
permissions. Add/move writes retry a missing parent by creating the requested
parent. Symlinks are followed for writes, including dangling links whose target
parent exists. Delete/move unlinks the leaf entry after checking the followed
target is not a directory. Moves can overwrite destinations; a move to the same
resolved path writes then unlinks that path, matching Codex.

UTF-8 text may contain NUL bytes. Updates use historical LF reconstruction by
default; select `ApplyPatchFileUpdateMode::PreserveLineEndings` for Codex's
preservation mode. The selected mode is stored in the prepared checkpoint.
Native paths are joined and lexically normalized, including `..`, before mapping
to registered execution roots. Host containment and read-only root policy still
apply. Windows device namespaces, alternate streams, ambiguous root mappings,
and unadvertised root spellings retain execution-layer restrictions. The
portable Windows resolver is tested on macOS; native Windows IO is not covered
by that test.

The package defaults impose no additional patch/file/operation caps. Callers
can configure finite limits; supervisor write limits, transport frame/body
limits, and storage budgets still apply. Checkpoints contain patch text and
pending file contents, so a future harness must choose suitable limits or blob
storage rather than assuming they fit its existing checkpoint budget.

## Execution deployment and recovery

Every patch mutation includes its expected supervisor generation. The supervisor
checks this before deduplication and IO, including requests from clients with a
cached descriptor. Completed, failed, and interrupted mutation receipts are
retained for that generation. A cancelled mutation with an unknown outcome
cannot be blindly retried. A supervisor restart returns `ExecutionLost` for old
patch requests; inspect/reconcile the filesystem before creating a new patch.
This is restart fencing, not a durable cross-restart mutation ledger.

The execution request additions use serde defaults, retaining legacy wire shapes
for other tools. New patch requests require upgraded execution-core consumers,
gateway/client binaries, and supervisors; older strict decoders reject the new
fields. No new gateway endpoint, database migration, server routing, or worker
registration is needed. Existing harness catalogs remain unchanged. Future
registration must preserve raw custom calls/results and the checkpoint sequence
above.

Tests include 24 vendored upstream filesystem scenarios, sequential destination
interactions, inode/permission and symlink behavior, and HTTP reply-loss/restart
recovery against the real execution supervisor.
