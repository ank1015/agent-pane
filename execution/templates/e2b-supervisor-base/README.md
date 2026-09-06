# E2B execution base

Public E2B template containing the `execution-supervisor` binary used by
`execution-e2b`, plus `ripgrep` for fast workspace search.

The Dockerfile downloads an immutable, content-addressed supervisor artifact,
verifies its pinned SHA-256 digest, and installs it at:

```text
/usr/local/bin/execution-supervisor
```

Build and publish with the authenticated E2B CLI:

```sh
e2b template create agent-pane-execution-base-2gb-7d4c8ad2 \
  --path execution/templates/e2b-supervisor-base \
  --dockerfile Dockerfile \
  --cpu-count 2 \
  --memory-mb 2048

e2b template publish agent-pane-execution-base-2gb-7d4c8ad2 --yes
```

Published templates:

| Alias | RAM (MiB) | vCPU | ID |
| --- | ---: | ---: | --- |
| `agent-pane-execution-base-1gb-7d4c8ad2` | 1024 | 1 | `gbiubfcth0yh0qke09xr` |
| `agent-pane-execution-base-2gb-7d4c8ad2` | 2048 | 2 | `rpffhldnk5j55ptiu283` |
| `agent-pane-execution-base-4gb-7d4c8ad2` | 4096 | 2 | `0eslaqgop81dfuolq66a` |
| `agent-pane-execution-base-8gb-7d4c8ad2` | 8192 | 4 | `11whj4oo5h2x55p3fzj1` |

`execution-e2b` pins these template IDs and defaults base creation to 2048 MiB.
