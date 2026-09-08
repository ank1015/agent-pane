//! Harness-published outputs, independent of private state and provisioning.
use super::{
    RuntimeService,
    error::{Result, RuntimeError},
    mutations,
    receipts::{self, Reply},
    worker_model::Owner,
    workers::{lock_runs, owned},
};
use platform_runtime_contracts::{
    ConflictCode, HarnessContract, OutputValue, PublishRunOutput, RUN_OUTPUT_MAX_COUNT,
    RUN_OUTPUT_PAGE_BYTES, RunOutputsQuery,
};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Registration validates schema syntax; publication validates the declared value.
pub(super) fn validate_contract(contract: &HarnessContract) -> Result<()> {
    contract.validate().map_err(RuntimeError::Invalid)?;
    for declaration in contract.outputs.values() {
        if let Some(schema) = &declaration.value_schema {
            jsonschema::draft202012::new(&json!(schema)).map_err(|_| {
                RuntimeError::Invalid(
                    "Output value_schema must be a locally resolvable draft 2020-12 JSON Schema.",
                )
            })?;
        }
    }
    Ok(())
}

/// Validate declared environment references at new-session admission. Resolving
/// references does not allocate hosts or alter the harness's configuration.
pub(super) async fn validate_inputs(
    tx: &mut Transaction<'_, Postgres>,
    project: Uuid,
    harness: &str,
    config: &Value,
) -> Result<()> {
    let contract: Value =
        sqlx::query_scalar("select harness_contract from harnesses where id=$1 for share")
            .bind(harness)
            .fetch_one(&mut **tx)
            .await?;
    let contract: HarnessContract =
        serde_json::from_value(contract).map_err(|_| RuntimeError::StoredData)?;
    let ids = contract
        .environment_ids(config)
        .map_err(RuntimeError::Invalid)?;
    for id in ids {
        let found: Option<Uuid> = sqlx::query_scalar(
            "select id from project_environments where project_id=$1 and id=$2 for share",
        )
        .bind(project)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
        found.ok_or(RuntimeError::NotFound)?;
    }
    Ok(())
}

const OUTPUT_VIEW: &str = "to_jsonb(o) - 'saved_by_lease_epoch'";

impl RuntimeService {
    pub(super) async fn publish_run_output(
        &self,
        run: Uuid,
        owner: Owner,
        key: &str,
        request: PublishRunOutput,
    ) -> Result<Reply> {
        request.validate().map_err(RuntimeError::Invalid)?;
        let scope = "run.output.publish";
        let hash = receipts::hash(&request)?;
        let mut tx = self.transaction().await?;
        if let Some(reply) = receipts::worker_replay(&mut tx, scope, run, key, &hash, owner).await?
        {
            return Ok(reply);
        }
        lock_runs(&mut tx, &[run]).await?;
        owned(&mut tx, run, owner).await?;
        let r = mutations::run(&mut tx, run).await?;
        let existing: Option<Value> = sqlx::query_scalar(&format!(
            "select {OUTPUT_VIEW} from run_outputs o where run_id=$1 and name=$2"
        ))
        .bind(run)
        .bind(&request.name)
        .fetch_optional(&mut *tx)
        .await?;
        let output = json!(request.output);
        let record = if let Some(existing) = existing {
            if existing["output"] != output {
                return Err(RuntimeError::CodedConflict(
                    ConflictCode::RunOutputImmutable,
                    "This output name already has a different immutable value.",
                ));
            }
            existing
        } else {
            let contract: Value =
                sqlx::query_scalar("select harness_contract from sessions where id=$1")
                    .bind(r.session_id)
                    .fetch_one(&mut *tx)
                    .await?;
            let contract: HarnessContract =
                serde_json::from_value(contract).map_err(|_| RuntimeError::StoredData)?;
            let declaration = contract
                .outputs
                .get(&request.name)
                .ok_or(RuntimeError::Invalid(
                    "Output name is not declared by this session's harness contract.",
                ))?;
            if declaration.kind != request.output.kind() {
                return Err(RuntimeError::Invalid(
                    "Output kind does not match its declaration.",
                ));
            }
            if let Some(schema) = &declaration.value_schema {
                let validator = jsonschema::draft202012::new(&json!(schema))
                    .map_err(|_| RuntimeError::StoredData)?;
                if !validator.is_valid(&request.output.value()) {
                    return Err(RuntimeError::Invalid(
                        "Output value does not match its declared schema.",
                    ));
                }
            }
            if let OutputValue::ExecutionWorkspace(workspace) = &request.output
                && let Some(environment) = workspace.environment_id
            {
                let found: Option<Uuid> = sqlx::query_scalar(
                    "select id from project_environments where project_id=$1 and id=$2 for share",
                )
                .bind(r.project_id)
                .bind(environment)
                .fetch_optional(&mut *tx)
                .await?;
                found.ok_or(RuntimeError::NotFound)?;
            }
            if let OutputValue::ExecutionWorkspace(workspace) = &request.output
                && let Some(sandbox) = workspace.sandbox_id
            {
                let found: bool = sqlx::query_scalar("select exists(select 1 from platform_remote_operations where project_id=$1 and id=$2 and kind='sandbox' and host_id=$3)")
                    .bind(r.project_id).bind(sandbox).bind(workspace.host_id).fetch_one(&mut *tx).await?;
                if !found {
                    return Err(RuntimeError::NotFound);
                }
            }
            let sequence: i64 = sqlx::query_scalar(
                "select coalesce(max(sequence),0)+1 from run_outputs where run_id=$1",
            )
            .bind(run)
            .fetch_one(&mut *tx)
            .await?;
            if sequence > RUN_OUTPUT_MAX_COUNT as i64 {
                return Err(RuntimeError::Invalid(
                    "A run may publish at most 64 outputs.",
                ));
            }
            let record = sqlx::query_scalar(&format!("insert into run_outputs as o(project_id,session_id,run_id,sequence,name,output,saved_by_lease_epoch) values($1,$2,$3,$4,$5,$6,$7) returning {OUTPUT_VIEW}"))
                .bind(r.project_id).bind(r.session_id).bind(run).bind(sequence).bind(&request.name)
                .bind(output).bind(owner.lease_epoch).fetch_one(&mut *tx).await?;
            // Events carry only references; large values never enter model history implicitly.
            mutations::event(
                &mut tx,
                run,
                "run.output_published",
                json!({"name":request.name,"sequence":sequence}),
            )
            .await?;
            record
        };
        let reply = Reply::new(200, record).owned_by(owner);
        receipts::save(&mut tx, r.project_id, scope, run, key, &hash, &reply).await?;
        owned(&mut tx, run, owner).await?;
        tx.commit().await?;
        self.notify(run);
        Ok(reply)
    }

    /// A leased caller can read any run in its project, including terminal runs.
    pub(super) async fn worker_run_outputs(
        &self,
        source: Uuid,
        owner: Owner,
        target: Uuid,
        query: RunOutputsQuery,
    ) -> Result<Value> {
        let mut tx = self.transaction().await?;
        lock_runs(&mut tx, &[source]).await?;
        owned(&mut tx, source, owner).await?;
        let r = mutations::run(&mut tx, source).await?;
        let result = Self::read_run_outputs(&mut tx, r.project_id, target, query).await?;
        owned(&mut tx, source, owner).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Caller supplies an authenticated project scope, never a guest-selected one.
    pub(super) async fn read_run_outputs(
        tx: &mut Transaction<'_, Postgres>,
        project: Uuid,
        target: Uuid,
        query: RunOutputsQuery,
    ) -> Result<Value> {
        query.validate().map_err(RuntimeError::Invalid)?;
        let exists: bool =
            sqlx::query_scalar("select exists(select 1 from runs where project_id=$1 and id=$2)")
                .bind(project)
                .bind(target)
                .fetch_one(&mut **tx)
                .await?;
        if !exists {
            return Err(RuntimeError::NotFound);
        }
        let limit = i64::from(query.limit.unwrap_or(20));
        let rows: Vec<Value> = sqlx::query_scalar(&format!("select {OUTPUT_VIEW} from run_outputs o where project_id=$1 and run_id=$2 and sequence>$3 order by sequence limit $4"))
            .bind(project).bind(target).bind(query.after_sequence.unwrap_or(0)).bind(limit+1)
            .fetch_all(&mut **tx).await?;
        let mut items: Vec<Value> = Vec::new();
        let mut bytes = 0;
        let mut next = None;
        for row in rows {
            let size = serde_json::to_vec(&row)
                .map_err(|_| RuntimeError::StoredData)?
                .len();
            if items.len() == limit as usize || bytes + size > RUN_OUTPUT_PAGE_BYTES {
                next = items.last().map(|v| v["sequence"].clone());
                break;
            }
            bytes += size;
            items.push(row);
        }
        Ok(json!({"items":items,"next_after_sequence":next}))
    }
}
