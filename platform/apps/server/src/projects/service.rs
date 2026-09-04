use sqlx::PgPool;
use uuid::Uuid;

use super::{CreateProjectInput, Project, ProjectError};

#[derive(Clone)]
pub struct ProjectService {
    pool: PgPool,
}

impl ProjectService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn list(&self) -> Result<Vec<Project>, ProjectError> {
        Ok(sqlx::query_as::<_, Project>(
            "select project_id as id, name, avatar from projects order by lower(name), project_id",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn create(&self, input: CreateProjectInput) -> Result<Project, ProjectError> {
        input.validate()?;
        Ok(sqlx::query_as::<_, Project>(
            "insert into projects (project_id, name, avatar) values ($1, $2, $3) returning project_id as id, name, avatar",
        )
        .bind(Uuid::now_v7())
        .bind(input.name)
        .bind(input.avatar)
        .fetch_one(&self.pool)
        .await?)
    }
}
