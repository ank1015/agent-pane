use std::collections::HashMap;

use codex_code_mode_runtime::{CellId, CodeModeStateStore, StateStoreFuture};
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Durable `store`/`load` values and cell-ID allocation for one Agent session.
pub struct PostgresCodeModeStateStore {
    pool: PgPool,
    agent_session_id: Uuid,
}

impl PostgresCodeModeStateStore {
    #[must_use]
    pub const fn new(pool: PgPool, agent_session_id: Uuid) -> Self {
        Self {
            pool,
            agent_session_id,
        }
    }

    async fn ensure(&self) -> Result<(), String> {
        sqlx::query(
            "insert into codex_code_mode_state (agent_session_id)
             values ($1)
             on conflict (agent_session_id) do nothing",
        )
        .bind(self.agent_session_id)
        .execute(&self.pool)
        .await
        .map_err(|error| error.to_string())?;
        Ok(())
    }
}

impl CodeModeStateStore for PostgresCodeModeStateStore {
    fn allocate_cell_id<'a>(&'a self) -> StateStoreFuture<'a, CellId> {
        Box::pin(async move {
            self.ensure().await?;
            let row = sqlx::query(
                "update codex_code_mode_state
                 set next_cell_id = next_cell_id + 1, updated_at = now()
                 where agent_session_id = $1
                 returning next_cell_id - 1 as cell_id",
            )
            .bind(self.agent_session_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| error.to_string())?;
            let cell_id = row
                .try_get::<i64, _>("cell_id")
                .map_err(|error| error.to_string())?;
            let cell_id = u64::try_from(cell_id).map_err(|error| error.to_string())?;
            Ok(CellId::new(cell_id.to_string()))
        })
    }

    fn load_values<'a>(&'a self) -> StateStoreFuture<'a, HashMap<String, Value>> {
        Box::pin(async move {
            self.ensure().await?;
            let row = sqlx::query(
                "select stored_values from codex_code_mode_state where agent_session_id = $1",
            )
            .bind(self.agent_session_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| error.to_string())?;
            let values = row
                .try_get::<Value, _>("stored_values")
                .map_err(|error| error.to_string())?;
            serde_json::from_value(values).map_err(|error| error.to_string())
        })
    }

    fn commit_values<'a>(&'a self, writes: HashMap<String, Value>) -> StateStoreFuture<'a, ()> {
        Box::pin(async move {
            if writes.is_empty() {
                return Ok(());
            }
            self.ensure().await?;
            let writes = serde_json::to_value(writes).map_err(|error| error.to_string())?;
            sqlx::query(
                "update codex_code_mode_state
                 set stored_values = stored_values || $2::jsonb, updated_at = now()
                 where agent_session_id = $1",
            )
            .bind(self.agent_session_id)
            .bind(writes)
            .execute(&self.pool)
            .await
            .map_err(|error| error.to_string())?;
            Ok(())
        })
    }
}
