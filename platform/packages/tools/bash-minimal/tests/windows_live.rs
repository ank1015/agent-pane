//! Opt-in, read-only command probe through a real gateway and Windows host.
use std::time::Duration;

use execution_client::{ExecutionClient, ExecutionClientConfig};
use execution_core::*;
use tool_bash_minimal::*;

#[tokio::test]
#[ignore = "requires EXECUTION_GATEWAY_URL, EXECUTION_GATEWAY_TOKEN and WINDOWS_HOST_ID"]
async fn powershell_and_advertised_windows_roots_work_through_gateway() {
    let client = ExecutionClient::new(ExecutionClientConfig::new(
        std::env::var("EXECUTION_GATEWAY_URL")
            .unwrap()
            .parse()
            .unwrap(),
        std::env::var("EXECUTION_GATEWAY_TOKEN").unwrap(),
    ))
    .unwrap();
    let ctx = OperationContext::with_timeout(Duration::from_secs(90));
    let host = client
        .connect_host(
            &ctx,
            ExecutionHostId::new(std::env::var("WINDOWS_HOST_ID").unwrap()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(host.descriptor().operating_system, OperatingSystem::Windows);
    let root = host.descriptor().roots.first().expect("registered root");
    let cwd = ExecutionPath::new(root.id.clone(), ".").unwrap();
    let tool = BashTool::new(&host, cwd, BashConfig::default()).unwrap();
    for workdir in [
        root.native_path.clone(),
        root.native_path
            .strip_prefix(r"\\?\")
            .unwrap_or(&root.native_path)
            .to_owned(),
    ] {
        let prepared = tool.prepare(BashInput {
            command: "$ErrorActionPreference = 'Stop'; Write-Output 'WINDOWS_BASH_PROBE_OK'; Write-Output (\"QUOTING=\" + \"works with spaces\"); Write-Output ('PowerShell=' + $PSVersionTable.PSVersion); Get-Location; Write-Output ([Environment]::GetFolderPath('Desktop'))".into(),
            workdir: Some(workdir), timeout: Some(15_000),
        }, BashIds { operation_id: OperationId::generate(), execution_id: ExecutionId::generate(), terminate_operation_id: OperationId::generate() }).unwrap();
        // Exercise checkpoint serialization as well as the actual tool request.
        let prepared: PreparedBash =
            serde_json::from_slice(&serde_json::to_vec(&prepared).unwrap()).unwrap();
        assert!(matches!(
            prepared.request().command,
            CommandSpec::Argv { .. }
        ));
        let mut running = tool.start(&ctx, &prepared).await.unwrap();
        let output = tool.wait(&ctx, &mut running).await.unwrap();
        assert!(!output.is_error(), "{}", output.to_text());
        assert_eq!(output.exit_code, Some(0));
        assert!(output.output.contains("WINDOWS_BASH_PROBE_OK"));
        assert!(output.output.contains("PowerShell="));
        assert!(output.output.contains("QUOTING=works with spaces"));
        println!("{}", output.to_text());
    }
}
