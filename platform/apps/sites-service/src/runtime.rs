//! Capability-only QuickJS guest in a fresh OS-sandboxed process. No JS host I/O
//! is installed. Parent owns SQLite and validates every message independently.
use crate::database::{Database, MAX_RESULT};
use rquickjs::{Context, Function, Module, Promise, Runtime};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Read, Write},
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

pub const FRAME_LIMIT: usize = 512 * 1024;
pub const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
pub struct GuestInput {
    pub code: String,
    pub context: Value,
    pub timeout_ms: u64,
}
#[derive(Default)]
pub struct Outcome {
    pub response: Option<Value>,
    pub error: Option<&'static str>,
    pub logs: Vec<Value>,
}

// Native bridge only speaks framed JSON on inherited pipes. It never opens a
// database, consults environment variables, or accesses a network/file API.
fn host_call(message: String) -> String {
    let result = (|| -> io::Result<String> {
        if message.len() > FRAME_LIMIT {
            return Err(io::Error::other("frame limit"));
        }
        let mut out = io::stdout().lock();
        out.write_all(message.as_bytes())?;
        out.write_all(b"\n")?;
        out.flush()?;
        let mut line = String::new();
        io::stdin()
            .lock()
            .take((FRAME_LIMIT + 1) as u64)
            .read_line(&mut line)?;
        if line.len() > FRAME_LIMIT || !line.ends_with('\n') {
            return Err(io::Error::other("invalid frame"));
        }
        Ok(line)
    })();
    result.unwrap_or_else(|_| {
        json!({"error":{"code":"BRIDGE_CLOSED","message":"Host bridge closed."}}).to_string()
    })
}

pub fn guest_main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut line = String::new();
    io::stdin()
        .lock()
        .take(32 * 1024 * 1024)
        .read_line(&mut line)?;
    let input: GuestInput = serde_json::from_str(&line)?;
    let deadline = Instant::now() + Duration::from_millis(input.timeout_ms);
    let runtime = Runtime::new()?;
    runtime.set_memory_limit(MEMORY_LIMIT);
    runtime.set_max_stack_size(512 * 1024);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let context = Context::full(&runtime)?;
    let result = context.with(|ctx| -> rquickjs::Result<String> {
        ctx.globals()
            .set("__sitesHost", Function::new(ctx.clone(), host_call)?)?;
        ctx.globals()
            .set("__sitesInput", input.context.to_string())?;
        let runner: Function = ctx.eval(include_str!("backend_sdk.js"))?;
        let (module, evaluated) = Module::declare(ctx.clone(), "backend.js", input.code)?.eval()?;
        evaluated.finish::<()>()?;
        let handler: Function = module.get("default")?;
        let response: Promise = runner.call((handler,))?;
        response.finish::<String>()
    });
    let message = match result {
        Ok(value) if value.len() <= MAX_RESULT => match serde_json::from_str::<Value>(&value) {
            Ok(response) => json!({"method":"result","args":response}),
            Err(_) => json!({"method":"failure","args":"INVALID_RESPONSE"}),
        },
        Ok(_) => json!({"method":"failure","args":"RESPONSE_LIMIT"}),
        Err(_) => {
            json!({"method":"failure","args": if Instant::now() >= deadline { "INVOCATION_TIMEOUT" } else { "BACKEND_ERROR" }})
        }
    };
    println!("{message}");
    Ok(())
}

pub(crate) fn sandbox_command(executable: &Path) -> io::Result<Command> {
    let executable = executable.canonicalize()?;
    #[cfg(target_os = "macos")]
    let mut command = {
        // Deny default, with read access only to the executable and system runtime.
        // The private storage volume and networking are not granted.
        let quoted = executable
            .to_str()
            .ok_or(io::Error::other("executable path"))?;
        if quoted.contains(['"', '\\', '\n']) {
            return Err(io::Error::other("executable path"));
        }
        let policy = format!(
            r#"(version 1)
(deny default)
(allow process-exec (literal "{quoted}"))
(allow file-read* (literal "/") (literal "{quoted}") (subpath "/System/Library") (subpath "/usr/lib") (literal "/dev/urandom") (literal "/dev/null"))
(allow sysctl-read)
(allow mach-lookup (global-name "com.apple.system.logger"))
"#
        );
        let mut cmd = Command::new("/usr/bin/sandbox-exec");
        cmd.args(["-p", &policy])
            .arg(&executable)
            .arg("--backend-runtime");
        cmd
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut cmd = Command::new("bwrap");
        cmd.args([
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
        ]);
        for path in ["/usr", "/lib", "/lib64"] {
            if Path::new(path).exists() {
                cmd.args(["--ro-bind", path, path]);
            }
        }
        cmd.args([
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--ro-bind",
        ])
        .arg(&executable)
        .arg("/backend-runtime")
        .args([
            "--chdir",
            "/tmp",
            "--",
            "/backend-runtime",
            "--backend-runtime",
        ]);
        cmd
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err(io::Error::other("No supported OS sandbox"));
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        command
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        Ok(command)
    }
}

pub async fn execute(input: GuestInput, db: Database, executable: &Path) -> Outcome {
    execute_with_platform(input, db, executable, None).await
}

pub async fn execute_with_platform(
    input: GuestInput,
    db: Database,
    executable: &Path,
    platform: Option<(crate::platform::PlatformClient, crate::platform::Scope)>,
) -> Outcome {
    let mut outcome = Outcome::default();
    let cancelled = db.cancellation();
    let timeout = Duration::from_millis(input.timeout_ms);
    let mut child = match sandbox_command(executable).and_then(|mut c| c.spawn()) {
        Ok(child) => child,
        Err(_) => {
            outcome.error = Some("RUNTIME_UNAVAILABLE");
            return outcome;
        }
    };
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    // Database work stays on one blocking thread, keeping transactions and hooks
    // on their owner. Dropping the sender closes the connection and rolls back.
    let (sender, mut receiver) =
        tokio::sync::mpsc::channel::<(String, Value, tokio::sync::oneshot::Sender<Value>)>(1);
    let db_task = tokio::task::spawn_blocking(move || {
        let mut db = db;
        while let Some((method, args, reply)) = receiver.blocking_recv() {
            let value = if method == "platform_check" {
                if db.in_transaction() {
                    json!({"error":{"code":"PLATFORM_IN_TRANSACTION","message":"Platform calls are unavailable inside database transactions."}})
                } else {
                    json!({"value":null})
                }
            } else if method == "finish" {
                if db.in_transaction() {
                    db.rollback();
                    json!({"error":{"code":"OPEN_TRANSACTION","message":"Uncommitted transaction."}})
                } else {
                    json!({"value":null})
                }
            } else {
                match db.call(&method, args) {
                    Ok(v) => json!({"value":v}),
                    Err(e) => json!({"error":e}),
                }
            };
            let _ = reply.send(value);
        }
    });
    let result = tokio::time::timeout(timeout + Duration::from_millis(100), async {
        stdin.write_all(serde_json::to_string(&input).unwrap().as_bytes()).await.map_err(|_| "RUNTIME_UNAVAILABLE")?;
        stdin.write_all(b"\n").await.map_err(|_| "RUNTIME_UNAVAILABLE")?;
        let mut calls = 0;
        let mut log_bytes = 0;
        loop {
            let mut line = Vec::new();
            (&mut stdout).take((FRAME_LIMIT + 1) as u64).read_until(b'\n', &mut line).await.map_err(|_| "BRIDGE_ERROR")?;
            if line.is_empty() { return Err("BACKEND_ERROR"); }
            if line.len() > FRAME_LIMIT || !line.ends_with(b"\n") { return Err("BRIDGE_LIMIT"); }
            let msg: Value = serde_json::from_slice(&line).map_err(|_| "BRIDGE_ERROR")?;
            let method = msg["method"].as_str().ok_or("BRIDGE_ERROR")?;
            let args = msg.get("args").cloned().unwrap_or(Value::Null);
            calls += 1;
            if calls > 1000 { return Err("CAPABILITY_LIMIT"); }
            if method == "failure" { return Err(match args.as_str() { Some("INVOCATION_TIMEOUT") => "INVOCATION_TIMEOUT", Some("RESPONSE_LIMIT") => "RESPONSE_LIMIT", Some("INVALID_RESPONSE") => "INVALID_RESPONSE", _ => "BACKEND_ERROR" }); }
            if method == "result" {
                if line.len() > MAX_RESULT { return Err("RESPONSE_LIMIT"); }
                let status = args["status"].as_u64().ok_or("INVALID_RESPONSE")?;
                if !(200..=599).contains(&status) || !args.is_object() || args.get("body").is_none() || args.as_object().unwrap().keys().any(|k| k != "status" && k != "body") { return Err("INVALID_RESPONSE"); }
                let (tx,rx) = tokio::sync::oneshot::channel();
                sender.send(("finish".into(),Value::Null,tx)).await.map_err(|_| "BRIDGE_ERROR")?;
                if rx.await.map_err(|_| "BRIDGE_ERROR")?.get("error").is_some() { return Err("OPEN_TRANSACTION"); }
                outcome.response = Some(args);
                return Ok(());
            }
            let response = if method.starts_with("platform.") {
                let name=method.strip_prefix("platform.").unwrap();
                if !crate::platform::allowed(name) { return Err("UNKNOWN_CAPABILITY"); }
                let (tx,rx)=tokio::sync::oneshot::channel();
                sender.send(("platform_check".into(),Value::Null,tx)).await.map_err(|_| "BRIDGE_ERROR")?;
                let check=rx.await.map_err(|_| "BRIDGE_ERROR")?;
                if check.get("error").is_some(){check}
                else if let Some((client,scope))=&platform {client.call(scope,name,args).await}
                else {json!({"error":{"code":"PLATFORM_UNAVAILABLE","message":"Platform integration is not configured."}})}
            } else if method == "log" {
                let size = serde_json::to_vec(&args).unwrap().len();
                if !matches!(args["level"].as_str(), Some("info" | "warn" | "error")) || !args["message"].is_string() || size > 4096 || outcome.logs.len() >= 100 || log_bytes + size > 32768 {
                    json!({"error":{"code":"LOG_LIMIT","message":"Log shape or size limit exceeded."}})
                } else { log_bytes += size; outcome.logs.push(args); json!({"value":null}) }
            } else if matches!(method, "query" | "execute" | "batch" | "begin" | "commit" | "rollback" | "check") {
                let (tx,rx) = tokio::sync::oneshot::channel();
                sender.send((method.into(),args,tx)).await.map_err(|_| "BRIDGE_ERROR")?;
                rx.await.map_err(|_| "BRIDGE_ERROR")?
            } else { return Err("UNKNOWN_CAPABILITY"); };
            stdin.write_all(response.to_string().as_bytes()).await.map_err(|_| "BRIDGE_ERROR")?;
            stdin.write_all(b"\n").await.map_err(|_| "BRIDGE_ERROR")?;
        }
    }).await;
    outcome.error = match result {
        Ok(Ok(())) => None,
        Ok(Err(code)) => Some(code),
        Err(_) => Some("INVOCATION_TIMEOUT"),
    };
    cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
    // Always terminate/reap the disposable guest, even after a valid response.
    let _ = child.kill().await;
    let _ = child.wait().await;
    drop(sender);
    if db_task.await.is_err() {
        outcome.error = Some("DATABASE_TASK_FAILED");
        outcome.response = None;
    }
    outcome
}

#[cfg(all(test, target_os = "macos"))]
mod sandbox_tests {
    use super::*;
    #[tokio::test]
    async fn os_sandbox_denies_private_files_writes_and_network() {
        let root = tempfile::TempDir::new().unwrap();
        let secret = root.path().join("private-data");
        std::fs::write(&secret, "must stay private").unwrap();
        let probe = root.path().join("probe");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let source = format!(
            r#"
#include <stdio.h>
#include <unistd.h>
#include <sys/socket.h>
#include <arpa/inet.h>
int main(void) {{
  FILE *f=fopen("{}","r"); if(f) return 10;
  f=fopen("{}/forbidden-write","w"); if(f) return 11;
  int s=socket(AF_INET, SOCK_STREAM, 0);
  struct sockaddr_in a={{0}}; a.sin_family=AF_INET; a.sin_port=htons({port}); a.sin_addr.s_addr=htonl(INADDR_LOOPBACK);
  if(s>=0 && connect(s,(struct sockaddr*)&a,sizeof(a))==0) return 12;
  return 0;
}}
"#,
            secret.display(),
            root.path().display()
        );
        let code = root.path().join("probe.c");
        std::fs::write(&code, source).unwrap();
        let compiled = std::process::Command::new("cc")
            .arg(&code)
            .arg("-o")
            .arg(&probe)
            .status()
            .unwrap();
        assert!(compiled.success());
        let status = tokio::time::timeout(
            Duration::from_secs(5),
            sandbox_command(&probe).unwrap().status(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(status.success(), "OS sandbox probe failed: {status}");
        assert!(!root.path().join("forbidden-write").exists());
    }
}
