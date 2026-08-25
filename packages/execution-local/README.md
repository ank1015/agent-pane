# execution-local

Reference implementation of `execution-runtime` for the device on which it is
running. The same crate is intended for macOS, Linux, Windows, cloud VMs, and
small devices; operating-system differences stay in internal `cfg` modules.

Implemented capabilities:

- secure workspace and grant-scoped native path resolution;
- bounded text, binary, and media reads;
- typed directory listing and path/content search;
- prepare/commit/abort and one-shot workspace mutations;
- durable piped and PTY process sessions with replayable sequence-numbered output;
- local artifact storage and chunked retrieval;
- primitive filesystem operations.

The resolver canonicalizes paths on the target and checks containment after
resolving symlinked ancestors. Workspace paths never trust cloud-side native
path interpretation. Native `file://` paths require an explicit
`LocalNativeGrant`.

Current first-pass limitations:

- Required sandboxing and denied-network policies fail as unsupported rather
  than silently running without isolation.
- Mutation preparation verifies strong file revisions but does not yet provide
  cross-file rollback, so results report `atomic: false`.
- Artifact metadata is maintained for the lifetime of the runtime instance;
  persistent indexing and cloud upload belong in the machine service layer.
