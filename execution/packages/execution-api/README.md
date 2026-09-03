# execution-api

Shared, provider-neutral public API and Host Daemon protocol types for
`execution-gateway` and its clients.

The execution operation body itself is not duplicated here: `POST /v1/hosts/{host_id}/operations`
uses `execution-wire::RequestEnvelope` and `execution-wire::ResponseEnvelope` directly.

The package also defines Registered Host enrollment responses and the small
WebSocket control envelope. Execution operations remain owned by
`execution-wire` rather than being redefined for the WebSocket transport.
