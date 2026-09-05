# Harnesses

- [basic-cc-tools-harness](basic-cc-tools-harness/): sequential read/write/edit/bash
  loop with provider-specific reasoning/cache policy and durable run checkpoints.
- [environments](environments/): machine and sandbox environment preparation,
  with host-targeted filesystem tools, web research, and recoverable provisioning.

These libraries implement `harness-runtime` and are compiled into the shared
worker. They do not host independent servers or own databases.
