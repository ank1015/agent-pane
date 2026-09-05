use super::{
    error::EnvironmentError,
    gateway::EnvironmentGateway,
    model::{CreateEnvironment, Environment, UpdateEnvironment},
};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone)]
pub struct EnvironmentService {
    pool: PgPool,
    gateway: EnvironmentGateway,
}

impl EnvironmentService {
    pub fn new(pool: PgPool, gateway: EnvironmentGateway) -> Self {
        Self { pool, gateway }
    }

    async fn require_project(&self, project_id: Uuid) -> Result<(), EnvironmentError> {
        let exists: bool =
            sqlx::query_scalar("select exists(select 1 from projects where project_id = $1)")
                .bind(project_id)
                .fetch_one(&self.pool)
                .await?;
        if !exists {
            return Err(EnvironmentError::NotFound);
        }
        Ok(())
    }

    pub async fn list(&self, project_id: Uuid) -> Result<Vec<Environment>, EnvironmentError> {
        self.require_project(project_id).await?;
        Ok(sqlx::query_as(
            "select * from project_environments where project_id = $1 order by lower(name), id",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn get(&self, project_id: Uuid, id: Uuid) -> Result<Environment, EnvironmentError> {
        sqlx::query_as("select * from project_environments where project_id = $1 and id = $2")
            .bind(project_id)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(EnvironmentError::NotFound)
    }

    pub async fn create(
        &self,
        project_id: Uuid,
        input: CreateEnvironment,
    ) -> Result<Environment, EnvironmentError> {
        input.validate()?;
        self.require_project(project_id).await?;
        self.gateway.validate(&input).await?;
        let mut tx = self.pool.begin().await?;
        let environment = Self::insert(&mut tx, project_id, input).await?;
        tx.commit().await?;
        Ok(environment)
    }

    pub(crate) async fn validate_reference(
        &self,
        input: &CreateEnvironment,
    ) -> Result<(), EnvironmentError> {
        input.validate()?;
        self.gateway.validate(input).await
    }

    pub(crate) async fn insert(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        project_id: Uuid,
        input: CreateEnvironment,
    ) -> Result<Environment, EnvironmentError> {
        Ok(sqlx::query_as("insert into project_environments (id, project_id, name, type, machine_id, snapshot_id, workspace_root, path) values ($1,$2,$3,$4,$5,$6,$7,$8) returning *")
            .bind(Uuid::now_v7()).bind(project_id).bind(input.name).bind(input.kind).bind(input.machine_id).bind(input.snapshot_id).bind(input.workspace_root).bind(input.path)
            .fetch_one(&mut **tx).await?)
    }

    pub async fn update(
        &self,
        project_id: Uuid,
        id: Uuid,
        patch: UpdateEnvironment,
    ) -> Result<Environment, EnvironmentError> {
        let old = self.get(project_id, id).await?;
        let validate_root = patch.workspace_root.is_some();
        let input = patch.merge(&old)?;
        if validate_root
            || input.machine_id != old.machine_id
            || input.snapshot_id != old.snapshot_id
            || input.workspace_root != old.workspace_root
            || input.path != old.path
        {
            self.gateway.validate(&input).await?;
        }
        // No transaction/row lock is held across a gateway request. Compare the
        // original timestamp so concurrent PATCH operations cannot silently lose edits.
        sqlx::query_as("update project_environments set name=$3, machine_id=$4, snapshot_id=$5, workspace_root=$6, path=$7 where project_id=$1 and id=$2 and updated_at=$8 returning *")
            .bind(project_id).bind(id).bind(input.name).bind(input.machine_id).bind(input.snapshot_id).bind(input.workspace_root).bind(input.path).bind(old.updated_at)
            .fetch_optional(&self.pool).await?.ok_or(EnvironmentError::Conflict)
    }

    pub async fn delete(&self, project_id: Uuid, id: Uuid) -> Result<(), EnvironmentError> {
        let result = sqlx::query("delete from project_environments where project_id=$1 and id=$2")
            .bind(project_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(EnvironmentError::NotFound);
        }
        Ok(())
    }
}
