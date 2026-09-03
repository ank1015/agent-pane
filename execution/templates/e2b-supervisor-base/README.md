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
e2b template create agent-pane-execution-base \
  --path execution/templates/e2b-supervisor-base \
  --dockerfile Dockerfile

e2b template publish agent-pane-execution-base --yes
```

Published template:

```text
Name: sugars-project/agent-pane-execution-base
ID:   uybwrhggvlhkmlbr27qw
```

`execution-e2b` pins this template ID for base creation.
