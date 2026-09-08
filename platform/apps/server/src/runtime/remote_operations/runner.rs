use super::*;
use execution_api::{DesiredHostState, ExecutionHostState as HostState};
use execution_client::ExecutionClient;
use execution_core::{
    ExecutionError, ExecutionErrorCode as Code, ExecutionHandle, ExecutionHostId, ExecutionPath,
    ExecutionRuntime, ExecutionState, OperationContext, ProcessEventKind,
};
use std::time::Duration;
use tool_bash_minimal::{BashConfig, BashIds, BashInput, BashTool, PreparedBash, RunningBash};

impl RuntimeService {
    /// Start once per server. Four bounded lanes; aborting the returned task drops
    /// all in-flight calls. Prepared requests remain recoverable in PostgreSQL.
    pub fn spawn_remote_reconciler(&self) -> tokio::task::JoinHandle<()> {
        let service = self.clone();
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_millis(500));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                let results =
                    futures_util::future::join_all((0..4).map(|_| service.reconcile_remote_once()))
                        .await;
                if results.iter().any(Result::is_err) {
                    tracing::warn!("remote reconciliation failed; durable intent retained");
                }
            }
        })
    }
    /// One bounded action, exposed for embedding and deterministic recovery tests.
    pub async fn reconcile_remote_once(&self) -> Result<bool> {
        let Some(gateway) = &self.execution else {
            return Ok(false);
        };
        let processor = Uuid::now_v7();
        let mut tx = self.transaction().await?;
        let row:Option<Remote>=sqlx::query_as("with candidate as (select id from platform_remote_operations where finished_at is null and next_attempt_at<=clock_timestamp() and (processor_expires_at is null or processor_expires_at<=clock_timestamp()) order by next_attempt_at,id limit 1 for update skip locked) update platform_remote_operations r set processor_id=$1,processor_expires_at=clock_timestamp()+interval '30 seconds',process_epoch=process_epoch+1 from candidate c where r.id=c.id returning r.*")
            .bind(processor).fetch_optional(&mut *tx).await?;
        tx.commit().await?;
        let Some(mut r) = row else { return Ok(false) };
        let expired = r.expires_at <= Utc::now();
        r.cancel_requested |= expired;
        let outcome = if r.kind == "sandbox" {
            sandbox_step(gateway, &mut r, expired).await
        } else {
            execution_step(gateway, &mut r).await
        };
        let retry_delay = if let Err(error) = outcome {
            // Transport failures never imply successful termination or permission
            // to invent a new execution identity. Keep the prepared request.
            r.error = Some(
                json!({"code":format!("REMOTE_{:?}",error.code).to_uppercase(),"message":error.message}),
            );
            if r.kind == "execution"
                && matches!(error.code, Code::ExecutionLost | Code::OperationConflict)
            {
                r.status = "lost".into();
                r.finished_at = Some(Utc::now());
            } else if r.kind == "execution"
                && r.state.get("prepared").is_none()
                && matches!(
                    error.code,
                    Code::InvalidPath
                        | Code::InvalidRequest
                        | Code::PathOutsideRoot
                        | Code::RootNotFound
                        | Code::ReadOnlyRoot
                        | Code::PermissionDenied
                        | Code::Unsupported
                )
            {
                r.status = "failed".into();
                r.finished_at = Some(Utc::now());
            }
            5
        } else {
            1
        };
        sqlx::query("update platform_remote_operations set host_id=$4,workspace=$5,state=$6,status=$7,error=$8,output=$9,output_bytes=$10,truncated=$11,exit_code=$12,cancel_requested=cancel_requested or $13,cancellation_confirmed=$14,finished_at=$15,next_attempt_at=clock_timestamp()+make_interval(secs=>$16::double precision),processor_id=null,processor_expires_at=null where id=$1 and processor_id=$2 and process_epoch=$3 and processor_expires_at>clock_timestamp()")
            .bind(r.id).bind(processor).bind(r.process_epoch).bind(r.host_id).bind(r.workspace).bind(r.state).bind(r.status).bind(r.error).bind(r.output).bind(r.output_bytes).bind(r.truncated).bind(r.exit_code).bind(r.cancel_requested).bind(r.cancellation_confirmed).bind(r.finished_at).bind(f64::from(retry_delay)).execute(&self.pool).await?;
        Ok(true)
    }
}
fn invalid(message: &str) -> ExecutionError {
    ExecutionError::new(Code::InvalidRequest, message)
}
fn stored<T: DeserializeOwned>(v: Value) -> std::result::Result<T, ExecutionError> {
    serde_json::from_value(v).map_err(|_| invalid("Invalid saved remote operation state"))
}

async fn sandbox_step(
    gateway: &ExecutionClient,
    r: &mut Remote,
    expired: bool,
) -> std::result::Result<(), ExecutionError> {
    let ctx = OperationContext::with_timeout(Duration::from_secs(10));
    if r.host_id.is_none() && r.state["submitted"] != true {
        if r.cancel_requested {
            finish_sandbox(r, expired);
        } else {
            r.state["submitted"] = json!(true);
        }
        return Ok(());
    }
    let mut host = if let Some(id) = r.host_id {
        match gateway.get_host(&ctx, id).await {
            Ok(h) => h,
            // DELETE reads tombstoned hosts and returns their actual state. A
            // missing ordinary GET alone is never confirmation of deletion.
            Err(e) if e.code == Code::NotFound && r.cancel_requested => {
                gateway.delete_host(&ctx, id).await?
            }
            Err(e) => return Err(e),
        }
    } else {
        let request = stored(r.request.clone())?;
        gateway
            .create_host(&ctx, &format!("platform-sandbox:{}", r.id), &request)
            .await?
    };
    if let Some(id) = r.host_id
        && host.id != id
    {
        return Err(invalid("Gateway returned a different host"));
    }
    if host.kind != execution_api::ExecutionHostKind::E2b {
        return Err(invalid("Sandbox binding requires an E2B host"));
    }
    if r.request["adopted"] != true && host.metadata["platform_project_id"] != json!(r.project_id) {
        return Err(invalid("Sandbox belongs to a different project"));
    }
    r.host_id = Some(host.id);
    r.workspace["host_id"] = json!(host.id);
    r.workspace["environment_id"] = json!(r.environment_id);
    if r.cancel_requested {
        r.status = "terminating".into();
    }
    if r.cancel_requested
        && host.desired_state != DesiredHostState::Deleted
        && host.state != HostState::Deleted
    {
        host = gateway.delete_host(&ctx, host.id).await?;
    }
    if host.state == HostState::Deleted {
        finish_sandbox(r, expired);
    } else if r.cancel_requested {
        r.status = "terminating".into();
    } else if host.state == HostState::Ready {
        if !host
            .roots
            .iter()
            .any(|root| json!(root.native_path) == r.workspace["workspace_root"])
        {
            r.status = "unavailable".into();
            return Err(invalid(
                "Snapshot host does not expose the saved workspace root",
            ));
        }
        r.status = "ready".into();
    } else if matches!(host.state, HostState::Provisioning | HostState::Resuming) {
        r.status = "provisioning".into();
    } else {
        r.status = "unavailable".into();
    }
    r.error = None;
    Ok(())
}
fn finish_sandbox(r: &mut Remote, expired: bool) {
    r.status = if expired { "expired" } else { "terminated" }.into();
    r.cancellation_confirmed = true;
    r.finished_at = Some(Utc::now());
}

async fn execution_step(
    gateway: &ExecutionClient,
    r: &mut Remote,
) -> std::result::Result<(), ExecutionError> {
    if r.cancel_requested && r.state["start_submitted"] != true {
        r.status = "cancelled".into();
        r.cancellation_confirmed = true;
        r.finished_at = Some(Utc::now());
        return Ok(());
    }
    let ctx = OperationContext::with_timeout(Duration::from_secs(10));
    let host = gateway
        .connect_host(
            &ctx,
            ExecutionHostId::new(
                r.host_id
                    .ok_or_else(|| invalid("Missing host"))?
                    .to_string(),
            )?,
        )
        .await?;
    let w: ExecutionWorkspace = stored(r.workspace.clone())?;
    let root = host
        .descriptor()
        .roots
        .iter()
        .find(|root| root.native_path == w.workspace_root)
        .ok_or_else(|| invalid("Project workspace root is no longer registered"))?;
    let cwd = ExecutionPath::new(root.id.clone(), w.path)?;
    let tool = BashTool::new(&host, cwd, BashConfig::default())?;
    let Some(prepared) = r.state.get("prepared") else {
        let input: c::Bash = stored(r.request.clone())?;
        let prepared = tool.prepare(
            BashInput {
                command: input.command,
                timeout: input.timeout_ms,
                workdir: Some(input.workdir),
            },
            BashIds {
                operation_id: execution_core::OperationId::new(format!("platform-start:{}", r.id))?,
                execution_id: execution_core::ExecutionId::new(r.id.to_string())?,
                terminate_operation_id: execution_core::OperationId::new(format!(
                    "platform-cancel:{}",
                    r.id
                ))?,
            },
        )?;
        r.state["prepared"] = json!(prepared);
        r.error = None;
        return Ok(()); // Must durably commit preparation before the first start.
    };
    let prepared: PreparedBash = stored(prepared.clone())?;
    if r.state["start_submitted"] != true {
        r.state["start_submitted"] = json!(true);
        return Ok(());
    }
    let mut running: RunningBash = if let Some(value) = r.state.get("running") {
        stored(value.clone())?
    } else if r.cancel_requested {
        tool.recover(
            &prepared,
            ExecutionHandle {
                execution_id: prepared.request().execution_id.clone(),
                supervisor_generation_id: prepared.generation().clone(),
                state: ExecutionState::Starting,
                started_at: None,
            },
        )?
    } else {
        let running = tool.start(&ctx, &prepared).await?;
        r.state["running"] = json!(running);
        r.status = "running".into();
        r.error = None;
        return Ok(());
    };
    if r.cancel_requested {
        tool.terminate(&ctx, &running).await?;
    }
    let page = tool.poll(&ctx, &mut running).await?;
    for event in page.events {
        if let ProcessEventKind::Output { data, .. } = event.event {
            r.output_bytes = r.output_bytes.saturating_add(data.as_slice().len() as i64);
            append_utf8(r, data.as_slice(), false);
        }
    }
    if page.complete {
        append_utf8(r, &[], true);
        let out = running.output();
        r.exit_code = out.exit_code;
        r.status = match out.handle.state {
            ExecutionState::Exited => "completed",
            ExecutionState::Cancelled => "cancelled",
            ExecutionState::Lost => "lost",
            _ => "failed",
        }
        .into();
        // Only terminal observation confirms cancellation. A terminate ACK or
        // network timeout is insufficient; an ordinary exit may win the race.
        r.cancellation_confirmed = r.cancel_requested && out.handle.state != ExecutionState::Lost;
        r.finished_at = Some(Utc::now());
    } else {
        r.status = "running".into();
    }
    r.state["running"] = json!(running);
    r.error = None;
    Ok(())
}
fn append_utf8(r: &mut Remote, data: &[u8], final_page: bool) {
    let mut bytes: Vec<u8> =
        serde_json::from_value(r.state["utf8_pending"].clone()).unwrap_or_default();
    bytes.extend_from_slice(data);
    let mut text = String::new();
    let mut offset = 0;
    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(s) => {
                text.push_str(s);
                offset = bytes.len();
            }
            Err(e) => {
                let end = offset + e.valid_up_to();
                text.push_str(std::str::from_utf8(&bytes[offset..end]).unwrap());
                offset = end;
                match e.error_len() {
                    Some(n) => {
                        text.push('\u{fffd}');
                        offset += n;
                    }
                    None if final_page => {
                        text.push('\u{fffd}');
                        offset = bytes.len();
                    }
                    None => break,
                }
            }
        }
    }
    r.state["utf8_pending"] = json!(&bytes[offset..]);
    let mut keep = text.len().min(1048576 - r.output.len());
    while !text.is_char_boundary(keep) {
        keep -= 1;
    }
    r.truncated |= keep < text.len();
    r.output.push_str(&text[..keep]);
}
