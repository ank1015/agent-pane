# Platform JavaScript SDK

Generated from the shared Rust capability contracts. Platform methods are shared by code-mode cells and site handlers. Sites methods are agent-only and require a session-bound Sites harness.

Mutations in site handlers require a stable `options.idempotencyKey`. Agent code mode assigns one before dispatch if omitted. Explicit keys are preserved. A mutation replay returns its acceptance receipt; getters return current state.

## ctx.platform.execution.listResources

List registered machines and sandbox accounts without credentials or provisioning. Each inventory contains at most 200 items, total and truncated. Requires Site execution access; discovery does not grant host access.

```ts
execution.listResources(): Promise<ExecutionResources>
```

Read: scoped by the trusted caller transport.

## ctx.platform.environments.list

List this project's environment references without provisioning hosts.

```ts
environments.list(options?: PageOptions): Promise<CursorPage_Environment>
```

Read: scoped by the trusted caller transport.

## ctx.platform.environments.get

Get one project environment reference.

```ts
environments.get(environmentId: Uuid): Promise<Environment>
```

Read: scoped by the trusted caller transport.

## ctx.platform.accounts.list

List available LLM accounts as credential-free metadata.

```ts
accounts.list(options?: PageOptions): Promise<CursorPage_Account>
```

Read: scoped by the trusted caller transport.

## ctx.platform.harnesses.list

List the project's enabled harnesses and configuration declarations.

```ts
harnesses.list(options?: PageOptions): Promise<CursorPage_Harness>
```

Read: scoped by the trusted caller transport.

## ctx.platform.harnesses.get

Read a registered harness and its declared inputs and outputs.

```ts
harnesses.get(harnessId: string): Promise<Harness>
```

Read: scoped by the trusted caller transport.

## ctx.platform.harnesses.startOptions

Discover supported accounts/models and permitted harness configuration fields.

```ts
harnesses.startOptions(harnessId: string): Promise<StartOptions>
```

Read: scoped by the trusted caller transport.

## ctx.platform.sessions.create

Create a session with immutable configuration; optional initialInput creates its first run atomically. onComplete requires a site backend.

```ts
sessions.create(input: CreateSession, options?: MutationOptions): Promise<SessionCreated>
```

Mutation: preserve its operation identity across retries.

## ctx.platform.sessions.list

Page through project sessions.

```ts
sessions.list(options?: PageOptions): Promise<CursorPage_Session>
```

Read: scoped by the trusted caller transport.

## ctx.platform.sessions.get

Get session metadata and its current active run.

```ts
sessions.get(sessionId: Uuid): Promise<Session>
```

Read: scoped by the trusted caller transport.

## ctx.platform.sessions.messages

Page through explicit message history, optionally filtered by run.

```ts
sessions.messages(sessionId: Uuid, options?: MessageOptions): Promise<MessagePage>
```

Read: scoped by the trusted caller transport.

## ctx.platform.sessions.stats

Read recorded usage and wall time; absent metrics remain null.

```ts
sessions.stats(sessionId: Uuid): Promise<Statistics>
```

Read: scoped by the trusted caller transport.

## ctx.platform.runs.create

Create a run with canonical user input and an expected session revision. onComplete requires a site backend.

```ts
runs.create(input: CreateRun, options?: MutationOptions): Promise<RunCreated>
```

Mutation: preserve its operation identity across retries.

## ctx.platform.runs.list

List a session's runs in cursor order.

```ts
runs.list(sessionId: Uuid, options?: PageOptions): Promise<CursorPage_Run>
```

Read: scoped by the trusted caller transport.

## ctx.platform.runs.get

Inspect one exact run, including terminal status.

```ts
runs.get(runId: Uuid): Promise<Run>
```

Read: scoped by the trusted caller transport.

## ctx.platform.runs.steer

Append canonical user input to the selected live run; acceptance is not model consumption.

```ts
runs.steer(input: SteerRun, options?: MutationOptions): Promise<MessageResponse>
```

Mutation: preserve its operation identity across retries.

## ctx.platform.runs.abort

Request cooperative abort of one exact run.

```ts
runs.abort(input: AbortRun, options?: MutationOptions): Promise<AbortResponse>
```

Mutation: preserve its operation identity across retries.

## ctx.platform.runs.stats

Read recorded usage and wall time for one run.

```ts
runs.stats(runId: Uuid): Promise<Statistics>
```

Read: scoped by the trusted caller transport.

## ctx.platform.runs.outputs

Read named immutable outputs. References do not grant resource access.

```ts
runs.outputs(runId: Uuid, options?: OutputOptions): Promise<RunOutputsPage>
```

Read: scoped by the trusted caller transport.

## ctx.platform.sandboxes.createFromSnapshot

Accept durable sandbox provisioning from a project snapshot. networkAccess defaults to true. Poll get until ready; sandboxes expire automatically. Prefer harness-provided workspaces when available.

```ts
sandboxes.createFromSnapshot(input: CreateSandboxFromSnapshot, options?: MutationOptions): Promise<Sandbox>
```

Mutation: preserve its operation identity across retries.

## ctx.platform.sandboxes.get

Inspect sandbox readiness, expiry and confirmed termination.

```ts
sandboxes.get(sandboxId: Uuid): Promise<Sandbox>
```

Read: scoped by the trusted caller transport.

## ctx.platform.execution.bash

Accept durable bash execution on an authorized host with absolute workdir. Returns immediately with a persistent handle.

```ts
execution.bash(input: Bash, options?: MutationOptions): Promise<Execution>
```

Mutation: preserve its operation identity across retries.

## ctx.platform.execution.get

Read durable command status and cancellation confirmation.

```ts
execution.get(executionId: Uuid): Promise<Execution>
```

Read: scoped by the trusted caller transport.

## ctx.platform.execution.output

Read bounded UTF-8 output pages; a live empty page retains a polling cursor.

```ts
execution.output(executionId: Uuid, options?: ExecutionOutputOptions): Promise<ExecutionOutput>
```

Read: scoped by the trusted caller transport.

## ctx.platform.execution.cancel

Request cancellation; transport errors and acknowledgements alone do not confirm termination.

```ts
execution.cancel(input: ExecutionId, options?: MutationOptions): Promise<Execution>
```

Mutation: preserve its operation identity across retries.

## ctx.sites.preview

Get temporary content access for the live frontend. Backend calls still require the authenticated dashboard viewer bridge.

```ts
sites.preview(): Promise<AnyValue>
```

Read: scoped by the trusted caller transport.

## ctx.sites.read

Read the bound site's live index.html and backend.js. No workspace or version argument is needed.

```ts
sites.read(): Promise<Source>
```

Read: scoped by the trusted caller transport.

## ctx.sites.applyPatch

Apply ordinary patch text to index.html and/or backend.js and make the pair live. Check the returned status; conflict means read and patch again with a new operation key.

```ts
sites.applyPatch(input: Patch, options?: MutationOptions): Promise<AnyValue>
```

Mutation: preserve its operation identity across retries.

## ctx.sites.operation

Inspect a saved edit operation without applying it again.

```ts
sites.operation(operationId: Uuid): Promise<AnyValue>
```

Read: scoped by the trusted caller transport.

## ctx.sites.invoke

Invoke the live backend. Writes and Platform calls are real effects; inspect interrupted invocations before repeating them.

```ts
sites.invoke(input: Request, options?: MutationOptions): Promise<AnyValue>
```

Mutation: preserve its operation identity across retries.

## ctx.sites.invocation

Inspect one backend invocation, including its logs.

```ts
sites.invocation(invocationId: Uuid): Promise<AnyValue>
```

Read: scoped by the trusted caller transport.

## ctx.sites.logs

Read recent bounded backend invocation summaries.

```ts
sites.logs(): Promise<AnyValue>
```

Read: scoped by the trusted caller transport.

## ctx.sites.query

Run bounded read-only SQL against the live site database. Use LIMIT and explicit columns.

```ts
sites.query(input: Sql): Promise<AnyValue>
```

Read: scoped by the trusted caller transport.

## ctx.sites.execute

Run one SQL data or schema statement on the live database. The write and its receipt commit together. Code snapshots do not undo database changes.

```ts
sites.execute(input: Sql, options?: MutationOptions): Promise<AnyValue>
```

Mutation: preserve its operation identity across retries.

