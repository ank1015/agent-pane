//! Shared JavaScript bindings and generated contract documentation.
mod generate;
pub use generate::{declarations, documentation, sites_declarations};
use platform_runtime_contracts::{self as t, capabilities as c};
use schemars::{JsonSchema, schema_for};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Clone, Serialize)]
pub struct Parameter {
    pub name: &'static str,
    pub optional: bool,
    pub schema: Value,
}
#[derive(Clone, Serialize)]
pub struct Method {
    pub name: &'static str,
    pub description: &'static str,
    pub mutation: bool,
    pub parameters: Vec<Parameter>,
    pub result: Value,
}
fn param<T: JsonSchema>(name: &'static str, optional: bool) -> Parameter {
    Parameter {
        name,
        optional,
        schema: serde_json::to_value(schema_for!(T)).unwrap(),
    }
}
fn method<T: JsonSchema>(
    name: &'static str,
    description: &'static str,
    mutation: bool,
    parameters: Vec<Parameter>,
) -> Method {
    let schema = schemars::generate::SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<T>();
    Method {
        name,
        description,
        mutation,
        parameters,
        result: serde_json::to_value(schema).unwrap(),
    }
}
fn mutation<T: JsonSchema>() -> Vec<Parameter> {
    vec![
        param::<T>("input", false),
        param::<c::MutationOptions>("options", true),
    ]
}
fn id(name: &'static str) -> Vec<Parameter> {
    vec![param::<Uuid>(name, false)]
}
fn page<T: JsonSchema>(name: &'static str) -> Vec<Parameter> {
    vec![param::<Uuid>(name, false), param::<T>("options", true)]
}

/// Common methods implemented by both authenticated transports. Legacy Sites
/// aliases and callbacks remain in its compatibility layer, outside this surface.
pub fn methods() -> Vec<Method> {
    vec![
        method::<t::CursorPage<t::Environment>>(
            "environments.list",
            "List this project's environment references without provisioning hosts.",
            false,
            vec![param::<c::PageOptions>("options", true)],
        ),
        method::<t::Environment>(
            "environments.get",
            "Get one project environment reference.",
            false,
            id("environmentId"),
        ),
        method::<t::CursorPage<c::Account>>(
            "accounts.list",
            "List available LLM accounts as credential-free metadata.",
            false,
            vec![param::<c::PageOptions>("options", true)],
        ),
        method::<t::CursorPage<t::Harness>>(
            "harnesses.list",
            "List the project's enabled harnesses and configuration declarations.",
            false,
            vec![param::<c::PageOptions>("options", true)],
        ),
        method::<t::Harness>(
            "harnesses.get",
            "Read a registered harness and its declared inputs and outputs.",
            false,
            vec![param::<String>("harnessId", false)],
        ),
        method::<c::StartOptions>(
            "harnesses.startOptions",
            "Discover supported accounts/models and permitted harness configuration fields.",
            false,
            vec![param::<String>("harnessId", false)],
        ),
        method::<c::SessionCreated>(
            "sessions.create",
            "Create a session with immutable configuration; optional initialInput creates its first run atomically. onComplete requires a site backend.",
            true,
            mutation::<c::CreateSession>(),
        ),
        method::<t::CursorPage<t::Session>>(
            "sessions.list",
            "Page through project sessions.",
            false,
            vec![param::<c::PageOptions>("options", true)],
        ),
        method::<t::Session>(
            "sessions.get",
            "Get session metadata and its current active run.",
            false,
            id("sessionId"),
        ),
        method::<t::MessagePage>(
            "sessions.messages",
            "Page through explicit message history, optionally filtered by run.",
            false,
            page::<c::MessageOptions>("sessionId"),
        ),
        method::<c::Statistics>(
            "sessions.stats",
            "Read recorded usage and wall time; absent metrics remain null.",
            false,
            id("sessionId"),
        ),
        method::<c::RunCreated>(
            "runs.create",
            "Create a run with canonical user input and an expected session revision. onComplete requires a site backend.",
            true,
            mutation::<c::CreateRun>(),
        ),
        method::<t::CursorPage<t::Run>>(
            "runs.list",
            "List a session's runs in cursor order.",
            false,
            page::<c::PageOptions>("sessionId"),
        ),
        method::<t::Run>(
            "runs.get",
            "Inspect one exact run, including terminal status.",
            false,
            id("runId"),
        ),
        method::<c::InputAccepted>(
            "runs.steer",
            "Append canonical user input to the selected live run; acceptance is not model consumption.",
            true,
            mutation::<c::SteerRun>(),
        ),
        method::<c::AbortAccepted>(
            "runs.abort",
            "Request cooperative abort of one exact run.",
            true,
            mutation::<c::AbortRun>(),
        ),
        method::<c::Statistics>(
            "runs.stats",
            "Read recorded usage and wall time for one run.",
            false,
            id("runId"),
        ),
        method::<t::RunOutputsPage>(
            "runs.outputs",
            "Read named immutable outputs. References do not grant resource access.",
            false,
            page::<c::OutputOptions>("runId"),
        ),
        method::<c::Sandbox>(
            "sandboxes.createFromSnapshot",
            "Accept durable sandbox provisioning from a project snapshot. Poll get until ready; never replace an evaluated workspace for verification.",
            true,
            mutation::<c::CreateSandboxFromSnapshot>(),
        ),
        method::<c::Sandbox>(
            "sandboxes.get",
            "Inspect sandbox readiness, expiry and confirmed termination.",
            false,
            id("sandboxId"),
        ),
        method::<c::Sandbox>(
            "sandboxes.terminate",
            "Request sandbox termination; poll terminationConfirmed before assuming deletion.",
            true,
            mutation::<c::SandboxId>(),
        ),
        method::<c::Execution>(
            "execution.bash",
            "Accept durable bash execution on an authorized host with absolute workdir. Returns immediately with a persistent handle.",
            true,
            mutation::<c::Bash>(),
        ),
        method::<c::Execution>(
            "execution.get",
            "Read durable command status and cancellation confirmation.",
            false,
            id("executionId"),
        ),
        method::<c::ExecutionOutput>(
            "execution.output",
            "Read bounded UTF-8 output pages; a live empty page retains a polling cursor.",
            false,
            page::<c::ExecutionOutputOptions>("executionId"),
        ),
        method::<c::Execution>(
            "execution.cancel",
            "Request cancellation; transport errors and acknowledgements alone do not confirm termination.",
            true,
            mutation::<c::ExecutionId>(),
        ),
    ]
}
impl Method {
    /// Root-local schema derived from shared Rust request contracts.
    pub fn argument_schema(&self) -> Value {
        let mut definitions = serde_json::Map::new();
        let mut properties = serde_json::Map::new();
        let mut required = vec![];
        for p in &self.parameters {
            let mut schema = p.schema.clone();
            if let Some(defs) = schema.as_object_mut().unwrap().remove("$defs") {
                definitions.extend(defs.as_object().unwrap().clone());
            }
            schema.as_object_mut().unwrap().remove("$schema");
            properties.insert(p.name.into(), schema);
            if !p.optional {
                required.push(p.name);
            }
        }
        json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":properties,"required":required,"additionalProperties":false,"$defs":definitions})
    }
}
/// Factory expression: `(dispatch) => platform`. Both guests evaluate these same
/// bytes before untrusted source, with dispatch captured in a private closure.
pub fn factory() -> String {
    let methods:Vec<Value>=methods().into_iter().map(|m|json!({"name":m.name,"parameters":m.parameters.iter().map(|p|json!({"name":p.name,"optional":p.optional})).collect::<Vec<_>>()})).collect();
    include_str!("factory.js").replace("/*METHODS*/", &json!(methods).to_string())
}
pub fn agent_factory() -> String {
    format!(
        "call => ({})((method,args) => call('platform.'+method,args))",
        factory()
    )
}

/// Agent-only authoring catalog. Snapshot and rollback operations intentionally
/// do not exist here or in deployed backend SDKs.
pub fn sites_methods() -> Vec<Method> {
    use t::sites_authoring as a;
    vec![
        method::<Value>(
            "sites.preview",
            "Get temporary content access for the live frontend. Backend calls still require the authenticated dashboard viewer bridge.",
            false,
            vec![],
        ),
        method::<a::Source>(
            "sites.read",
            "Read the bound site's live index.html and backend.js. No workspace or version argument is needed.",
            false,
            vec![],
        ),
        method::<Value>(
            "sites.applyPatch",
            "Apply ordinary patch text to index.html and/or backend.js and make the pair live. Check the returned status; conflict means read and patch again with a new operation key.",
            true,
            mutation::<a::Patch>(),
        ),
        method::<Value>(
            "sites.operation",
            "Inspect a saved edit operation without applying it again.",
            false,
            id("operationId"),
        ),
        method::<Value>(
            "sites.invoke",
            "Invoke the live backend. Writes and Platform calls are real effects; inspect interrupted invocations before repeating them.",
            true,
            mutation::<a::Request>(),
        ),
        method::<Value>(
            "sites.invocation",
            "Inspect one backend invocation, including its logs.",
            false,
            id("invocationId"),
        ),
        method::<Value>(
            "sites.logs",
            "Read recent bounded backend invocation summaries.",
            false,
            vec![],
        ),
        method::<Value>(
            "sites.query",
            "Run bounded read-only SQL against the live site database. Use LIMIT and explicit columns.",
            false,
            vec![param::<a::Sql>("input", false)],
        ),
        method::<Value>(
            "sites.execute",
            "Run one SQL data or schema statement on the live database. The write and its receipt commit together. Code snapshots do not undo database changes.",
            true,
            mutation::<a::Sql>(),
        ),
    ]
}
pub fn sites_factory() -> String {
    let methods:Vec<Value>=sites_methods().into_iter().map(|m|json!({"name":m.name,"parameters":m.parameters.iter().map(|p|json!({"name":p.name,"optional":p.optional})).collect::<Vec<_>>()})).collect();
    format!(
        "call => ({})((method,args)=>call(method,args)).sites",
        include_str!("factory.js").replace("/*METHODS*/", &json!(methods).to_string())
    )
}
