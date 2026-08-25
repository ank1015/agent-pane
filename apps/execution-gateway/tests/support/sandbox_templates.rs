use execution_gateway::{
    sandbox_templates::{SandboxEnvironmentInstance, SandboxEnvironmentTemplate},
    snapshots::Snapshot,
};
use reqwest::StatusCode;

pub async fn assert_template_control_api(
    client: &reqwest::Client,
    base: &str,
    snapshot: &Snapshot,
) {
    let unauthorized = client
        .get(format!("{base}/v1/control/sandbox-environment-templates"))
        .bearer_auth("test-api")
        .send()
        .await
        .expect("unauthorized template list");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let create = client
        .post(format!("{base}/v1/control/sandbox-environment-templates"))
        .bearer_auth("test-control")
        .json(&serde_json::json!({
            "name": "TypeScript project",
            "snapshot_id": snapshot.id,
            "cwd": "/workspace/./project/",
            "creation_script": "npm install"
        }))
        .send()
        .await
        .expect("create template");
    assert_eq!(create.status(), StatusCode::CREATED);
    let template: SandboxEnvironmentTemplate = create.json().await.expect("template response");
    assert_eq!(template.snapshot_id, snapshot.id);
    assert_eq!(template.sandbox_account_id, snapshot.sandbox_account_id);
    assert_eq!(template.provider, snapshot.provider);
    assert_eq!(template.cwd, "/workspace/project");

    let duplicate = client
        .post(format!("{base}/v1/control/sandbox-environment-templates"))
        .bearer_auth("test-control")
        .json(&serde_json::json!({
            "name": "typescript PROJECT",
            "snapshot_id": snapshot.id,
            "cwd": "/workspace"
        }))
        .send()
        .await
        .expect("duplicate template");
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);

    let update = client
        .patch(format!(
            "{base}/v1/control/sandbox-environment-templates/{}",
            template.id
        ))
        .bearer_auth("test-control")
        .json(&serde_json::json!({
            "name": "TypeScript workspace",
            "creation_script": ""
        }))
        .send()
        .await
        .expect("update template");
    assert_eq!(update.status(), StatusCode::OK);
    let updated: SandboxEnvironmentTemplate = update.json().await.expect("updated template");
    assert_eq!(updated.name, "TypeScript workspace");
    assert!(updated.creation_script.is_empty());

    let listed: Vec<SandboxEnvironmentTemplate> = client
        .get(format!("{base}/v1/control/sandbox-environment-templates"))
        .bearer_auth("test-control")
        .send()
        .await
        .expect("list templates")
        .json()
        .await
        .expect("template list");
    assert!(listed.iter().any(|item| item.id == template.id));

    let instances: Vec<SandboxEnvironmentInstance> = client
        .get(format!(
            "{base}/v1/control/sandbox-environment-templates/{}/environments",
            template.id
        ))
        .bearer_auth("test-control")
        .send()
        .await
        .expect("list template instances")
        .json()
        .await
        .expect("template instance list");
    assert!(instances.is_empty());

    let snapshot_in_use = client
        .delete(format!("{base}/v1/control/snapshots/{}", snapshot.id))
        .bearer_auth("test-control")
        .send()
        .await
        .expect("delete snapshot in use");
    assert_eq!(snapshot_in_use.status(), StatusCode::CONFLICT);

    let deleted = client
        .delete(format!(
            "{base}/v1/control/sandbox-environment-templates/{}",
            template.id
        ))
        .bearer_auth("test-control")
        .send()
        .await
        .expect("delete template");
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    let missing = client
        .get(format!(
            "{base}/v1/control/sandbox-environment-templates/{}",
            template.id
        ))
        .bearer_auth("test-control")
        .send()
        .await
        .expect("get deleted template");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}
