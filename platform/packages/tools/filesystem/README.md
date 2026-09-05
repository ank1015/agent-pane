# Filesystem tool paths

`tool-filesystem` shares `resolve_path` and `validate_cwd` between the read and
write libraries. It is an internal support package, not a model-facing tool.

Resolution uses the selected execution host's roots and path convention, never
the worker's native filesystem. Relative paths use the supplied cwd. Absolute
paths select the most specific advertised root, with ambiguous matches rejected.
Unix, Windows drive, and UNC paths are covered by tests. Parent (`..`) segments
are rejected to avoid changing the meaning of remote symlink traversal; use an
absolute path instead. The execution filesystem enforces symlink containment.

Errors from this package use `source: "tool-filesystem"`.
