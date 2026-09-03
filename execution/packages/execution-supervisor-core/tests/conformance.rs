use execution_conformance::{ConformanceConfig, run_all};
use execution_core::{ExecutionHostId, RootId};
use execution_supervisor_core::{
    SupervisorConfig, SupervisorLimits, SupervisorRoot, SupervisorRuntime,
};

#[tokio::test]
async fn supervisor_runtime_passes_shared_conformance_suite() {
    let temporary = tempfile::tempdir().expect("create conformance directory");
    let root_directory = temporary.path().join("workspace");
    tokio::fs::create_dir(&root_directory)
        .await
        .expect("create conformance root");
    let root_id = RootId::new("workspace").expect("valid root ID");
    let runtime = SupervisorRuntime::new(SupervisorConfig {
        host_id: ExecutionHostId::generate(),
        state_directory: temporary.path().join("state"),
        roots: vec![SupervisorRoot {
            id: root_id,
            name: "Workspace".to_string(),
            path: root_directory,
            read_only: false,
        }],
        limits: SupervisorLimits::default(),
    })
    .await
    .expect("create supervisor runtime");
    let config = ConformanceConfig::for_runtime(&runtime).expect("build conformance config");

    let result = run_all(&runtime, &config).await;
    runtime.shutdown().await;

    let report = result.expect("supervisor runtime must satisfy execution-core");
    assert!(
        report.passed_checks().len() >= 9,
        "unexpected report: {report}"
    );
    assert!(
        report.skipped_checks().is_empty(),
        "supervisor unexpectedly skipped checks: {:?}",
        report.skipped_checks()
    );
}
