mod http;
pub mod model;

use std::collections::HashSet;

use agent_contracts::{NewRunMessage, Run, RunEventPage, RunStatus, SessionMessagePage};
use chrono::Utc;
use execution_protocol::ProjectEnvironment;
use llm_contracts::{
    ContentPart, Message, MessageId, TextContent, Timestamp, UserMessage, Validate as _,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::task::JoinError;
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::upstream::{
    agent::{
        AgentClient, AgentError, AgentHarnessSelection, AgentRunAbortRequest, AgentRunAbortResult,
        AgentRunEventListQuery, AgentRunEventStreamQuery, AgentRunLimits,
        AgentSessionMessageListQuery, AgentSessionRunListQuery, AgentSessionRunPage, AgentStartRun,
    },
    execution_gateway::{ExecutionGatewayClient, ExecutionGatewayError},
    llm_gateway::{LlmGatewayClient, LlmGatewayError},
};
use model::{
    CreateProjectHarnessSessionRequest, CreateProjectHarnessSessionResponse, CreateProjectRequest,
    Project, ProjectBootstrap, ProjectBootstrapHarness, ProjectBootstrapHarnessProvider,
    ProjectBootstrapProviderAccount, ProjectHarnessSessionResponse,
    StartProjectHarnessSessionRunRequest, StartProjectHarnessSessionRunResponse,
    UpdateProjectRequest,
};

const MAX_NAME_LENGTH: usize = 128;
const MAX_AVATAR_LENGTH: usize = 800_000;

#[derive(Clone)]
pub struct ProjectService {
    pool: PgPool,
    gateway: ExecutionGatewayClient,
    agent: AgentClient,
    llm_gateway: LlmGatewayClient,
}

impl ProjectService {
    #[must_use]
    pub const fn new(
        pool: PgPool,
        gateway: ExecutionGatewayClient,
        agent: AgentClient,
        llm_gateway: LlmGatewayClient,
    ) -> Self {
        Self {
            pool,
            gateway,
            agent,
            llm_gateway,
        }
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

    async fn bootstrap(&self, project_id: Uuid) -> Result<ProjectBootstrap, ProjectError> {
        self.get(project_id).await?;

        let (harnesses, accounts, project_environments) = tokio::join!(
            self.list_bootstrap_harnesses(),
            self.llm_gateway.list_providers(None),
            self.gateway.list_project_environments(project_id),
        );
        let mut provider_accounts = accounts?
            .accounts
            .into_iter()
            .filter(|account| account.enabled)
            .map(ProjectBootstrapProviderAccount::from)
            .collect::<Vec<_>>();
        provider_accounts.sort_by(|left, right| {
            left.provider
                .as_str()
                .cmp(right.provider.as_str())
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.account_id.cmp(&right.account_id))
        });

        Ok(ProjectBootstrap {
            harnesses: harnesses?,
            provider_accounts,
            project_environments: project_environments?,
        })
    }

    async fn list_bootstrap_harnesses(&self) -> Result<Vec<ProjectBootstrapHarness>, ProjectError> {
        let mut harnesses = Vec::new();
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();

        loop {
            let page = self
                .agent
                .list_harnesses(cursor.as_deref(), Some(true))
                .await?;
            harnesses.extend(page.items);
            match page.next_cursor {
                Some(next_cursor) if seen_cursors.insert(next_cursor.clone()) => {
                    cursor = Some(next_cursor);
                }
                Some(_) => return Err(ProjectError::Agent(AgentError::InvalidPagination)),
                None => break,
            }
        }

        let mut tasks = JoinSet::new();
        for harness in harnesses {
            let revision_id = harness.active_revision_id.clone().ok_or_else(|| {
                ProjectError::InvalidHarnessMetadata(format!(
                    "enabled harness {} has no active revision",
                    harness.harness_id
                ))
            })?;
            let agent = self.agent.clone();
            tasks.spawn(async move {
                let revision = agent
                    .get_harness_revision(&harness.harness_id, &revision_id)
                    .await?;
                Ok::<_, AgentError>((harness, revision))
            });
        }

        let mut resolved = Vec::new();
        while let Some(result) = tasks.join_next().await {
            let (harness, revision) = result??;
            resolved.push(ProjectBootstrapHarness {
                harness_id: harness.harness_id,
                active_revision_id: revision.harness_revision_id,
                config_schema: revision.config_schema,
                supported_providers: harness
                    .supported_providers
                    .into_iter()
                    .map(|provider| ProjectBootstrapHarnessProvider {
                        provider_id: provider.provider_id,
                        model_ids: provider.model_ids,
                    })
                    .collect(),
                supported_reasoning_levels: harness.supported_reasoning_levels,
            });
        }
        resolved.sort_by(|left, right| left.harness_id.cmp(&right.harness_id));
        Ok(resolved)
    }

    async fn create_harness_session(
        &self,
        project_id: Uuid,
        idempotency_key: &str,
        request: &CreateProjectHarnessSessionRequest,
    ) -> Result<CreateProjectHarnessSessionResponse, ProjectError> {
        validate_harness_session_request(idempotency_key, request)?;
        let request_hash = request_hash(request)?;
        let reserved = self
            .reserve_initial_run(
                project_id,
                idempotency_key,
                &request.harness_id,
                &request_hash,
            )
            .await?;
        let message = user_message(
            reserved.trigger_message_id,
            &request.prompt,
            &request.attachments,
        )?;
        let mut session = self.agent.create_session(reserved.session_id).await?;
        let accepted = self
            .agent
            .start_run(
                reserved.session_id,
                &AgentStartRun {
                    run_id: reserved.run_id,
                    input: NewRunMessage {
                        session_message_id: reserved.trigger_message_id,
                        message,
                    },
                    harness: AgentHarnessSelection::ActiveRevision {
                        harness_id: request.harness_id.clone(),
                    },
                    config_override: request.config_override.clone(),
                    limits: AgentRunLimits {
                        max_turns: request.limits.max_turns,
                    },
                    expected_session_revision: Some(0),
                },
            )
            .await?;
        session.current_revision = accepted.trigger_message.revision;
        self.mark_session_and_run_accepted(reserved.session_id, reserved.run_id)
            .await?;

        Ok(CreateProjectHarnessSessionResponse {
            session,
            trigger_message: accepted.trigger_message,
            run: accepted.run,
        })
    }

    async fn start_harness_session_run(
        &self,
        project_id: Uuid,
        session_id: Uuid,
        idempotency_key: &str,
        request: &StartProjectHarnessSessionRunRequest,
    ) -> Result<StartProjectHarnessSessionRunResponse, ProjectError> {
        validate_start_run_request(idempotency_key, request)?;
        let request_hash = request_hash(request)?;
        let reserved = self
            .reserve_follow_up_run(project_id, session_id, idempotency_key, &request_hash)
            .await?;
        let message = user_message(
            reserved.trigger_message_id,
            &request.prompt,
            &request.attachments,
        )?;
        let accepted = self
            .agent
            .start_run(
                session_id,
                &AgentStartRun {
                    run_id: reserved.run_id,
                    input: NewRunMessage {
                        session_message_id: reserved.trigger_message_id,
                        message,
                    },
                    harness: AgentHarnessSelection::ActiveRevision {
                        harness_id: reserved.harness_id,
                    },
                    config_override: request.config_override.clone(),
                    limits: AgentRunLimits {
                        max_turns: request.limits.max_turns,
                    },
                    expected_session_revision: Some(request.expected_session_revision),
                },
            )
            .await?;
        self.mark_run_accepted(reserved.run_id).await?;

        Ok(StartProjectHarnessSessionRunResponse {
            trigger_message: accepted.trigger_message,
            run: accepted.run,
        })
    }

    async fn get_harness_session(
        &self,
        project_id: Uuid,
        session_id: Uuid,
    ) -> Result<ProjectHarnessSessionResponse, ProjectError> {
        let mapped = self.mapped_harness_session(project_id, session_id).await?;
        let (session, latest_run) = match mapped.latest_run_id {
            Some(run_id) => {
                let (session, run) = tokio::try_join!(
                    self.agent.get_session(session_id),
                    self.agent.get_run(run_id)
                )?;
                (session, Some(run))
            }
            None => (self.agent.get_session(session_id).await?, None),
        };
        let active_run = latest_run
            .as_ref()
            .filter(|run| matches!(run.status, RunStatus::Active | RunStatus::Waiting))
            .cloned();
        Ok(ProjectHarnessSessionResponse {
            session,
            harness_id: mapped.harness_id,
            latest_run,
            active_run,
        })
    }

    async fn list_harness_session_messages(
        &self,
        project_id: Uuid,
        session_id: Uuid,
        query: &AgentSessionMessageListQuery,
    ) -> Result<SessionMessagePage, ProjectError> {
        self.mapped_harness_session(project_id, session_id).await?;
        Ok(self.agent.list_session_messages(session_id, query).await?)
    }

    async fn list_harness_session_runs(
        &self,
        project_id: Uuid,
        session_id: Uuid,
        query: &AgentSessionRunListQuery,
    ) -> Result<AgentSessionRunPage, ProjectError> {
        self.mapped_harness_session(project_id, session_id).await?;
        Ok(self.agent.list_session_runs(session_id, query).await?)
    }

    async fn get_harness_run(
        &self,
        project_id: Uuid,
        session_id: Uuid,
        run_id: Uuid,
    ) -> Result<Run, ProjectError> {
        self.mapped_harness_run(project_id, session_id, run_id)
            .await?;
        Ok(self.agent.get_run(run_id).await?)
    }

    async fn list_harness_run_events(
        &self,
        project_id: Uuid,
        session_id: Uuid,
        run_id: Uuid,
        query: &AgentRunEventListQuery,
    ) -> Result<RunEventPage, ProjectError> {
        self.mapped_harness_run(project_id, session_id, run_id)
            .await?;
        Ok(self.agent.list_run_events(run_id, query).await?)
    }

    async fn stream_harness_run_events(
        &self,
        project_id: Uuid,
        session_id: Uuid,
        run_id: Uuid,
        query: &AgentRunEventStreamQuery,
        last_event_id: Option<&str>,
    ) -> Result<reqwest::Response, ProjectError> {
        self.mapped_harness_run(project_id, session_id, run_id)
            .await?;
        Ok(self
            .agent
            .stream_run_events(run_id, query, last_event_id)
            .await?)
    }

    async fn abort_harness_run(
        &self,
        project_id: Uuid,
        session_id: Uuid,
        run_id: Uuid,
        request: &AgentRunAbortRequest,
    ) -> Result<AgentRunAbortResult, ProjectError> {
        self.mapped_harness_run(project_id, session_id, run_id)
            .await?;
        Ok(self.agent.abort_run(run_id, request).await?)
    }

    async fn mapped_harness_session(
        &self,
        project_id: Uuid,
        session_id: Uuid,
    ) -> Result<MappedHarnessSessionRow, ProjectError> {
        sqlx::query_as::<_, MappedHarnessSessionRow>(
            "select session.harness_id, latest_run.run_id as latest_run_id \
             from project_harness_sessions session \
             left join lateral ( \
                 select run.run_id \
                 from project_harness_runs run \
                 where run.session_id = session.session_id \
                   and run.creation_state = 'accepted' \
                 order by run.run_sequence desc \
                 limit 1 \
             ) latest_run on true \
             where session.project_id = $1 and session.session_id = $2",
        )
        .bind(project_id)
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ProjectError::HarnessSessionNotFound)
    }

    async fn mapped_harness_run(
        &self,
        project_id: Uuid,
        session_id: Uuid,
        run_id: Uuid,
    ) -> Result<(), ProjectError> {
        let exists = sqlx::query_scalar::<_, bool>(
            "select exists( \
                 select 1 \
                 from project_harness_runs run \
                 join project_harness_sessions session \
                   on session.session_id = run.session_id \
                 where session.project_id = $1 \
                   and session.session_id = $2 \
                   and run.run_id = $3 \
                   and session.creation_state = 'accepted' \
                   and run.creation_state = 'accepted' \
             )",
        )
        .bind(project_id)
        .bind(session_id)
        .bind(run_id)
        .fetch_one(&self.pool)
        .await?;
        if !exists {
            return Err(ProjectError::HarnessRunNotFound);
        }
        Ok(())
    }

    async fn reserve_initial_run(
        &self,
        project_id: Uuid,
        idempotency_key: &str,
        harness_id: &str,
        request_hash: &[u8; 32],
    ) -> Result<ReservedInitialRun, ProjectError> {
        let mut transaction = self.pool.begin().await?;
        let project_exists = sqlx::query_scalar::<_, bool>(
            "select exists(select 1 from projects where project_id = $1)",
        )
        .bind(project_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !project_exists {
            return Err(ProjectError::NotFound);
        }

        let candidate_session_id = Uuid::now_v7();
        sqlx::query(
            "insert into project_harness_sessions \
             (session_id, project_id, harness_id, idempotency_key, request_hash) \
             values ($1, $2, $3, $4, $5) \
             on conflict (project_id, idempotency_key) do nothing",
        )
        .bind(candidate_session_id)
        .bind(project_id)
        .bind(harness_id)
        .bind(idempotency_key)
        .bind(request_hash.as_slice())
        .execute(&mut *transaction)
        .await?;

        let session = sqlx::query_as::<_, ReservedSessionRow>(
            "select session_id, request_hash \
             from project_harness_sessions \
             where project_id = $1 and idempotency_key = $2",
        )
        .bind(project_id)
        .bind(idempotency_key)
        .fetch_one(&mut *transaction)
        .await?;
        if session.request_hash.as_slice() != request_hash {
            return Err(ProjectError::IdempotencyConflict);
        }

        let candidate_run_id = Uuid::now_v7();
        let candidate_message_id = Uuid::now_v7();
        sqlx::query(
            "insert into project_harness_runs \
             (run_id, session_id, trigger_message_id, run_sequence, idempotency_key, request_hash) \
             values ($1, $2, $3, 1, $4, $5) \
             on conflict (session_id, run_sequence) do nothing",
        )
        .bind(candidate_run_id)
        .bind(session.session_id)
        .bind(candidate_message_id)
        .bind(idempotency_key)
        .bind(request_hash.as_slice())
        .execute(&mut *transaction)
        .await?;
        let run = sqlx::query_as::<_, ReservedRunRow>(
            "select run_id, trigger_message_id \
             from project_harness_runs \
             where session_id = $1 and run_sequence = 1",
        )
        .bind(session.session_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(ReservedInitialRun {
            session_id: session.session_id,
            run_id: run.run_id,
            trigger_message_id: run.trigger_message_id,
        })
    }

    async fn reserve_follow_up_run(
        &self,
        project_id: Uuid,
        session_id: Uuid,
        idempotency_key: &str,
        request_hash: &[u8; 32],
    ) -> Result<ReservedFollowUpRun, ProjectError> {
        let mut transaction = self.pool.begin().await?;
        let harness_id = sqlx::query_scalar::<_, String>(
            "select harness_id \
             from project_harness_sessions \
             where project_id = $1 and session_id = $2 and creation_state = 'accepted' \
             for update",
        )
        .bind(project_id)
        .bind(session_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(ProjectError::HarnessSessionNotFound)?;

        if let Some(existing) = sqlx::query_as::<_, ReservedIdempotentRunRow>(
            "select run_id, trigger_message_id, request_hash \
             from project_harness_runs \
             where session_id = $1 and idempotency_key = $2",
        )
        .bind(session_id)
        .bind(idempotency_key)
        .fetch_optional(&mut *transaction)
        .await?
        {
            if existing.request_hash.as_slice() != request_hash {
                return Err(ProjectError::IdempotencyConflict);
            }
            transaction.commit().await?;
            return Ok(ReservedFollowUpRun {
                harness_id,
                run_id: existing.run_id,
                trigger_message_id: existing.trigger_message_id,
            });
        }

        let run_sequence = sqlx::query_scalar::<_, i32>(
            "select coalesce(max(run_sequence), 0) + 1 \
             from project_harness_runs \
             where session_id = $1",
        )
        .bind(session_id)
        .fetch_one(&mut *transaction)
        .await?;
        let run_id = Uuid::now_v7();
        let trigger_message_id = Uuid::now_v7();
        sqlx::query(
            "insert into project_harness_runs \
             (run_id, session_id, trigger_message_id, run_sequence, idempotency_key, request_hash) \
             values ($1, $2, $3, $4, $5, $6)",
        )
        .bind(run_id)
        .bind(session_id)
        .bind(trigger_message_id)
        .bind(run_sequence)
        .bind(idempotency_key)
        .bind(request_hash.as_slice())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(ReservedFollowUpRun {
            harness_id,
            run_id,
            trigger_message_id,
        })
    }

    async fn mark_session_and_run_accepted(
        &self,
        session_id: Uuid,
        run_id: Uuid,
    ) -> Result<(), ProjectError> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "update project_harness_sessions \
             set creation_state = 'accepted', accepted_at = coalesce(accepted_at, now()) \
             where session_id = $1",
        )
        .bind(session_id)
        .execute(&mut *transaction)
        .await?;
        mark_run_accepted_in(&mut transaction, run_id).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn mark_run_accepted(&self, run_id: Uuid) -> Result<(), ProjectError> {
        let mut transaction = self.pool.begin().await?;
        mark_run_accepted_in(&mut transaction, run_id).await?;
        transaction.commit().await?;
        Ok(())
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

#[derive(sqlx::FromRow)]
struct ReservedSessionRow {
    session_id: Uuid,
    request_hash: Vec<u8>,
}

#[derive(sqlx::FromRow)]
struct MappedHarnessSessionRow {
    harness_id: String,
    latest_run_id: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct ReservedRunRow {
    run_id: Uuid,
    trigger_message_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct ReservedIdempotentRunRow {
    run_id: Uuid,
    trigger_message_id: Uuid,
    request_hash: Vec<u8>,
}

struct ReservedInitialRun {
    session_id: Uuid,
    run_id: Uuid,
    trigger_message_id: Uuid,
}

struct ReservedFollowUpRun {
    harness_id: String,
    run_id: Uuid,
    trigger_message_id: Uuid,
}

fn validate_harness_session_request(
    idempotency_key: &str,
    request: &CreateProjectHarnessSessionRequest,
) -> Result<(), ProjectError> {
    validate_idempotency_key(idempotency_key)?;
    if request.harness_id.is_empty()
        || request.harness_id != request.harness_id.trim()
        || request.harness_id.chars().count() > 255
    {
        return Err(ProjectError::InvalidRequest(
            "harness_id must be between 1 and 255 characters and have no surrounding whitespace",
        ));
    }
    validate_run_input(&request.prompt, &request.attachments, request.limits)
}

fn validate_start_run_request(
    idempotency_key: &str,
    request: &StartProjectHarnessSessionRunRequest,
) -> Result<(), ProjectError> {
    validate_idempotency_key(idempotency_key)?;
    validate_run_input(&request.prompt, &request.attachments, request.limits)
}

fn validate_idempotency_key(idempotency_key: &str) -> Result<(), ProjectError> {
    if idempotency_key.is_empty()
        || idempotency_key != idempotency_key.trim()
        || idempotency_key.chars().count() > 255
    {
        return Err(ProjectError::InvalidRequest(
            "Idempotency-Key must be between 1 and 255 characters and have no surrounding whitespace",
        ));
    }
    Ok(())
}

fn validate_run_input(
    prompt: &str,
    attachments: &[llm_contracts::ImageContent],
    limits: model::ProjectHarnessRunLimits,
) -> Result<(), ProjectError> {
    if prompt.trim().is_empty() && attachments.is_empty() {
        return Err(ProjectError::InvalidRequest(
            "at least one of prompt or attachments must be provided",
        ));
    }
    if limits.max_turns == Some(0) {
        return Err(ProjectError::InvalidRequest(
            "limits.max_turns must be greater than zero",
        ));
    }
    Ok(())
}

fn request_hash<T: Serialize>(request: &T) -> Result<[u8; 32], ProjectError> {
    let bytes = serde_json::to_vec(request).map_err(ProjectError::RequestSerialization)?;
    Ok(Sha256::digest(bytes).into())
}

async fn mark_run_accepted_in(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: Uuid,
) -> Result<(), ProjectError> {
    sqlx::query(
        "update project_harness_runs \
         set creation_state = 'accepted', accepted_at = coalesce(accepted_at, now()) \
         where run_id = $1",
    )
    .bind(run_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn user_message(
    session_message_id: Uuid,
    prompt: &str,
    attachments: &[llm_contracts::ImageContent],
) -> Result<Message, ProjectError> {
    let mut content =
        Vec::with_capacity(usize::from(!prompt.trim().is_empty()) + attachments.len());
    if !prompt.trim().is_empty() {
        content.push(ContentPart::Text(TextContent {
            content: prompt.to_owned(),
            metadata: None,
        }));
    }
    content.extend(attachments.iter().cloned().map(ContentPart::Image));
    let timestamp = u64::try_from(Utc::now().timestamp_millis())
        .map_err(|_| ProjectError::InvalidSystemClock)?;
    let message = Message::User(UserMessage {
        id: MessageId::new(session_message_id.to_string())
            .map_err(|error| ProjectError::InvalidMessage(error.to_string()))?,
        timestamp: Timestamp(timestamp),
        content,
    });
    message
        .validate()
        .map_err(|error| ProjectError::InvalidMessage(error.to_string()))?;
    Ok(message)
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
    #[error("project harness session not found")]
    HarnessSessionNotFound,
    #[error("project harness run not found")]
    HarnessRunNotFound,
    #[error("the Idempotency-Key is already associated with a different request")]
    IdempotencyConflict,
    #[error("invalid harness session message: {0}")]
    InvalidMessage(String),
    #[error("invalid harness metadata: {0}")]
    InvalidHarnessMetadata(String),
    #[error("the system clock is before the Unix epoch")]
    InvalidSystemClock,
    #[error("could not serialize the harness session request")]
    RequestSerialization(#[source] serde_json::Error),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error(transparent)]
    ExecutionGateway(#[from] ExecutionGatewayError),
    #[error(transparent)]
    LlmGateway(#[from] LlmGatewayError),
    #[error("harness metadata task failed")]
    HarnessMetadataTask(#[from] JoinError),
    #[error("project database operation failed")]
    Database(#[from] sqlx::Error),
}

pub(crate) fn router(service: ProjectService) -> axum::Router<crate::AppState> {
    http::router(service)
}

#[cfg(test)]
mod tests {
    use llm_contracts::{ContentPart, ImageSource, Message};
    use serde_json::json;
    use sqlx::postgres::PgPoolOptions;
    use uuid::Uuid;

    use super::{
        ProjectError, ProjectService,
        model::{
            CreateProjectHarnessSessionRequest, CreateProjectRequest,
            StartProjectHarnessSessionRunRequest, UpdateProjectRequest,
        },
        request_hash, user_message, validate_avatar, validate_harness_session_request,
        validate_name, validate_start_run_request,
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

    #[test]
    fn harness_session_message_places_prompt_before_image_attachments() {
        let request: CreateProjectHarnessSessionRequest = serde_json::from_value(json!({
            "harness_id": "environment",
            "prompt": "Build from this reference",
            "attachments": [
                {
                    "source": {
                        "type": "url",
                        "url": "https://example.com/reference.png"
                    },
                    "detail": "high"
                },
                {
                    "source": {
                        "type": "base64",
                        "data": "aW1hZ2U=",
                        "mime_type": "image/png"
                    }
                }
            ]
        }))
        .unwrap();

        let Message::User(message) =
            user_message(Uuid::now_v7(), &request.prompt, &request.attachments).unwrap()
        else {
            panic!("expected a user message");
        };
        assert_eq!(message.content.len(), 3);
        assert!(matches!(
            &message.content[0],
            ContentPart::Text(text) if text.content == "Build from this reference"
        ));
        assert!(matches!(
            &message.content[1],
            ContentPart::Image(image)
                if matches!(&image.source, ImageSource::Url(source) if source.url == "https://example.com/reference.png")
        ));
        assert!(matches!(
            &message.content[2],
            ContentPart::Image(image)
                if matches!(&image.source, ImageSource::Base64(source) if source.mime_type == "image/png")
        ));
    }

    #[test]
    fn harness_session_accepts_an_attachment_without_a_prompt() {
        let request: CreateProjectHarnessSessionRequest = serde_json::from_value(json!({
            "harness_id": "environment",
            "prompt": "  ",
            "attachements": [{
                "source": {
                    "type": "url",
                    "url": "https://example.com/reference.png"
                }
            }]
        }))
        .unwrap();

        validate_harness_session_request("request-1", &request).unwrap();
        let Message::User(message) =
            user_message(Uuid::now_v7(), &request.prompt, &request.attachments).unwrap()
        else {
            panic!("expected a user message");
        };
        assert_eq!(message.content.len(), 1);
        assert!(matches!(message.content[0], ContentPart::Image(_)));
    }

    #[test]
    fn harness_session_rejects_empty_input_and_invalid_limits() {
        let mut request: CreateProjectHarnessSessionRequest = serde_json::from_value(json!({
            "harness_id": "environment",
            "prompt": "",
            "limits": {"max_turns": 0}
        }))
        .unwrap();

        assert!(matches!(
            validate_harness_session_request("request-1", &request),
            Err(ProjectError::InvalidRequest(_))
        ));
        request.prompt = "Build it".to_owned();
        assert!(matches!(
            validate_harness_session_request("request-1", &request),
            Err(ProjectError::InvalidRequest(_))
        ));
    }

    #[test]
    fn follow_up_run_requires_input_and_preserves_the_expected_revision() {
        let request: StartProjectHarnessSessionRunRequest = serde_json::from_value(json!({
            "prompt": "Continue building the environment",
            "config_override": {"model_id": "gpt-5.6-sol"},
            "limits": {"max_turns": 25},
            "expected_session_revision": 7
        }))
        .unwrap();

        validate_start_run_request("run-request-2", &request).unwrap();
        assert_eq!(request.expected_session_revision, 7);

        let empty: StartProjectHarnessSessionRunRequest = serde_json::from_value(json!({
            "prompt": "  ",
            "expected_session_revision": 7
        }))
        .unwrap();
        assert!(matches!(
            validate_start_run_request("run-request-3", &empty),
            Err(ProjectError::InvalidRequest(_))
        ));
    }

    #[test]
    fn follow_up_run_idempotency_hash_includes_the_expected_revision() {
        let first: StartProjectHarnessSessionRunRequest = serde_json::from_value(json!({
            "prompt": "Continue",
            "expected_session_revision": 3
        }))
        .unwrap();
        let second: StartProjectHarnessSessionRunRequest = serde_json::from_value(json!({
            "prompt": "Continue",
            "expected_session_revision": 4
        }))
        .unwrap();

        assert_ne!(
            request_hash(&first).unwrap(),
            request_hash(&second).unwrap()
        );
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
        let agent = crate::upstream::agent::AgentClient::new(
            "http://127.0.0.1:1".parse().unwrap(),
            "test-control-token",
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        let llm_gateway = crate::upstream::llm_gateway::LlmGatewayClient::new(
            "http://127.0.0.1:1".parse().unwrap(),
            "test-admin-token",
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        let service = ProjectService::new(pool, gateway, agent, llm_gateway);

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
