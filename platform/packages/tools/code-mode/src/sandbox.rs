//! Shared fail-closed process isolation for disposable JavaScript guests.
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::process::Stdio;
use std::{io, path::Path};
use tokio::process::Command;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn command(_: &Path, _: &str) -> io::Result<Command> {
    Err(io::Error::other("No supported OS sandbox"))
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn command(executable: &Path, flag: &str) -> io::Result<Command> {
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
        cmd.args(["-p", &policy]).arg(&executable).arg(flag);
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
        .arg("/js-runtime")
        .args(["--chdir", "/tmp", "--", "/js-runtime", flag]);
        cmd
    };
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
