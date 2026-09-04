# execution-host-daemon

`execution-host-daemon` installs on a user-owned Registered Host. Its
`execution-host` binary maintains an authenticated outbound WebSocket to the
hosted Execution Gateway and embeds `execution-supervisor-core`; it does not
open an inbound server.

## Configure

Copy `execution-host.example.json` and set:

- `gateway`: public HTTP(S) URL of the Execution Gateway.
- `state_directory`: private identity, credential, and supervisor state.
- `roots`: local directories exposed through the execution filesystem.

Relative paths are resolved relative to the configuration file.

The daemon and embedded supervisor support Linux, macOS, and Windows. Unix hosts
use process-group signals. Windows maps interrupt, terminate, and kill requests
to native child termination. Windows PowerShell is required only for running the
shared conformance test; normal execution may use `cmd.exe`, PowerShell, or an
explicit executable.

## Install a published binary

Versioned release `0.1.0-dev-27ca5507` is published for Linux and macOS on
x86_64 and arm64, and Windows on x86_64. The installers download the matching
raw binary and verify its SHA-256 digest before replacing the destination.

Linux or macOS:

```sh
curl --fail --location --silent --show-error \
  https://downloads.acentric.dev/execution-host/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://downloads.acentric.dev/execution-host/install.ps1 | iex
```

The versioned directory also contains `.tar.gz`/`.zip` packages, individual
checksums, a combined `SHA256SUMS`, and a machine-readable `manifest.json`.
These development artifacts are SHA-256 protected but are not Apple-notarized
or Authenticode-signed. Production distribution should add platform signing
once release signing identities are configured.

## Register

Create a registration through `POST /v1/registered-hosts`, then exchange its
single-use token. Prefer the environment variable so the token is not placed in
shell history:

```sh
EXECUTION_HOST_REGISTRATION_TOKEN=... \
  execution-host --config execution-host.json register
```

The returned machine credential is stored in `state_directory` and is never
printed. Credential files use mode `0600` on Unix and inherit the configured
state directory's ACL on Windows, so that directory should be private to the
daemon account.

## Connect

```sh
execution-host --config execution-host.json connect
```

The daemon reconnects with bounded exponential backoff. It also enforces the
gateway's negotiated heartbeat deadline, so a half-open connection left behind
by sleep, wake, or a network change is discarded and reconnected automatically.
A network or gateway disconnect does not restart the embedded supervisor, so
owned processes and their output journals remain available after reconnection.
Restarting the daemon creates a new supervisor generation.

Use `doctor` to inspect registration state without printing the credential, and
`describe` to inspect the descriptor generated from the configured roots.

## End-to-end verification

The production-daemon test starts a real `execution-host` process, registers it
with an in-process Execution Gateway HTTP server backed by PostgreSQL, and sends
every version 1 wire operation through the gateway. The shared conformance suite
runs without skipped checks; the test also sends `describe`, verifies host
deletion disconnects the daemon, and waits for the daemon to exit cleanly.

Use an isolated test database because the test truncates the gateway tables:

```sh
EXECUTION_GATEWAY_TEST_DATABASE_URL=postgresql:///execution_gateway_daemon_e2e \
  cargo test -p execution-host-daemon --test registered_host_e2e \
  -- --ignored --nocapture
```

The repository workflow runs the component suite and this production-daemon
test natively on Ubuntu, macOS, and Windows.
