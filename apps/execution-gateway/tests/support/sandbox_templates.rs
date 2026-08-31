use execution_gateway::{
    sandbox_accounts::SandboxProvider,
    sandbox_templates::{SandboxEnvironmentInstance, SandboxEnvironmentTemplate},
    snapshots::Snapshot,
};
use reqwest::StatusCode;

pub async fn assert_template_control_api(
    client: &reqwest::Client,
    base: &str,
    snapshot: &Snapshot,
    project_id: uuid::Uuid,
) {
    let workspace_root = provider_workspace_root(snapshot.provider);
    let project_cwd = format!("{workspace_root}/project");
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
            "project_id": project_id,
            "name": "TypeScript project",
            "snapshot_id": snapshot.id,
            "cwd": format!("{workspace_root}/./project/"),
            "creation_script": "npm install"
        }))
        .send()
        .await
        .expect("create template");
    assert_eq!(create.status(), StatusCode::CREATED);
    let template: SandboxEnvironmentTemplate = create.json().await.expect("template response");
    assert_eq!(template.snapshot_id, snapshot.id);
    assert_eq!(template.project_id, project_id);
    assert_eq!(template.sandbox_account_id, snapshot.sandbox_account_id);
    assert_eq!(template.provider, snapshot.provider);
    assert_eq!(template.cwd, project_cwd);

    let duplicate = client
        .post(format!("{base}/v1/control/sandbox-environment-templates"))
        .bearer_auth("test-control")
        .json(&serde_json::json!({
            "project_id": project_id,
            "name": "typescript PROJECT",
            "snapshot_id": snapshot.id,
            "cwd": workspace_root
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
        .get(format!(
            "{base}/v1/control/sandbox-environment-templates?project_id={project_id}"
        ))
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

pub fn provider_workspace_root(provider: SandboxProvider) -> &'static str {
    match provider {
        SandboxProvider::E2b => "/home/user",
        SandboxProvider::Daytona => "/home/daytona",
        SandboxProvider::Blaxel => "/blaxel",
        SandboxProvider::Tensorlake => "/home/tl-user",
    }
}
