# Cloudflare Pages artifacts

The `agent-pane-execution-artifacts` Pages project hosts immutable, versioned
execution binaries. A deployment must include the complete current site, not
only the new release, because each direct-upload Pages deployment replaces the
previous asset set.

The production artifact hostname is `downloads.acentric.dev`; the default
`agent-pane-execution-artifacts.pages.dev` hostname remains available as a
fallback.

Each release publishes raw binaries, archive packages, per-file SHA-256 files,
a combined `SHA256SUMS`, and `manifest.json`. Convenience installers verify the
raw binary digest before installation. The `latest.json` document is a mutable
pointer; versioned release URLs are immutable by convention.
