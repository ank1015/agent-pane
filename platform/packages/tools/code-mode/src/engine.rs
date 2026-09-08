use crate::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::watch,
};

pub struct Engine {
    executable: PathBuf,
    pub limits: Limits,
    pub extensions: Vec<Extension>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Inspect {
    pub cell: Cell,
    pub calls: Page,
}
impl Engine {
    /// Seal an unfinished cell belonging to a replaced owner, without needing its
    /// old source/configuration or launching JavaScript. Same-owner active cells
    /// must be cancelled through their running execution's cancellation channel.
    pub async fn recover_abandoned(journal: &dyn Journal, id: Uuid) -> Result<Option<Cell>> {
        let key = cell_key(id);
        let Some(entry) = journal.get(&key).await? else {
            return Ok(None);
        };
        let mut cell: Cell = serde_json::from_value(entry.value).map_err(|_| Error::Storage)?;
        if cell.status == CellStatus::Running {
            if cell.owner == journal.owner() {
                return Err(Error::Invalid(
                    "Cell still belongs to this owner; cancel its execution first",
                ));
            }
            cell.status = CellStatus::Interrupted;
            cell.error = Some(ToolError::uncertain(
                "OWNER_REPLACED",
                "Cell interrupted by owner replacement. Inspect prepared calls; do not replay source.",
            ));
            journal.put(&key, entry.version, json!(cell)).await?;
        }
        Ok(Some(cell))
    }
    pub fn new(executable: impl AsRef<Path>) -> Self {
        Self {
            executable: executable.as_ref().to_owned(),
            limits: Limits::default(),
            extensions: vec![],
        }
    }
    pub async fn inspect(
        journal: &dyn Journal,
        id: Uuid,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Option<Inspect>> {
        if !(1..=50).contains(&limit) {
            return Err(Error::Invalid("Trace page limit must be 1–50"));
        }
        let Some(entry) = journal.get(&cell_key(id)).await? else {
            return Ok(None);
        };
        let cell: Cell = serde_json::from_value(entry.value).map_err(|_| Error::Storage)?;
        let prefix = call_prefix(id);
        if after.is_some_and(|s| !s.starts_with(&prefix)) {
            return Err(Error::Invalid("Trace cursor belongs to another cell"));
        }
        Ok(Some(Inspect {
            cell,
            calls: journal.list(&prefix, after, limit).await?,
        }))
    }
    /// Repeated cell IDs return saved state. A new owner's unfinished cell is
    /// sealed as interrupted; source is never replayed, even if no call is visible.
    pub async fn execute(
        &self,
        input: Input,
        registry: &Registry,
        journal: &dyn Journal,
        dispatcher: &dyn Dispatcher,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Cell> {
        self.limits.validate()?;
        if input.id.is_nil() || input.source.len() > self.limits.source_bytes {
            return Err(Error::Invalid("Invalid cell ID or excessive source"));
        }
        let mut names = std::collections::HashSet::new();
        for e in &self.extensions {
            if e.name.is_empty()
                || e.name.len() > 128
                || !names.insert(&e.name)
                || e.factory.len() > 256 * 1024
            {
                return Err(Error::Invalid("Invalid SDK extension"));
            }
        }
        let definitions: Vec<Tool> = registry.tools().cloned().collect();
        let guest = guest::Input {
            source: input.source.clone(),
            tools: definitions.clone(),
            extensions: self.extensions.clone(),
            timeout_ms: self.limits.timeout_ms,
        };
        let wire = serde_json::to_vec(&guest).map_err(|_| Error::Storage)?;
        if wire.len() > 2 * 1024 * 1024 {
            return Err(Error::Invalid("Guest bootstrap exceeds 2 MiB"));
        }
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&json!([&guest, &self.limits])).map_err(|_| Error::Storage)?
            )
        );
        let key = cell_key(input.id);
        if let Some(entry) = journal.get(&key).await? {
            let mut cell: Cell = serde_json::from_value(entry.value).map_err(|_| Error::Storage)?;
            if cell.fingerprint != fingerprint {
                return Err(Error::Conflict);
            }
            if cell.status == CellStatus::Running && cell.owner != journal.owner() {
                cell.status = CellStatus::Interrupted;
                cell.error = Some(ToolError::uncertain(
                    "OWNER_REPLACED",
                    "Cell interrupted by owner replacement. Inspect prepared calls; do not replay source.",
                ));
                journal.put(&key, entry.version, json!(cell)).await?;
            }
            return Ok(cell);
        }
        let mut cell = Cell {
            id: input.id,
            owner: journal.owner().into(),
            fingerprint,
            source: input.source,
            status: CellStatus::Running,
            output: vec![],
            value: None,
            error: None,
        };
        let mut saved = journal.put(&key, 0, json!(cell)).await?;
        if *cancel.borrow() {
            cell.status = CellStatus::Interrupted;
            cell.error = Some(ToolError::rejected(
                "CELL_CANCELLED",
                "Cell cancelled before launch",
            ));
            journal.put(&key, saved.version, json!(cell)).await?;
            return Ok(cell);
        }
        let mut child = match sandbox::command(&self.executable, "--code-mode-runtime")
            .and_then(|mut c| c.spawn())
        {
            Ok(child) => child,
            Err(_) => {
                cell.status = CellStatus::Failed;
                cell.error = Some(ToolError::rejected(
                    "RUNTIME_UNAVAILABLE",
                    "Cannot start isolated JavaScript runtime",
                ));
                journal.put(&key, saved.version, json!(cell)).await?;
                return Ok(cell);
            }
        };
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let execution = async {
            stdin
                .write_all(&wire)
                .await
                .map_err(|_| Error::Invalid("Guest bridge closed"))?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|_| Error::Invalid("Guest bridge closed"))?;
            let mut count = 0;
            let mut output_bytes = 0;
            loop {
                let mut line = vec![];
                (&mut stdout)
                    .take((guest::FRAME_LIMIT + 1) as u64)
                    .read_until(b'\n', &mut line)
                    .await
                    .map_err(|_| Error::Invalid("Guest bridge closed"))?;
                if line.len() > guest::FRAME_LIMIT || !line.ends_with(b"\n") {
                    return Err(Error::Invalid("Invalid guest frame"));
                }
                let msg: Value = serde_json::from_slice(&line)
                    .map_err(|_| Error::Invalid("Invalid guest frame"))?;
                let reply = match msg["kind"].as_str() {
                    Some("result") => {
                        let value = msg.get("value").cloned().unwrap_or(Value::Null);
                        if serde_json::to_vec(&value)
                            .map_err(|_| Error::Storage)?
                            .len()
                            + output_bytes
                            > self.limits.output_bytes
                        {
                            return Err(Error::Invalid("Cell output exceeds configured limit"));
                        }
                        cell.value = Some(value);
                        cell.status = CellStatus::Completed;
                        return Ok(());
                    }
                    Some("failure") => {
                        let timeout = msg["code"] == "CELL_TIMEOUT";
                        cell.status = if timeout {
                            CellStatus::Interrupted
                        } else {
                            CellStatus::Failed
                        };
                        cell.error = Some(ToolError::rejected(
                            if timeout {
                                "CELL_TIMEOUT"
                            } else {
                                "JAVASCRIPT_ERROR"
                            },
                            &msg["message"]
                                .as_str()
                                .unwrap_or("JavaScript cell failed")
                                .chars()
                                .take(2048)
                                .collect::<String>(),
                        ));
                        return Ok(());
                    }
                    Some("text") => {
                        let value = msg.get("value").cloned().unwrap_or(Value::Null);
                        let bytes = serde_json::to_vec(&value)
                            .map_err(|_| Error::Storage)?
                            .len();
                        if bytes + output_bytes > self.limits.output_bytes
                            || cell.output.len() >= 1000
                        {
                            return Err(Error::Invalid("Cell output exceeds configured limit"));
                        }
                        output_bytes += bytes;
                        cell.output.push(value);
                        saved = journal.put(&key, saved.version, json!(cell)).await?;
                        json!({"value":null})
                    }
                    Some("call") => {
                        count += 1;
                        if count > self.limits.max_calls {
                            return Err(Error::Invalid("Nested call limit exceeded"));
                        }
                        let name = msg["tool"]
                            .as_str()
                            .ok_or(Error::Invalid("Invalid tool name"))?;
                        let tool = registry
                            .get(name)
                            .ok_or(Error::Invalid("Tool is not registered"))?;
                        let operation_key = format!("cm:{}:{count}", cell.id);
                        let prepared = dispatcher.prepare(
                            tool,
                            &operation_key,
                            msg.get("input").cloned().unwrap_or(Value::Null),
                        );
                        let (args, invalid) = match prepared {
                            Ok(args) => {
                                if serde_json::to_vec(&args).map_err(|_| Error::Storage)?.len()
                                    > self.limits.argument_bytes
                                {
                                    return Err(Error::Invalid(
                                        "Nested arguments exceed configured limit",
                                    ));
                                }
                                let valid =
                                    jsonschema::draft202012::is_valid(&tool.input_schema, &args);
                                (
                                    args,
                                    (!valid).then(|| {
                                        ToolError::rejected(
                                            "INVALID_TOOL_INPUT",
                                            "Arguments do not match the registered tool schema",
                                        )
                                    }),
                                )
                            }
                            Err(e) => (Value::Null, Some(e)),
                        };
                        let mut call = Call {
                            cell_id: cell.id,
                            sequence: count,
                            operation_key,
                            tool: name.into(),
                            effect: tool.effect,
                            input: args,
                            status: CallStatus::Prepared,
                            result: None,
                            error: None,
                        };
                        let call_key = call_key(cell.id, count);
                        // This write must commit before any external tool effect.
                        let entry = journal.put(&call_key, 0, json!(call)).await?;
                        let result = if let Some(error) = invalid {
                            Err(error)
                        } else {
                            dispatcher.invoke(&call).await
                        };
                        let result = match result {
                            Ok(v)
                                if serde_json::to_vec(&v).map_err(|_| Error::Storage)?.len()
                                    <= self.limits.result_bytes =>
                            {
                                Ok(v)
                            }
                            Ok(_) => Err(ToolError::uncertain(
                                "TOOL_RESULT_LIMIT",
                                "Tool returned more data than the cell can receive; inspect using the saved operation identity",
                            )),
                            Err(mut e) => {
                                e.code = e.code.chars().take(128).collect();
                                e.message = e.message.chars().take(2048).collect();
                                Err(e)
                            }
                        };
                        match &result {
                            Ok(v) => {
                                call.status = CallStatus::Succeeded;
                                call.result = Some(v.clone());
                            }
                            Err(e) => {
                                call.status = if e.uncertain {
                                    CallStatus::Uncertain
                                } else {
                                    CallStatus::Rejected
                                };
                                call.error = Some(e.clone());
                            }
                        }
                        // Lost journal acknowledgement must stop the cell, retaining
                        // the prepared identity rather than falsely reporting failure.
                        journal.put(&call_key, entry.version, json!(call)).await?;
                        match result {
                            Ok(v) => json!({"value":v}),
                            Err(e) => json!({"error":e,"callId":call_key}),
                        }
                    }
                    _ => return Err(Error::Invalid("Unknown guest frame")),
                };
                stdin
                    .write_all(reply.to_string().as_bytes())
                    .await
                    .map_err(|_| Error::Invalid("Guest bridge closed"))?;
                stdin
                    .write_all(b"\n")
                    .await
                    .map_err(|_| Error::Invalid("Guest bridge closed"))?;
            }
        };
        let cancellation = async {
            loop {
                if *cancel.borrow_and_update() {
                    break;
                }
                if cancel.changed().await.is_err() {
                    break;
                }
            }
        };
        let result = tokio::select! {
            result=tokio::time::timeout(Duration::from_millis(self.limits.timeout_ms),execution)=>match result {Ok(r)=>r,Err(_)=>Err(Error::Invalid("Cell deadline elapsed"))},
            _=cancellation=>Err(Error::Invalid("Cell cancelled or owner disconnected")),
        };
        let _ = child.kill().await;
        let _ = child.wait().await;
        match result {
            Ok(()) => {}
            Err(Error::Storage | Error::Conflict) => {
                // The guest is already dead. A lost journal acknowledgement may
                // have committed; read it back and seal this cell when storage is
                // reachable, without ever resubmitting the tool or source.
                let latest = journal.get(&key).await?.ok_or(Error::Storage)?;
                let stored: Cell =
                    serde_json::from_value(latest.value).map_err(|_| Error::Storage)?;
                if stored.status != CellStatus::Running {
                    return Ok(stored);
                }
                if stored.owner != cell.owner {
                    return Err(Error::Conflict);
                }
                saved.version = latest.version;
                cell.status = CellStatus::Interrupted;
                cell.error = Some(ToolError::uncertain(
                    "JOURNAL_INTERRUPTED",
                    "Journal acknowledgement was lost; inspect nested call records before continuing",
                ));
            }
            Err(e) => {
                cell.status = CellStatus::Interrupted;
                cell.error = Some(ToolError::uncertain("CELL_INTERRUPTED", &e.to_string()));
            }
        }
        journal.put(&key, saved.version, json!(cell)).await?;
        Ok(cell)
    }
}
