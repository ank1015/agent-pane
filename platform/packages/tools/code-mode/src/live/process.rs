use super::*;
use crate::{Call, CallStatus, ToolError};
use futures_util::future::{AbortHandle, Abortable};
use futures_util::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub(super) struct Process {
    pub executable: PathBuf,
    pub registry: Registry,
    pub dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    pub extensions: Vec<Extension>,
    pub store: Arc<Mutex<BTreeMap<String, Value>>>,
    pub state: Arc<Mutex<CellState>>,
    pub changed: Arc<Notify>,
    pub notifications: mpsc::Sender<Notification>,
    pub resume: Arc<Notify>,
    pub id: String,
    pub call_id: String,
}

impl Process {
    pub async fn run(self, source: String, mut cancelled: watch::Receiver<bool>) {
        let outcome = tokio::select! {
            result = self.work(source) => result,
            _ = async { loop { if *cancelled.borrow_and_update() { break; }
                if cancelled.changed().await.is_err() { break; } } } => Ok(Status::Terminated),
        };
        let mut state = self.state.lock().unwrap();
        match outcome {
            Ok(status) => state.status = status,
            Err(error) => {
                state.status = Status::Failed;
                state.error = Some(error);
            }
        }
        self.changed.notify_one();
    }

    async fn work(&self, source: String) -> Result<Status, String> {
        let mut child = crate::sandbox::command(&self.executable, "--live-code-mode-runtime")
            .and_then(|mut command| command.spawn())
            .map_err(|_| "Cannot start isolated code-mode runtime".to_owned())?;
        let mut stdin = child.stdin.take().ok_or("Guest stdin unavailable")?;
        let mut lines =
            BufReader::new(child.stdout.take().ok_or("Guest stdout unavailable")?).lines();
        let definitions: Vec<_> = self
            .registry
            .tools()
            .map(|tool| {
                json!({
                    "name": protocol::normalize_name(&tool.name), "original": tool.name,
                    "description": tool.description
                })
            })
            .collect();
        let wire = json!({"source":source,"tools":definitions,"extensions":self.extensions});
        let mut bootstrap = serde_json::to_vec(&wire).map_err(|e| e.to_string())?;
        if bootstrap.len() > 2 * 1024 * 1024 {
            return Err("Code-mode bootstrap exceeds 2 MiB".into());
        }
        bootstrap.push(b'\n');
        stdin
            .write_all(&bootstrap)
            .await
            .map_err(|_| "Guest bridge closed")?;
        let mut pending: FuturesUnordered<BoxFuture<'_, (u64, Value)>> = FuturesUnordered::new();
        let mut calls = 0_u32;
        let mut timers = BTreeMap::new();
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let line = line.map_err(|_| "Guest bridge failed")?.ok_or("Guest exited without a result")?;
                    if line.len() > 256 * 1024 { return Err("Guest frame exceeds 256 KiB".into()); }
                    let frame: Value = serde_json::from_str(&line).map_err(|_| "Invalid guest frame")?;
                    match frame["kind"].as_str() {
                        Some("done") => { let _ = child.kill().await; let _ = child.wait().await; return Ok(Status::Completed); }
                        Some("failure") => return Err(frame["message"].as_str().unwrap_or("JavaScript error").chars().take(2048).collect()),
                        Some("output") => {
                            let item: ContentItem = serde_json::from_value(frame["item"].clone()).map_err(|_| "Invalid output item")?;
                            let mut state = self.state.lock().unwrap();
                            state.bytes += line.len();
                            if state.bytes > 256 * 1024 || state.output.len() >= 1000 { return Err("Uncollected cell output exceeds limit; use yield_control()".into()); }
                            state.output.push(item);
                        }
                        Some("yield") => {
                            if pending.len() >= 1024 { return Err("Too many pending cell operations".into()); }
                            let id = frame["id"].as_u64().ok_or("Invalid yield ID")?;
                            self.state.lock().unwrap().yielded = true;
                            self.changed.notify_one();
                            pending.push(Box::pin(async move { self.resume.notified().await; (id, json!({"value":null})) }));
                        }
                        Some("notify") => {
                            let text = frame["text"].as_str().ok_or("Invalid notification")?.to_owned();
                            self.notifications.send(Notification { call_id: self.call_id.clone(), cell_id: self.id.clone(), text })
                                .await.map_err(|_| "Notification receiver closed")?;
                        }
                        Some("store") => {
                            let key = frame["key"].as_str().ok_or("Invalid store key")?;
                            let reply = {
                                let mut store = self.store.lock().unwrap();
                                let mut candidate = store.clone();
                                candidate.insert(key.to_owned(), frame["value"].clone());
                                if serde_json::to_vec(&candidate).map_err(|_| "Invalid store")?.len() > 128 * 1024 {
                                    json!({"sync":true,"error":"Session store exceeds 128 KiB"})
                                } else { *store = candidate; json!({"sync":true}) }
                            };
                            stdin.write_all(format!("{reply}\n").as_bytes()).await.map_err(|_| "Guest bridge closed")?;
                        }
                        Some("load") => {
                            let key = frame["key"].as_str().ok_or("Invalid store key")?;
                            let mut reply = json!({"sync":true});
                            if let Some(value) = self.store.lock().unwrap().get(key) { reply["value"] = value.clone(); }
                            stdin.write_all(format!("{reply}\n").as_bytes()).await.map_err(|_| "Guest bridge closed")?;
                        }
                        Some("clear_timer") => {
                            if let Some(handle) = frame["id"].as_u64().and_then(|id| timers.remove(&id)) {
                                AbortHandle::abort(&handle);
                            }
                        }
                        Some("timer") => {
                            let id = frame["id"].as_u64().ok_or("Invalid timer")?;
                            let delay = frame["delay"].as_u64().unwrap_or(0).min(2_147_483_647);
                            if pending.len() >= 1024 { return Err("Too many pending cell operations".into()); }
                            let (handle, registration) = AbortHandle::new_pair();
                            timers.insert(id, handle);
                            pending.push(Box::pin(async move {
                                let cancelled = Abortable::new(tokio::time::sleep(std::time::Duration::from_millis(delay)), registration).await.is_err();
                                (id, json!({"value":null,"cancelled":cancelled})) }));
                        }
                        Some("call") => {
                            calls += 1;
                            if calls > 10_000 || pending.len() >= 1024 { return Err("Nested call limit exceeded".into()); }
                            let id = frame["id"].as_u64().ok_or("Invalid call ID")?;
                            let tool = self.registry.get(frame["tool"].as_str().ok_or("Invalid tool name")?).ok_or("Unknown tool")?;
                            let key = format!("cm:{}:{calls}", self.id);
                            let input = frame.get("input").cloned().unwrap_or(Value::Null);
                            pending.push(Box::pin(async move {
                                let result = async {
                                    let input = self.dispatcher.prepare(tool, &key, input)?;
                                    if input.to_string().len() > 65_536 || !jsonschema::draft202012::is_valid(&tool.input_schema, &input) {
                                        return Err(ToolError::rejected("INVALID_TOOL_INPUT", "Arguments do not match the registered schema or exceed 64 KiB"));
                                    }
                                    let call = Call { cell_id: self.id.parse().unwrap(), sequence: calls,
                                        operation_key: key, tool: tool.name.clone(), effect: tool.effect,
                                        input, status: CallStatus::Prepared, result: None, error: None };
                                    let value = self.dispatcher.invoke(&call).await?;
                                    if value.to_string().len() > 128 * 1024 { return Err(ToolError::uncertain("TOOL_RESULT_LIMIT", "Tool result exceeds 128 KiB")); }
                                    Ok(value)
                                }.await;
                                (id, match result { Ok(value) => json!({"value":value}), Err(e) => json!({"error":e}) })
                            }));
                        }
                        _ => return Err("Unknown guest frame".into()),
                    }
                }
                Some((id, mut reply)) = pending.next(), if !pending.is_empty() => {
                    timers.remove(&id);
                    if reply["cancelled"].as_bool() == Some(true) { continue; }
                    reply["id"] = json!(id);
                    let mut bytes = reply.to_string().into_bytes(); bytes.push(b'\n');
                    stdin.write_all(&bytes).await.map_err(|_| "Guest bridge closed")?;
                }
            }
        }
    }
}
