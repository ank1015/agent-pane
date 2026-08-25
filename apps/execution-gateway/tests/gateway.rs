use std::{sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use credential_vault::CredentialVault;
use execution_contracts::{MachineId, WorkspaceRootId};
use execution_gateway::{
    config::Config,
    connection_registry::ConnectionRegistry,
    connectors::ConnectorRouter,
    db::Database,
    routing::OperationRouter,
    sandbox_accounts::{SandboxAccount, SandboxAccountService},
};
use execution_local::{LocalExecutionRuntime, LocalRuntimeConfig, LocalWorkspaceRoot};
use execution_protocol::{
    CreateEnvironmentRequest, CreateOperationRequest, CreateRegistrationRequest, Environment,
    MachineSummary, Operation, OperationRecord, OperationStatus, RegistrationCreated, Response,
};
use execution_runtime::ExecutionRuntime;
use reqwest::StatusCode;
use tokio::net::TcpListener;

#[path = "support/sandbox_templates.rs"]
mod sandbox_templates;
#[path = "support/snapshots.rs"]
mod snapshots;

fn id<T>(value: String) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value).expect("valid identifier")
}

#[tokio::test]
#[ignore = "requires EXECUTION_GATEWAY_TEST_DATABASE_URL pointing to an empty PostgreSQL database"]
async fn controls_sandbox_accounts_and_routes_a_real_daemon_operation() {
    let database_url = std::env::var("EXECUTION_GATEWAY_TEST_DATABASE_URL")
        .expect("EXECUTION_GATEWAY_TEST_DATABASE_URL");
    let database = Database::connect(&database_url, 4).await.expect("database");
    database.migrate().await.expect("migrations");
    let vault = CredentialVault::from_base64(&STANDARD.encode([3_u8; 32])).expect("vault");
    let account_service = SandboxAccountService::new(database.clone(), vault.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let config = Config {
        bind_address: address,
        database_url,
        database_max_connections: 4,
        admin_token: "test-admin".to_owned(),
        api_token: "test-api".to_owned(),
        control_token: "test-control".to_owned(),
        vault,
        daemon_websocket_url: format!("ws://{address}/v1/machines/connect"),
        registration_ttl: Duration::from_secs(300),
        max_request_bytes: 1024 * 1024,
    };
    let connections = ConnectionRegistry::default();
    let connectors = ConnectorRouter::new(connections.clone());
    let operation_router = OperationRouter::new(database.clone(), connectors);
    let router =
        execution_gateway::http::router(&config, database.clone(), connections, operation_router);
    let server = tokio::spawn(async move { axum::serve(listener, router).await });
    let base = format!("http://{address}");
    let client = reqwest::Client::new();

    let unauthorized = client
        .get(format!("{base}/v1/control/sandbox-accounts"))
        .bearer_auth("test-api")
        .send()
        .await
        .expect("unauthorized account list");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let fixtures = [
        ("e2b", "E2B main", "e2b-secret"),
        ("daytona", "Daytona main", "daytona-secret"),
        ("blaxel", "Blaxel main", "blaxel-secret"),
        ("tensorlake", "Tensorlake main", "tensorlake-secret"),
    ];
    let mut accounts = Vec::new();
    for (provider, name, api_key) in fixtures {
        let response = client
            .post(format!("{base}/v1/control/sandbox-accounts"))
            .bearer_auth("test-control")
            .json(&serde_json::json!({
                "provider": provider,
                "name": name,
                "api_key": api_key,
            }))
            .send()
            .await
            .expect("create sandbox account");
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = response.text().await.expect("account response");
        assert!(!body.contains(api_key));
        let account: SandboxAccount = serde_json::from_str(&body).expect("sandbox account");
        assert!(account.is_default);
        let decrypted = account_service
            .credentials(account.id)
            .await
            .expect("decrypt credentials")
            .expect("credentials exist");
        assert_eq!(decrypted.api_key(), api_key);
        accounts.push(account);
    }

    let template_snapshot =
        snapshots::assert_snapshot_control_api(&client, &base, accounts[0].id, accounts[1].id)
            .await;
    sandbox_templates::assert_template_control_api(&client, &base, &template_snapshot).await;

    let duplicate = client
        .post(format!("{base}/v1/control/sandbox-accounts"))
        .bearer_auth("test-control")
        .json(&serde_json::json!({
            "provider": "e2b",
            "name": "e2b MAIN",
            "api_key": "duplicate-secret",
        }))
        .send()
        .await
        .expect("duplicate sandbox account");
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);

    let secondary_response = client
        .post(format!("{base}/v1/control/sandbox-accounts"))
        .bearer_auth("test-control")
        .json(&serde_json::json!({
            "provider": "e2b",
            "name": "E2B secondary",
            "api_key": "e2b-secondary-secret",
        }))
        .send()
        .await
        .expect("create secondary account");
    assert_eq!(secondary_response.status(), StatusCode::CREATED);
    let secondary: SandboxAccount = secondary_response.json().await.expect("secondary account");
    assert!(!secondary.is_default);

    let list_response = client
        .get(format!("{base}/v1/control/sandbox-accounts"))
        .bearer_auth("test-control")
        .send()
        .await
        .expect("list sandbox accounts");
    let list_body = list_response.text().await.expect("account list response");
    for (_, _, api_key) in fixtures {
        assert!(!list_body.contains(api_key));
    }
    let listed: Vec<SandboxAccount> = serde_json::from_str(&list_body).expect("account list");
    assert_eq!(listed.len(), 5);

    let rotate_response = client
        .put(format!(
            "{base}/v1/control/sandbox-accounts/{}/credentials",
            accounts[0].id
        ))
        .bearer_auth("test-control")
        .json(&serde_json::json!({"api_key": "e2b-rotated-secret"}))
        .send()
        .await
        .expect("rotate credentials");
    assert_eq!(rotate_response.status(), StatusCode::OK);
    let rotated_body = rotate_response.text().await.expect("rotation response");
    assert!(!rotated_body.contains("e2b-rotated-secret"));
    let rotated: SandboxAccount = serde_json::from_str(&rotated_body).expect("rotated account");
    assert_eq!(rotated.credential_version, 2);
    assert_eq!(
        account_service
            .credentials(accounts[0].id)
            .await
            .expect("decrypt rotated credentials")
            .expect("rotated credentials exist")
            .api_key(),
        "e2b-rotated-secret"
    );

    let inspection_pool = sqlx::PgPool::connect(&config.database_url)
        .await
        .expect("inspection database connection");
    let ciphertexts: Vec<Vec<u8>> = sqlx::query_scalar(
        "select encrypted_payload from execution_vault_secrets order by created_at",
    )
    .fetch_all(&inspection_pool)
    .await
    .expect("encrypted payloads");
    assert!(ciphertexts.iter().all(|ciphertext| {
        fixtures.iter().all(|(_, _, api_key)| {
            !ciphertext
                .windows(api_key.len())
                .any(|window| window == api_key.as_bytes())
        })
    }));

    let delete_response = client
        .delete(format!(
            "{base}/v1/control/sandbox-accounts/{}",
            secondary.id
        ))
        .bearer_auth("test-control")
        .send()
        .await
        .expect("delete secondary account");
    assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);
    let deleted_response = client
        .get(format!(
            "{base}/v1/control/sandbox-accounts/{}",
            secondary.id
        ))
        .bearer_auth("test-control")
        .send()
        .await
        .expect("get deleted account");
    assert_eq!(deleted_response.status(), StatusCode::NOT_FOUND);

    let registration_response = client
        .post(format!("{base}/v1/admin/machine-registrations"))
        .bearer_auth("test-admin")
        .json(&CreateRegistrationRequest {
            label: Some("integration fixture".to_owned()),
            expires_in_seconds: None,
        })
        .send()
        .await
        .expect("create registration");
    assert_eq!(registration_response.status(), StatusCode::CREATED);
    let registration: RegistrationCreated = registration_response
        .json()
        .await
        .expect("registration response");

    let directory = tempfile::tempdir().expect("temporary directory");
    let workspace = directory.path().join("workspace");
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace");
    let machine_id = id::<MachineId>(format!("machine-{}", uuid::Uuid::now_v7()));
    let workspace_root_id = id::<WorkspaceRootId>("workspace".to_owned());
    let runtime = Arc::new(
        LocalExecutionRuntime::new(LocalRuntimeConfig {
            machine_id: machine_id.clone(),
            name: "Integration daemon".to_owned(),
            state_directory: directory.path().join("state"),
            workspace_roots: vec![LocalWorkspaceRoot {
                id: workspace_root_id.clone(),
                name: "Workspace".to_owned(),
                path: workspace,
                read_only: false,
            }],
            native_grants: Vec::new(),
        })
        .await
        .expect("execution runtime"),
    );
    let credential = machine_daemon::registration::register(
        &base,
        registration.registration_token,
        runtime.descriptor(),
        &directory.path().join("state"),
    )
    .await
    .expect("claim registration");
    let daemon = tokio::spawn(machine_daemon::transport::connect::run(
        Arc::clone(&runtime),
        credential.websocket_url,
        Some(credential.credential),
    ));

    let machines = wait_for_machines(&client, &base).await;
    assert_eq!(machines.len(), 1);
    assert_eq!(machines[0].machine_id, machine_id);
    assert!(machines[0].online);

    let inventory_response = client
        .get(format!("{base}/v1/control/machines"))
        .bearer_auth("test-control")
        .send()
        .await
        .expect("machine inventory");
    assert_eq!(inventory_response.status(), StatusCode::OK);
    let inventory_body = inventory_response
        .text()
        .await
        .expect("machine inventory response");
    for (_, _, api_key) in fixtures {
        assert!(!inventory_body.contains(api_key));
    }
    let inventory: serde_json::Value =
        serde_json::from_str(&inventory_body).expect("machine inventory JSON");
    assert_eq!(inventory["connector_accounts"].as_array().unwrap().len(), 4);
    let daemons = inventory["machine_daemons"].as_array().unwrap();
    assert_eq!(daemons.len(), 1);
    assert_eq!(daemons[0]["machine_id"], machine_id.as_str());
    assert_eq!(daemons[0]["connector"], "machine_daemon");
    assert_eq!(daemons[0]["online"], true);

    let create = client
        .post(format!(
            "{base}/v1/machines/{}/operations",
            machine_id.as_str()
        ))
        .bearer_auth("test-api")
        .json(&CreateOperationRequest {
            operation: Box::new(Operation::Describe),
        })
        .send()
        .await
        .expect("create operation");
    assert_eq!(create.status(), StatusCode::ACCEPTED);
    let operation: OperationRecord = create.json().await.expect("operation response");
    let completed = wait_for_operation(&client, &base, &operation.operation_id).await;
    assert_eq!(completed.status, OperationStatus::Completed);
    assert!(matches!(
        completed.response,
        Some(Response::MachineDescriptor(_))
    ));

    let unauthorized_environment = client
        .post(format!("{base}/v1/control/environments"))
        .bearer_auth("test-api")
        .json(&CreateEnvironmentRequest {
            machine_id: machine_id.clone(),
            workspace_root_id: workspace_root_id.clone(),
            path: "project".to_owned(),
        })
        .send()
        .await
        .expect("unauthorized environment create");
    assert_eq!(unauthorized_environment.status(), StatusCode::UNAUTHORIZED);

    let invalid_root = client
        .post(format!("{base}/v1/control/environments"))
        .bearer_auth("test-control")
        .json(&CreateEnvironmentRequest {
            machine_id: machine_id.clone(),
            workspace_root_id: id("missing-root".to_owned()),
            path: "project".to_owned(),
        })
        .send()
        .await
        .expect("invalid environment root");
    assert_eq!(invalid_root.status(), StatusCode::BAD_REQUEST);

    let create_environment = client
        .post(format!("{base}/v1/control/environments"))
        .bearer_auth("test-control")
        .json(&CreateEnvironmentRequest {
            machine_id: machine_id.clone(),
            workspace_root_id: workspace_root_id.clone(),
            path: "project/./".to_owned(),
        })
        .send()
        .await
        .expect("create environment");
    assert_eq!(create_environment.status(), StatusCode::CREATED);
    let environment: Environment = create_environment
        .json()
        .await
        .expect("environment response");
    assert_eq!(environment.machine_id, machine_id);
    assert_eq!(environment.workspace_root_id, workspace_root_id);
    assert_eq!(environment.path, "project");

    let duplicate = client
        .post(format!("{base}/v1/control/environments"))
        .bearer_auth("test-control")
        .json(&CreateEnvironmentRequest {
            machine_id: machine_id.clone(),
            workspace_root_id: workspace_root_id.clone(),
            path: "project".to_owned(),
        })
        .send()
        .await
        .expect("duplicate environment");
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);

    let environments: Vec<Environment> = client
        .get(format!(
            "{base}/v1/control/environments?machine_id={}",
            machine_id.as_str()
        ))
        .bearer_auth("test-control")
        .send()
        .await
        .expect("list environments")
        .json()
        .await
        .expect("environment list");
    assert_eq!(environments.len(), 1);
    assert_eq!(&environments[0], &environment);

    let fetched: Environment = client
        .get(format!(
            "{base}/v1/environments/{}",
            environment.environment_id.as_str()
        ))
        .bearer_auth("test-api")
        .send()
        .await
        .expect("get operational environment")
        .json()
        .await
        .expect("operational environment response");
    assert_eq!(fetched, environment);

    let create = client
        .post(format!(
            "{base}/v1/environments/{}/operations",
            environment.environment_id.as_str()
        ))
        .bearer_auth("test-api")
        .json(&CreateOperationRequest {
            operation: Box::new(Operation::Describe),
        })
        .send()
        .await
        .expect("create environment operation");
    assert_eq!(create.status(), StatusCode::ACCEPTED);
    let operation: OperationRecord = create.json().await.expect("environment operation response");
    assert_eq!(
        operation.environment_id.as_ref(),
        Some(&environment.environment_id)
    );
    let completed = wait_for_operation(&client, &base, &operation.operation_id).await;
    assert_eq!(
        completed.environment_id,
        Some(environment.environment_id.clone())
    );
    assert_eq!(completed.status, OperationStatus::Completed);

    let deleted = client
        .delete(format!(
            "{base}/v1/control/environments/{}",
            environment.environment_id.as_str()
        ))
        .bearer_auth("test-control")
        .send()
        .await
        .expect("delete environment");
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    let replacement: Environment = client
        .post(format!("{base}/v1/control/environments"))
        .bearer_auth("test-control")
        .json(&CreateEnvironmentRequest {
            machine_id: machine_id.clone(),
            workspace_root_id: workspace_root_id.clone(),
            path: "project".to_owned(),
        })
        .send()
        .await
        .expect("recreate environment location")
        .json()
        .await
        .expect("replacement environment");
    assert_ne!(replacement.environment_id, environment.environment_id);

    let deleted_machine = client
        .delete(format!(
            "{base}/v1/control/machines/{}",
            machine_id.as_str()
        ))
        .bearer_auth("test-control")
        .send()
        .await
        .expect("delete machine");
    assert_eq!(deleted_machine.status(), StatusCode::NO_CONTENT);
    let deleted_with_machine = client
        .get(format!(
            "{base}/v1/environments/{}",
            replacement.environment_id.as_str()
        ))
        .bearer_auth("test-api")
        .send()
        .await
        .expect("get environment deleted with machine");
    assert_eq!(deleted_with_machine.status(), StatusCode::NOT_FOUND);

    let registration: RegistrationCreated = client
        .post(format!("{base}/v1/admin/machine-registrations"))
        .bearer_auth("test-admin")
        .json(&CreateRegistrationRequest {
            label: Some("re-registration fixture".to_owned()),
            expires_in_seconds: None,
        })
        .send()
        .await
        .expect("create re-registration")
        .json()
        .await
        .expect("re-registration response");
    machine_daemon::registration::register(
        &base,
        registration.registration_token,
        runtime.descriptor(),
        &directory.path().join("state"),
    )
    .await
    .expect("re-register machine");
    let still_deleted = client
        .get(format!(
            "{base}/v1/environments/{}",
            replacement.environment_id.as_str()
        ))
        .bearer_auth("test-api")
        .send()
        .await
        .expect("get environment after machine re-registration");
    assert_eq!(still_deleted.status(), StatusCode::NOT_FOUND);

    daemon.abort();
    server.abort();
}

async fn wait_for_machines(client: &reqwest::Client, base: &str) -> Vec<MachineSummary> {
    for _ in 0..100 {
        let response = client
            .get(format!("{base}/v1/machines"))
            .bearer_auth("test-api")
            .send()
            .await
            .expect("list machines");
        let machines: Vec<MachineSummary> = response.json().await.expect("machine list");
        if machines.iter().any(|machine| machine.online) {
            return machines;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("daemon did not connect");
}

async fn wait_for_operation(
    client: &reqwest::Client,
    base: &str,
    operation_id: &str,
) -> OperationRecord {
    for _ in 0..100 {
        let operation: OperationRecord = client
            .get(format!("{base}/v1/operations/{operation_id}"))
            .bearer_auth("test-api")
            .send()
            .await
            .expect("get operation")
            .json()
            .await
            .expect("operation record");
        if matches!(
            operation.status,
            OperationStatus::Completed | OperationStatus::Failed
        ) {
            return operation;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("operation did not finish");
}
