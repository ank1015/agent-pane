# execution-supervisor

Small local IPC endpoint for `execution-supervisor-core`.

The executable is intended to be installed in an E2B Managed Host. It has no
provider API, hosted-service authentication, public network listener, or
tool-specific behavior.

## Commands

Start one supervisor generation:

```sh
execution-supervisor serve \
  --host-id host-123 \
  --root /home/user \
  --socket /tmp/agent-pane-execution/supervisor.sock \
  --state-dir /tmp/agent-pane-execution/state
```

Relay one NDJSON request from stdin and write one NDJSON response to stdout:

```sh
execution-supervisor rpc \
  --socket /tmp/agent-pane-execution/supervisor.sock
```

Probe a running supervisor:

```sh
execution-supervisor health \
  --socket /tmp/agent-pane-execution/supervisor.sock
```

Print executable and protocol versions:

```sh
execution-supervisor version
```

`serve` exposes one configured execution root in the first implementation. Its
root ID defaults to `workspace` and its display name defaults to `Workspace`.
The supervisor supports concurrent local connections, while operations on an
individual connection are dispatched in request order.

The state-directory and socket locks are operating-system locks. They prevent a
second active process from using the same state or endpoint and are released if
the process exits unexpectedly.
