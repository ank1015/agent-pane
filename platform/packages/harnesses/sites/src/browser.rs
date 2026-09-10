//! One trusted browser helper per run activation. The helper never receives
//! worker credentials; every backend admission goes through the leased client.
use futures_util::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
use platform_runtime_client::RunClient;
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::{Mutex, mpsc, oneshot},
};
use tool_code_mode::ToolError;
use uuid::Uuid;

#[derive(Clone)]
pub struct BrowserConfig {
    /// Operator-selected absolute paths, never model arguments.
    pub node: PathBuf,
    pub script: PathBuf,
}
impl BrowserConfig {
    fn command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.node);
        command
            .arg(&self.script)
            .env_clear()
            .env("PATH", "/usr/bin:/bin");
        // Playwright's installed browser cache, not credentials or gateway env.
        for name in ["HOME", "PLAYWRIGHT_BROWSERS_PATH", "TMPDIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        #[cfg(unix)]
        command.process_group(0);
        command.kill_on_drop(cfg!(not(unix)));
        command
    }

    /// Fail worker startup if the configured helper/browser cannot run safely.
    pub async fn check(&self) -> std::io::Result<()> {
        if !self.node.is_absolute()
            || !self.script.is_absolute()
            || !self.node.is_file()
            || !self.script.is_file()
        {
            return Err(std::io::Error::other(
                "Sites browser requires absolute Node and runtime.mjs paths",
            ));
        }
        let mut child = self
            .command()
            .arg("--check")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?;
        #[cfg(unix)]
        let _group = ProcessGroup(child.id().unwrap() as i32);
        let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
            .await
            .map_err(|_| std::io::Error::other("Sites browser startup check timed out"))??;
        if !status.success() {
            return Err(std::io::Error::other(
                "Sites browser startup check failed; install Playwright Chromium with native sandbox support",
            ));
        }
        Ok(())
    }
}

type Reply = std::result::Result<Value, ToolError>;
struct Request {
    input: Value,
    preview: Option<Value>,
    reply: oneshot::Sender<Reply>,
}
struct Actor {
    sender: mpsc::Sender<Request>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Actor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) struct BrowserSession {
    config: BrowserConfig,
    actor: Mutex<Option<Actor>>,
}
impl BrowserSession {
    pub fn new(config: BrowserConfig) -> Self {
        Self {
            config,
            actor: Mutex::new(None),
        }
    }

    pub async fn invoke(&self, client: &RunClient, input: &Value) -> Reply {
        // The mutex serializes whole browser actions, including across cells.
        let mut slot = self.actor.lock().await;
        let reload = input["action"] == "reload";
        if slot.as_ref().is_some_and(|actor| actor.sender.is_closed()) {
            if !reload {
                return Err(reset());
            }
            slot.take();
        }
        let preview = if slot.is_none() || reload {
            Some(client.platform().call("sites.preview", json!({})).await
                .map_err(|_| ToolError::rejected("BROWSER_PREVIEW_FAILED", "Could not obtain the bound site's preview. Check metadata and provisioning, then reload."))?)
        } else {
            None
        };
        if slot.is_none() {
            *slot = Some(start(&self.config, client.clone())?);
        }
        let (reply, receive) = oneshot::channel();
        slot.as_ref()
            .unwrap()
            .sender
            .send(Request {
                input: input.clone(),
                preview,
                reply,
            })
            .await
            .map_err(|_| reset())?;
        match tokio::time::timeout(Duration::from_secs(45), receive).await {
            Ok(Ok(result)) => result,
            _ => {
                // Keep a closed actor marker: only explicit reload may create a
                // new page after uncertain completion, never replay source.
                slot.as_ref().unwrap().task.abort();
                Err(reset())
            }
        }
    }
}
fn reset() -> ToolError {
    ToolError::uncertain(
        "BROWSER_RESET_REQUIRED",
        "Browser state was lost or the action was cancelled. Use reload to start a fresh preview. Accepted backend effects may continue; do not replay a mutation blindly.",
    )
}

fn start(config: &BrowserConfig, client: RunClient) -> std::result::Result<Actor, ToolError> {
    let mut child = config
        .command()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| {
            ToolError::rejected(
                "BROWSER_START_FAILED",
                "Could not start the configured Sites browser runtime.",
            )
        })?;
    #[cfg(unix)]
    let group = ProcessGroup(child.id().unwrap() as i32);
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, mut commands) = mpsc::channel::<Request>(1);
    let task = tokio::spawn(async move {
        #[cfg(unix)]
        let _group = group;
        let mut reader = BufReader::new(stdout).take(256 * 1024 + 1);
        let mut line = Vec::new();
        let mut current: Option<(String, oneshot::Sender<Reply>)> = None;
        let mut backend: FuturesUnordered<BoxFuture<'_, Value>> = FuturesUnordered::new();
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            let outbound = tokio::select! {
                command = commands.recv(), if current.is_none() => {
                    let Some(command) = command else { break; };
                    if command.reply.is_closed() { break; }
                    let id = Uuid::new_v4().to_string();
                    current = Some((id.clone(), command.reply));
                    Some(json!({"kind":"action","id":id,"input":command.input,"preview":command.preview}))
                }
                result = reader.read_until(b'\n', &mut line) => {
                    if !matches!(result, Ok(n) if n > 0) || line.len() > 256 * 1024 || line.last() != Some(&b'\n') { break; }
                    let Ok(frame) = serde_json::from_slice::<Value>(&line) else { break; };
                    line.clear(); reader.set_limit(256 * 1024 + 1);
                    match frame["kind"].as_str() {
                        Some("result") => {
                            let Some((id, reply)) = current.take() else { break; };
                            if frame["id"] != id { break; }
                            let result = if frame.get("error").is_some() {
                                serde_json::from_value(frame["error"].clone()).map_or_else(|_| Err(reset()), Err)
                            } else if frame["result"].to_string().len() <= 120 * 1024 {
                                Ok(frame["result"].clone())
                            } else { Err(ToolError::uncertain("BROWSER_RESULT_LIMIT", "Browser result exceeds transport limits.")) };
                            let _ = reply.send(result);
                        }
                        Some("invoke") => {
                            if backend.len() >= 8 { break; }
                            let request = frame["request"].clone();
                            let Some(id) = request["id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) else { break; };
                            let Some(release) = frame["release_id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) else { break; };
                            let client = client.clone();
                            backend.push(Box::pin(async move {
                                let result = client.platform().call("sites.browserInvoke", json!({
                                    "release_id":release,
                                    "input":{"method":"POST","path":request["endpoint"],"body":request.get("input").cloned().unwrap_or(Value::Null)},
                                    "options":{"idempotencyKey":format!("browser:{id}")}
                                })).await;
                                match result {
                                    Ok(result) => json!({"kind":"backend_result","id":id,"result":result}),
                                    Err(_) => json!({"kind":"backend_result","id":id,"error":"Backend bridge failed or authority expired. Accepted effects may continue; do not retry blindly."})
                                }
                            }));
                        }
                        _ => break,
                    }
                    None
                }
                Some(result) = backend.next(), if !backend.is_empty() => Some(result),
                _ = tick.tick() => {
                    // A cancelled tool future must stop page activity, even
                    // though this actor also services callbacks between calls.
                    if current.as_ref().is_some_and(|(_, reply)| reply.is_closed()) { break; }
                    None
                }
            };
            if let Some(value) = outbound {
                let mut bytes = value.to_string().into_bytes();
                bytes.push(b'\n');
                if bytes.len() > 256 * 1024 || stdin.write_all(&bytes).await.is_err() {
                    break;
                }
            }
        }
        // Advertise lost state before waiting for the browser process to exit;
        // otherwise a new call could queue behind an actor that is shutting down.
        commands.close();
        if let Some((_, reply)) = current {
            let _ = reply.send(Err(reset()));
        }
        drop(stdin);
        let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
    });
    Ok(Actor { sender, task })
}

// Chromium can have its own process group. SIGTERM lets the trusted Node helper
// close it; EOF and the helper's watchdog also clean up after worker death.
#[cfg(unix)]
struct ProcessGroup(i32);
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.0),
            nix::sys::signal::Signal::SIGTERM,
        );
    }
}
