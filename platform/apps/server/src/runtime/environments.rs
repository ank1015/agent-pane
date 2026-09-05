//! Project-scoped environment access for leased harnesses. Reference validation
//! performs network I/O outside transactions; ownership is rechecked at commit.
use super::{
    RuntimeService,
    error::{Result, RuntimeError},
    receipts::{self, Reply},
    worker_model::Owner,
    workers::{lock_runs, owned},
};
use crate::projects::environments::{CreateEnvironment, EnvironmentService};
use serde_json::{Value, json};
use uuid::Uuid;

impl RuntimeService {
    pub(super) async fn worker_environments(&self, run: Uuid, owner: Owner) -> Result<Value> {
        let mut tx = self.transaction().await?;
        lock_runs(&mut tx, &[run]).await?;
        owned(&mut tx, run, owner).await?;
        let items: Vec<Value> = sqlx::query_scalar("select to_jsonb(e) from project_environments e join runs r on r.project_id=e.project_id where r.id=$1 order by lower(e.name), e.id")
            .bind(run).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(json!({"items": items}))
    }

    pub(super) async fn worker_create_environment(
        &self,
        run: Uuid,
        owner: Owner,
        key: &str,
        request: CreateEnvironment,
    ) -> Result<Reply> {
        request.validate()?;
        let hash = receipts::hash(&request)?;
        let scope = "run.environment.create";
        // Check the receipt first: replay does not depend on a still-live gateway
        // reference, and survives a new worker claiming the same logical run.
        let mut tx = self.transaction().await?;
        if let Some(reply) = receipts::worker_replay(&mut tx, scope, run, key, &hash, owner).await?
        {
            return Ok(reply);
        }
        lock_runs(&mut tx, &[run]).await?;
        owned(&mut tx, run, owner).await?;
        tx.commit().await?;
        let service = self
            .environments
            .as_ref()
            .ok_or(RuntimeError::Configuration)?;
        let root_path = service.validate_reference(&request).await?;

        let mut tx = self.transaction().await?;
        // A concurrent request may have completed during reference validation.
        if let Some(reply) = receipts::worker_replay(&mut tx, scope, run, key, &hash, owner).await?
        {
            return Ok(reply);
        }
        lock_runs(&mut tx, &[run]).await?;
        owned(&mut tx, run, owner).await?;
        let project: Uuid = sqlx::query_scalar("select project_id from runs where id=$1")
            .bind(run)
            .fetch_one(&mut *tx)
            .await?;
        let environment = EnvironmentService::insert(&mut tx, project, request, root_path).await?;
        let reply = Reply::new(
            201,
            serde_json::to_value(environment).map_err(|_| RuntimeError::StoredData)?,
        )
        .owned_by(owner);
        receipts::save(&mut tx, project, scope, run, key, &hash, &reply).await?;
        owned(&mut tx, run, owner).await?;
        tx.commit().await?;
        Ok(reply)
    }
}
