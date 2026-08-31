mod http;
pub mod model;

use execution_protocol::ProjectEnvironment;
use sqlx::PgPool;
use uuid::Uuid;

use crate::upstream::execution_gateway::{ExecutionGatewayClient, ExecutionGatewayError};
use model::{CreateProjectRequest, Project, UpdateProjectRequest};

const MAX_NAME_LENGTH: usize = 128;
const MAX_AVATAR_LENGTH: usize = 800_000;

#[derive(Clone)]
pub struct ProjectService {
    pool: PgPool,
    gateway: ExecutionGatewayClient,
}

impl ProjectService {
    #[must_use]
    pub const fn new(pool: PgPool, gateway: ExecutionGatewayClient) -> Self {
        Self { pool, gateway }
    }

    async fn list(&self) -> Result<Vec<Project>, ProjectError> {
        Ok(sqlx::query_as::<_, Project>(
            "select project_id as id, name, avatar
             from projects
             order by lower(name), project_id",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    async fn get(&self, project_id: Uuid) -> Result<Project, ProjectError> {
        sqlx::query_as::<_, Project>(
            "select project_id as id, name, avatar
             from projects
             where project_id = $1",
        )
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ProjectError::NotFound)
    }

    async fn list_environments(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<ProjectEnvironment>, ExecutionGatewayError> {
        self.gateway.list_project_environments(project_id).await
    }

    async fn create(&self, request: &CreateProjectRequest) -> Result<Project, ProjectError> {
        validate_name(&request.name)?;
        validate_avatar(request.avatar.as_deref())?;

        let project_id = Uuid::now_v7();
        Ok(sqlx::query_as::<_, Project>(
            "insert into projects (project_id, name, avatar)
             values ($1, $2, $3)
             returning project_id as id, name, avatar",
        )
        .bind(project_id)
        .bind(&request.name)
        .bind(&request.avatar)
        .fetch_one(&self.pool)
        .await?)
    }

    async fn update(
        &self,
        project_id: Uuid,
        request: &UpdateProjectRequest,
    ) -> Result<Project, ProjectError> {
        if request.name.is_none() && request.avatar.is_none() {
            return Err(ProjectError::InvalidRequest(
                "at least one of name or avatar must be provided",
            ));
        }
        if let Some(name) = request.name.as_deref() {
            validate_name(name)?;
        }
        if let Some(avatar) = &request.avatar {
            validate_avatar(avatar.as_deref())?;
        }

        sqlx::query_as::<_, Project>(
            "update projects
             set name = coalesce($2, name),
                 avatar = case when $3 then $4 else avatar end
             where project_id = $1
             returning project_id as id, name, avatar",
        )
        .bind(project_id)
        .bind(&request.name)
        .bind(request.avatar.is_some())
        .bind(request.avatar.as_ref().and_then(|avatar| avatar.as_ref()))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ProjectError::NotFound)
    }

    async fn delete(&self, project_id: Uuid) -> Result<(), ProjectError> {
        let result = sqlx::query("delete from projects where project_id = $1")
            .bind(project_id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(ProjectError::NotFound);
        }
        Ok(())
    }
}

fn validate_name(name: &str) -> Result<(), ProjectError> {
    if name.trim() != name || name.is_empty() || name.chars().count() > MAX_NAME_LENGTH {
        return Err(ProjectError::InvalidRequest(
            "name must be between 1 and 128 characters and have no surrounding whitespace",
        ));
    }
    Ok(())
}

fn validate_avatar(avatar: Option<&str>) -> Result<(), ProjectError> {
    if let Some(avatar) = avatar {
        if avatar.trim() != avatar
            || avatar.is_empty()
            || avatar.chars().count() > MAX_AVATAR_LENGTH
        {
            return Err(ProjectError::InvalidRequest(
                "avatar must be between 1 and 800000 characters and have no surrounding whitespace",
            ));
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("{0}")]
    InvalidRequest(&'static str),
    #[error("project not found")]
    NotFound,
    #[error("project database operation failed")]
    Database(#[from] sqlx::Error),
}

pub(crate) fn router(service: ProjectService) -> axum::Router<crate::AppState> {
    http::router(service)
}

#[cfg(test)]
mod tests {
    use sqlx::postgres::PgPoolOptions;

    use super::{
        ProjectError, ProjectService,
        model::{CreateProjectRequest, UpdateProjectRequest},
        validate_avatar, validate_name,
    };

    #[test]
    fn project_fields_reject_empty_or_untrimmed_values() {
        assert!(validate_name("").is_err());
        assert!(validate_name(" project").is_err());
        assert!(validate_avatar(Some("")).is_err());
        assert!(validate_avatar(Some("avatar ")).is_err());
        assert!(validate_name("project").is_ok());
        assert!(validate_avatar(None).is_ok());
        assert!(validate_avatar(Some("https://example.com/avatar.png")).is_ok());
    }

    #[tokio::test]
    #[ignore = "requires PLATFORM_TEST_DATABASE_URL pointing to an empty PostgreSQL database"]
    async fn project_lifecycle_is_persisted() {
        let database_url = std::env::var("PLATFORM_TEST_DATABASE_URL")
            .expect("PLATFORM_TEST_DATABASE_URL must be set");
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .expect("test database connection");
        crate::db::migrate(&pool)
            .await
            .expect("platform migrations");
        let gateway = crate::upstream::execution_gateway::ExecutionGatewayClient::new(
            "http://127.0.0.1:1".parse().unwrap(),
            "test-control-token",
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        let service = ProjectService::new(pool, gateway);

        let created = service
            .create(&CreateProjectRequest {
                name: "Agent Pane".to_owned(),
                avatar: Some("https://example.com/avatar.png".to_owned()),
            })
            .await
            .expect("create project");
        assert_eq!(created.name, "Agent Pane");

        let updated = service
            .update(
                created.id,
                &UpdateProjectRequest {
                    name: Some("Agent Pane Cloud".to_owned()),
                    avatar: Some(None),
                },
            )
            .await
            .expect("update project");
        assert_eq!(updated.name, "Agent Pane Cloud");
        assert_eq!(updated.avatar, None);
        assert_eq!(service.get(created.id).await.unwrap().id, created.id);

        service.delete(created.id).await.expect("delete project");
        assert!(matches!(
            service.get(created.id).await,
            Err(ProjectError::NotFound)
        ));
    }
}
