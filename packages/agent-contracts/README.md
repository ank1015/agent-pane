# agent-contracts

Shared wire contracts for Agent and harness servers.

`harness_protocol` defines the NATS JetStream subjects and payloads for
`TurnRequested`, `HarnessCommand`, `HarnessCommandResult`, `RunCancelled`,
`HarnessRunEvent`, and the canonical replayable `RunEvent`.
Harness commands have exactly four operations: `complete`, `continue`, `fail`,
and `wait`.

The same module contains the lease-free HTTP contracts used only to read and
append canonical session messages. There are no compatibility contracts for
polling, claiming, leases, attempts, heartbeats, defer, or Agent-owned retries.
