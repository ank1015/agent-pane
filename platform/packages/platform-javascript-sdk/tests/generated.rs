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
