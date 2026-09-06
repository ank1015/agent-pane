# Filesystem tool paths

`tool-filesystem` shares `resolve_path` and `validate_cwd` between the read and
write, edit, and bash libraries. It is an internal support package, not a model-facing tool.

Resolution uses the selected execution host's roots and path convention, never
the worker's native filesystem. Relative paths use the supplied cwd. Absolute
paths select the most specific advertised root, with ambiguous matches rejected.
Unix, Windows drive, and UNC paths are covered by tests, including the Windows
canonical `\\?\C:\` and `\\?\UNC\server\share` forms. These normalize only for
root matching; advertised paths and the supervisor's canonical roots stay intact.
Other device namespaces remain unsupported. Parent (`..`) segments
are rejected to avoid changing the meaning of remote symlink traversal; use an
absolute path instead. The execution filesystem enforces symlink containment.

Errors from this package use `source: "tool-filesystem"`.

`resolve_path_normalized`, used by apply-patch, provides a separate Codex-style
lexical join. Parent segments collapse before registered-root mapping, stopping
at the native POSIX, drive, or share root. The strict `resolve_path` behavior
used by existing tools is unchanged. Containment and other execution path
restrictions still apply.
