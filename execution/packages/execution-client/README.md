# execution-client

An authenticated Rust client for executing filesystem and process operations on
hosts managed by `execution-gateway`.

`ExecutionClient` holds the gateway connection and shared API token.
`connect_host` performs a live `describe` and returns a `GatewayHostRuntime`
implementing `execution_core::ExecutionRuntime`, `FileSystem`, and `ProcessRuntime`.
Tools can accept `&dyn ExecutionRuntime`; their callers own client construction
and credential configuration.

## Usage

```rust,no_run
use std::time::Duration;
use execution_client::{ExecutionClient, ExecutionClientConfig};
use execution_core::{
    ExecutionPath, ExecutionRuntime, OperationContext, ReadFileRequest, RootId,
};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let config = ExecutionClientConfig::new(
    std::env::var("EXECUTION_GATEWAY_URL")?.parse()?,
    std::env::var("EXECUTION_GATEWAY_API_TOKEN")?,
);
let client = ExecutionClient::new(config)?;
let context = OperationContext::with_timeout(Duration::from_secs(30));
let host = client.connect_host(
    &context,
    std::env::var("EXECUTION_HOST_ID")?.parse()?,
).await?;

// Choose a root advertised by host.descriptor().roots.
let file = host.filesystem().read(&context, ReadFileRequest {
    path: ExecutionPath::new(RootId::new("workspace")?, "src/main.rs")?,
    offset: 0,
    max_bytes: 64 * 1024,
    follow_symlinks: true,
}).await?;
let bytes = file.data.as_slice();
# let _ = bytes;
# Ok(())
# }
```

The base URL is the deployment root, such as `https://execution.example.com/`.
An optional deployment path prefix is preserved. Do not include `/v1`; the
client appends `v1/hosts/{host_id}/operations`. Hosted IDs must be UUIDs.

The credential is the shared gateway API token. Registered Host daemon
credentials are used for daemon connections and cannot authorize this client.
HTTP requires explicitly setting `config.allow_insecure_http = true` for local
development. Redirects are not followed.

## Runtime interface

All request and result types come from `execution-core`:

| Surface | Operations |
| --- | --- |
| `descriptor()` | Identity, supervisor generation, OS, roots, features, limits |
| `filesystem()` | Stat, bounded read, conditional atomic write, create directory, remove, paginated list |
| `processes()` | Start, cursor-based read, write/close stdin, resize PTY, signal, terminate |

Cloning a client or host runtime shares the HTTP connection pool. It does not
share mutable output cursors or tool-session state. The package has no runtime
dependency on the gateway application, provider implementations, or supervisor.

## Operation semantics

### Gateway management

`list_accounts`, `list_hosts`/`get_host`, `create_host`, `resume_host`, and
`list_snapshots`/`get_snapshot`/`create_snapshot` use the same authenticated,
bounded, cancellable transport settings as execution. Types are re-exported
through `execution_client::api`; list queries use `HostFilter`/`SnapshotFilter`.
The configured base URL is still the deployment root, not `/v1`.

Creation requires a caller-owned idempotency key (1–255 printable ASCII
characters without spaces). Persist it and the request before dispatch; reuse
both after an ambiguous failure. No automatic retries or new keys are generated.
Lifecycle mutations return the accepted resource state, not a ready guarantee:
poll `get_host`/`get_snapshot` with a bounded caller-owned recovery policy.
Restoring a logical gateway snapshot uses its account; base creation can select
an account or use the gateway default. Base sources accept optional `ram` values
of `1024`, `2048`, `4096`, or `8192` MiB and default to `2048`. The top-level
optional `network_access` field applies to both base and snapshot creation and
defaults to `true`. For example:

```rust,no_run
use execution_client::api::{CreateExecutionHostRequest, E2bHostSource};

let request = CreateExecutionHostRequest {
    name: Some("worker".into()),
    source: E2bHostSource::Base {
        e2b_account_id: None,
        ram: Some(4096),
    },
    timeout_seconds: Some(3600),
    network_access: Some(false),
    metadata: serde_json::json!({}),
};
```

Snapshot sources intentionally have no RAM field because they retain the
source sandbox's hardware specification. Listing metadata does not resume hosts.

### Host execution

- **Connection:** performs a live protocol handshake and validates the host
  descriptor and identity. It does not create hosts. The gateway resumes paused
  E2B hosts on use, including this handshake. Other unavailable states return
  gateway errors. Resume waits are bounded; `HOST_RESUMING` is retryable.
- **Descriptor:** a snapshot from connection time. Connect again to refresh it.
  The client does not fence every operation to that snapshot's generation; the
  current wire contract has no generation field on filesystem requests or
  process starts. Existing process requests retain the generation from their
  execution handle and can report `ExecutionLost` after a supervisor restart.
- **Identity and retries:** operation IDs, execution IDs, and input write IDs
  remain caller-owned. The client generates only wire request IDs and never
  automatically retries, including on retryable errors. A lost response can
  leave the outcome of a mutation unknown. Supervisor deduplication is not a
  guarantee of exactly-once effects across supervisor restarts.
- **Cancellation:** `OperationContext` cancellation and deadlines bound the
  complete local request, including reading the response body. Cancellation does
  not send a remote cancellation instruction or imply that a dispatched
  mutation did not complete. Terminate processes explicitly when needed.
- **Timeouts:** the default per-request timeout is 60 seconds. The earlier of
  that timeout and the context deadline wins. Configure the timeout to allow
  your process-read `wait_ms` plus transport overhead. The process-start
  `timeout_ms` controls the remote process lifetime independently.
- **Output:** process reads return bounded, sequence-numbered events. Persist
  the execution handle and `next_sequence` as needed to recover after a caller
  restart. The client does not consume events in a background task.
- **Bounds:** requests retain core validation and host-enforced limits. Responses
  are limited to 32 MiB by default, including JSON/Base64 overhead, even when
  streamed without `Content-Length`. Adjust `max_response_bytes` when needed.

## Errors

Client construction returns `ConfigError`. Connecting and executing return
`execution_core::ExecutionResult<T>`:

- Supervisor errors preserve their code, message, retryability, and details.
- Gateway errors retain `gateway_code`, `gateway_details`, `http_status`, and
  `gateway_request_id` when provided, with `source: "gateway"` in details.
  A missing host maps to `NotFound`, rather than `ExecutionNotFound`.
- Transport failures map to `Unavailable` or `DeadlineExceeded`, with
  `source: "transport"`. Protocol failures have `source: "protocol"`.
- Invalid configuration, client debug output, and unstructured proxy error
  bodies do not expose the API token. Structured gateway and supervisor error
  messages are returned as supplied by those endpoints.

Provisioning, host lifecycle, snapshots, account management, tool-output
formatting, and harness session storage remain outside this package's initial
execution interface.

## Verification

From the `execution/` workspace:

```sh
cargo test -p execution-client
cargo clippy -p execution-client --all-targets -- -D warnings
```

The package's HTTP test runs the shared conformance suite against a real local
supervisor. Other tests exercise authentication, handshake validation, error
preservation, cancellation during headers/body reads, timeouts, response bounds,
and ambiguous mutation failures without replay.

The gateway's Registered Host and live E2B conformance tests, and the production
daemon end-to-end test, also use this client. Their PostgreSQL and live-provider
prerequisites are documented in the gateway README.
