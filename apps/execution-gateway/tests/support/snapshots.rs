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

    let invalid = client
        .post(format!("{base}/v1/control/snapshots"))
        .bearer_auth("test-control")
        .json(&CreateSnapshotRequest {
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
