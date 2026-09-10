#[test]
fn execution_resource_discovery_is_a_no_argument_read() {
    let methods = platform_javascript_sdk::methods();
    let method = methods
        .iter()
        .find(|m| m.name == "execution.listResources")
        .unwrap();
    assert!(!method.mutation);
    assert!(method.parameters.is_empty());
    let declarations = platform_javascript_sdk::declarations();
    assert!(declarations.contains("listResources(): Promise<ExecutionResources>"));
    assert!(declarations.contains("ResourceInventory_ExecutionMachine"));
    assert!(declarations.contains("ResourceInventory_SandboxAccount"));
}

#[test]
fn sandbox_sdk_keeps_readiness_and_exposes_optional_network_access_without_termination() {
    let methods = platform_javascript_sdk::methods();
    assert!(methods.iter().any(|m| m.name == "sandboxes.get"));
    assert!(!methods.iter().any(|m| m.name == "sandboxes.terminate"));
    let create = methods
        .iter()
        .find(|m| m.name == "sandboxes.createFromSnapshot")
        .unwrap();
    let input = &create.parameters[0].schema;
    assert!(input["properties"].get("networkAccess").is_some());
    assert!(
        !input["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "networkAccess")
    );
    let declarations = platform_javascript_sdk::declarations();
    assert!(declarations.contains("\"networkAccess\"?: boolean | null"));
    assert!(!declarations.contains("terminate("));
}

#[test]
fn generated_contracts_documentation_and_backend_facade_are_current() {
    assert_eq!(
        platform_javascript_sdk::sites_declarations(),
        include_str!("../sites.d.ts")
    );
    assert!(
        !platform_javascript_sdk::sites_methods()
            .iter()
            .any(|m| m.name.contains("snapshot") || m.name.contains("restore"))
    );
    let declarations = platform_javascript_sdk::declarations();
    assert_eq!(declarations, include_str!("../platform.d.ts"));
    assert_eq!(
        platform_javascript_sdk::documentation(),
        include_str!("../METHODS.md")
    );
    let backend = format!(
        "{}\n{}\n{}",
        include_str!("../../../apps/sites-service/src/sdk.base.d.ts"),
        declarations,
        include_str!("../../../apps/sites-service/src/sdk.compat.d.ts")
    );
    assert_eq!(
        backend,
        include_str!("../../../apps/sites-service/src/sdk.d.ts")
    );
}
