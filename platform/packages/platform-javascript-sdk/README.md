# Shared Platform JavaScript SDK

`methods()` associates common method names and argument mappings with shared Rust
capability DTOs. Optional `schema` derives generate JSON Schemas from the runtime
contracts and canonical LLM user-content types. The method catalog drives:

- The same frozen JavaScript factory in agent cells and site handlers.
- Parent-side code-mode input validation and tool discovery.
- [platform.d.ts](platform.d.ts) and [METHODS.md](METHODS.md).
- The common portion of the site's standalone `sdk.d.ts`.

`Platform` is the common interface; `AgentPlatform` permits omitted mutation keys
because its trusted adapter creates and journals them before dispatch. Backend
calls require explicit stable keys. Existing backend aliases and callback APIs
remain in `sdk.compat.d.ts`; they are site-specific extensions to the common SDK.
Site database and invocation APIs remain in `sdk.base.d.ts`.

From `platform/`:

```sh
cargo run -p platform-javascript-sdk --bin generate
cargo run -p platform-javascript-sdk --bin generate -- --check
cargo test -p platform-javascript-sdk
node apps/dashboard/node_modules/typescript/bin/tsc --noEmit --strict --target ES2022 --allowJs --checkJs packages/platform-javascript-sdk/examples/types.ts packages/platform-javascript-sdk/examples/project-helper.js
```

Generated artifacts are checked by tests. [project-helper.js](examples/project-helper.js)
is bundled unchanged into both real guest contexts in server integration tests.
JSON message-history/native-provider payloads remain open JSON; canonical user
inputs and their text/image content have generated types. Authorization, model
compatibility, current grants and runtime state remain server responsibilities.

Agent-only `sites_methods()` and `sites_factory()` add the session-bound authoring SDK. Its generated declarations are in [sites.d.ts](sites.d.ts). It is not included in deployed backend contexts; snapshot/restore methods remain exclusively in user-facing HTTP APIs.
