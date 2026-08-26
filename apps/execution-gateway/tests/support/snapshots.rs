use execution_gateway::{
    sandbox_accounts::SandboxProvider,
    snapshots::{CreateSnapshotRequest, Snapshot},
};
use reqwest::StatusCode;
use uuid::Uuid;

pub async fn assert_snapshot_control_api(
    client: &reqwest::Client,
    base: &str,
    e2b_account_id: Uuid,
    daytona_account_id: Uuid,
) -> Snapshot {
    let unauthorized = client
        .get(format!("{base}/v1/control/snapshots"))
        .bearer_auth("test-api")
        .send()
        .await
        .expect("unauthorized snapshot list");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let unauthorized_e2b_snapshot = client
        .post(format!(
            "{base}/v1/control/sandbox-accounts/{e2b_account_id}/sandboxes/missing-sandbox/snapshots"
        ))
        .bearer_auth("test-api")
        .json(&serde_json::json!({ "name": "Ready workspace" }))
        .send()
        .await
        .expect("unauthorized E2B snapshot create");
    assert_eq!(unauthorized_e2b_snapshot.status(), StatusCode::UNAUTHORIZED);

    let invalid_e2b_snapshot = client
        .post(format!(
            "{base}/v1/control/sandbox-accounts/{e2b_account_id}/sandboxes/missing-sandbox/snapshots"
        ))
        .bearer_auth("test-control")
        .json(&serde_json::json!({ "name": " Ready workspace" }))
        .send()
        .await
        .expect("invalid E2B snapshot create");
    assert_eq!(invalid_e2b_snapshot.status(), StatusCode::BAD_REQUEST);

    let missing_e2b_sandbox = client
        .post(format!(
            "{base}/v1/control/sandbox-accounts/{e2b_account_id}/sandboxes/missing-sandbox/snapshots"
        ))
        .bearer_auth("test-control")
        .json(&serde_json::json!({ "name": "Ready workspace" }))
        .send()
        .await
        .expect("missing E2B sandbox snapshot create");
    assert_eq!(missing_e2b_sandbox.status(), StatusCode::NOT_FOUND);

    let invalid = client
        .post(format!("{base}/v1/control/snapshots"))
        .bearer_auth("test-control")
        .json(&CreateSnapshotRequest {
            name: "Invalid snapshot".to_owned(),
            provider: SandboxProvider::E2b,
            sandbox_account_id: Some(e2b_account_id),
            provider_snapshot_id: " snapshot-1".to_owned(),
            sandbox_id: "sandbox-1".to_owned(),
        })
        .send()
        .await
        .expect("invalid snapshot create");
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let wrong_account = client
        .post(format!("{base}/v1/control/snapshots"))
        .bearer_auth("test-control")
        .json(&CreateSnapshotRequest {
            name: "Wrong account snapshot".to_owned(),
            provider: SandboxProvider::E2b,
            sandbox_account_id: Some(daytona_account_id),
            provider_snapshot_id: "snapshot-wrong-account".to_owned(),
            sandbox_id: "sandbox-wrong-account".to_owned(),
        })
        .send()
        .await
        .expect("mismatched snapshot account");
    assert_eq!(wrong_account.status(), StatusCode::BAD_REQUEST);

    let request = CreateSnapshotRequest {
        name: "Snapshot 1".to_owned(),
        provider: SandboxProvider::E2b,
        sandbox_account_id: Some(e2b_account_id),
        provider_snapshot_id: "snapshot-1".to_owned(),
        sandbox_id: "sandbox-1".to_owned(),
    };
    let response = client
        .post(format!("{base}/v1/control/snapshots"))
        .bearer_auth("test-control")
        .json(&request)
        .send()
        .await
        .expect("create snapshot");
    assert_eq!(response.status(), StatusCode::CREATED);
    let snapshot: Snapshot = response.json().await.expect("snapshot response");
    assert_eq!(snapshot.provider, SandboxProvider::E2b);
    assert_eq!(snapshot.name, "Snapshot 1");
    assert_eq!(snapshot.sandbox_account_id, e2b_account_id);
    assert_eq!(snapshot.provider_snapshot_id, "snapshot-1");
    assert_eq!(snapshot.sandbox_id, "sandbox-1");
    assert!(snapshot.created_at.0 > 0);

    let duplicate = client
        .post(format!("{base}/v1/control/snapshots"))
        .bearer_auth("test-control")
        .json(&request)
        .send()
        .await
        .expect("duplicate snapshot create");
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);

    let daytona_response = client
        .post(format!("{base}/v1/control/snapshots"))
        .bearer_auth("test-control")
        .json(&serde_json::json!({
            "name": "Daytona snapshot",
            "provider": "daytona",
            "snapshot_sandbox_id": "snapshot-1",
            "sandbox_id": "sandbox-1"
        }))
        .send()
        .await
        .expect("create provider-scoped snapshot");
    assert_eq!(daytona_response.status(), StatusCode::CREATED);
    let daytona: Snapshot = daytona_response
        .json()
        .await
        .expect("Daytona snapshot response");
    assert_eq!(daytona.provider, SandboxProvider::Daytona);
    assert_eq!(daytona.sandbox_account_id, daytona_account_id);
    assert_eq!(daytona.provider_snapshot_id, "snapshot-1");

    let filtered: Vec<Snapshot> = client
        .get(format!(
            "{base}/v1/control/snapshots?provider=e2b&sandbox_id=sandbox-1"
        ))
        .bearer_auth("test-control")
        .send()
        .await
        .expect("filter snapshots")
        .json()
        .await
        .expect("filtered snapshot list");
    assert_eq!(filtered.len(), 1);
    assert_eq!(&filtered[0], &snapshot);

    let fetched: Snapshot = client
        .get(format!("{base}/v1/control/snapshots/{}", snapshot.id))
        .bearer_auth("test-control")
        .send()
        .await
        .expect("get snapshot")
        .json()
        .await
        .expect("snapshot get response");
    assert_eq!(fetched, snapshot);

    let invalid_rename = client
        .patch(format!("{base}/v1/control/snapshots/{}", snapshot.id))
        .bearer_auth("test-control")
        .json(&serde_json::json!({ "name": " Renamed snapshot" }))
        .send()
        .await
        .expect("invalid snapshot rename");
    assert_eq!(invalid_rename.status(), StatusCode::BAD_REQUEST);

    let renamed: Snapshot = client
        .patch(format!("{base}/v1/control/snapshots/{}", snapshot.id))
        .bearer_auth("test-control")
        .json(&serde_json::json!({ "name": "Renamed snapshot" }))
        .send()
        .await
        .expect("rename snapshot")
        .json()
        .await
        .expect("renamed snapshot response");
    assert_eq!(renamed.id, snapshot.id);
    assert_eq!(renamed.name, "Renamed snapshot");

    let deleted = client
        .delete(format!("{base}/v1/control/snapshots/{}", snapshot.id))
        .bearer_auth("test-control")
        .send()
        .await
        .expect("delete snapshot");
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    let missing = client
        .get(format!("{base}/v1/control/snapshots/{}", snapshot.id))
        .bearer_auth("test-control")
        .send()
        .await
        .expect("get deleted snapshot");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    daytona
}
