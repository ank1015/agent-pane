# E2B execution base

Public E2B template containing the `execution-supervisor` binary used by
`execution-e2b`.

The Dockerfile downloads an immutable, content-addressed supervisor artifact,
verifies its pinned SHA-256 digest, and installs it at:

```text
/usr/local/bin/execution-supervisor
```

Build and publish with the authenticated E2B CLI:

```sh
e2b template create agent-pane-execution-base-2gb \
  --path execution/templates/e2b-supervisor-base \
  --dockerfile Dockerfile \
  --cpu-count 2 \
  --memory-mb 2048

e2b template publish agent-pane-execution-base-2gb --yes
```

Published templates:

| Alias | RAM (MiB) | vCPU | ID |
| --- | ---: | ---: | --- |
| `agent-pane-execution-base-1gb` | 1024 | 1 | `h5178y3chdmnimc6bk78` |
| `agent-pane-execution-base-2gb` | 2048 | 2 | `gls86opr20ek0ijjzvn7` |
| `agent-pane-execution-base-4gb` | 4096 | 2 | `ujcvzqxszftbr47tx76e` |
| `agent-pane-execution-base-8gb` | 8192 | 4 | `0zce89ggh7g74rred802` |

`execution-e2b` pins these template IDs and defaults base creation to 2048 MiB.
