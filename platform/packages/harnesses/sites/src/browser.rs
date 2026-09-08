use platform_agent_code_mode::code_mode::ToolError;
use platform_runtime_client::RunClient;
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
/// Operator-selected trusted Node executable and bundled browser helper. Never model paths.
#[derive(Clone)]
pub struct Browser {
    pub node: PathBuf,
    pub script: PathBuf,
}
impl Browser {
    pub(crate) async fn verify(
        &self,
        client: &RunClient,
        input: &Value,
    ) -> Result<Value, ToolError> {
        let preview = client
            .platform()
            .call("sites.preview", json!({}))
            .await
            .map_err(|_| failure("Could not obtain this site's live preview"))?;
        let url = preview["url"]
            .as_str()
            .ok_or_else(|| failure("Preview URL missing"))?;
        let mut command = tokio::process::Command::new(&self.node);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .arg(&self.script)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(cfg!(not(unix)))
            .spawn()
            .map_err(|_| failure("Browser helper could not start"))?;
        #[cfg(unix)]
        let _group = ProcessGroup(
            child
                .id()
                .ok_or_else(|| failure("Browser process ID missing"))? as i32,
        );
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap().take(65537);
        let result=tokio::time::timeout(Duration::from_secs(25),async {
            stdin.write_all(json!({"url":url,"selectors":input.get("selectors").cloned().unwrap_or(json!([]))}).to_string().as_bytes()).await?;
            stdin.shutdown().await?; drop(stdin);
            let mut bytes=Vec::new(); stdout.read_to_end(&mut bytes).await?;
            if bytes.len()>65536 {return Err(std::io::Error::other("Browser output limit"));}
            let status=child.wait().await?;
            if !status.success() {return Err(std::io::Error::other("Browser failed"));}
            Ok(bytes)
        }).await.map_err(|_|failure("Browser verification timed out"))?.map_err(|_|failure("Browser verification failed; check the installed browser runtime"))?;
        serde_json::from_slice(&result).map_err(|_| failure("Invalid browser report"))
    }
}
fn failure(message: &str) -> ToolError {
    ToolError::rejected("BROWSER_FAILED", message)
}

// A cancelled cell drops the future. SIGTERM asks the trusted helper to close
// Chromium (which can own a separate process group); its watchdog is a fallback.
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
