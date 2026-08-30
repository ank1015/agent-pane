# agent-harness-sdk

Reusable infrastructure for independent Agent harness servers.

Each harness remains its own binary, process, deployment, durable work
consumer, concurrency boundary, and runtime implementation. This package owns
the common Agent protocol machinery: JetStream turn consumption, command
delivery and result routing, cancellation, progress acknowledgements, active
turn tracking, and fenced transcript access.

A harness supplies a [`HarnessDescriptor`], a [`HarnessServerConfig`], and an
implementation of [`HarnessRuntime`]. The runtime receives an [`ActiveTurn`]
and returns a [`TurnOutcome`]; it never needs to publish lifecycle commands or
manage broker acknowledgements itself.
