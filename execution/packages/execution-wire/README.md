# execution-wire

Versioned request/response protocol for `execution-core` operations.

This crate contains:

- A stable protocol name and version.
- Validated request identifiers.
- Request, success-response, and error-response envelopes.
- A tagged operation enum using the request types from `execution-core`.
- A tagged result enum using the result types from `execution-core`.
- A bounded NDJSON codec for local sockets, stdin/stdout, and other byte streams.
- Shared dispatch from a wire operation to an `ExecutionRuntime`.

It intentionally contains no filesystem or process implementation, provider API,
connection manager, authentication, hosted persistence, or tool behavior.

`process_read` is a unary cursor-based operation. Transports may provide a
convenience stream by issuing repeated reads, but the wire protocol does not own
live process subscriptions.

## Example

```json
{"version":1,"request_id":"019...","operation":{"operation":"describe"}}
```

```json
{"status":"success","version":1,"request_id":"019...","result":{"result":"host_descriptor","value":{}}}
```

Frames are compact JSON followed by one newline. Binary fields are serialized by
`execution-core` with the standard Base64 alphabet.
