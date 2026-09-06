//! Host-side direct shell discovery and argument construction. Legacy Shell
//! requests deliberately keep their original launch behavior.
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Sh,
    Bash,
    Zsh,
    PowerShell,
    Cmd,
}

fn classify(path: &str) -> Option<Kind> {
    let name = path.rsplit(['/', '\\']).next()?.trim_end_matches(".exe");
    match name {
        "sh" => Some(Kind::Sh),
        "bash" => Some(Kind::Bash),
        "zsh" => Some(Kind::Zsh),
        "powershell" | "pwsh" => Some(Kind::PowerShell),
        "cmd" => Some(Kind::Cmd),
        _ => None,
    }
}

#[cfg(unix)]
fn user_shell() -> Option<PathBuf> {
    nix::unistd::User::from_uid(nix::unistd::Uid::current())
        .ok()
        .flatten()
        .map(|user| user.shell)
}
#[cfg(not(unix))]
fn user_shell() -> Option<PathBuf> {
    None
}

fn discover(kind: Kind) -> Option<PathBuf> {
    if let Some(path) = user_shell()
        && classify(&path.to_string_lossy()) == Some(kind)
        && path.is_file()
    {
        return Some(path);
    }
    let candidates: &[(&str, &[&str])] = match kind {
        Kind::Sh => &[("sh", &["/bin/sh"])],
        Kind::Bash => &[("bash", &["/bin/bash", "/usr/bin/bash"])],
        Kind::Zsh => &[("zsh", &["/bin/zsh"])],
        Kind::Cmd => &[("cmd", &[])],
        Kind::PowerShell => {
            if cfg!(windows) {
                &[
                    ("pwsh", &[r"C:\Program Files\PowerShell\7\pwsh.exe"]),
                    (
                        "powershell",
                        &[r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"],
                    ),
                ]
            } else {
                &[("pwsh", &["/usr/local/bin/pwsh"]), ("powershell", &[])]
            }
        }
    };
    for (binary, fallbacks) in candidates {
        if let Ok(path) = which::which(binary) {
            return Some(path);
        }
        if let Some(path) = fallbacks.iter().map(Path::new).find(|path| path.is_file()) {
            return Some(path.to_owned());
        }
    }
    None
}

fn fallback() -> (Kind, PathBuf) {
    if cfg!(windows) {
        (Kind::Cmd, "cmd.exe".into())
    } else {
        (Kind::Sh, "/bin/sh".into())
    }
}

fn select(shell: Option<&str>) -> (Kind, PathBuf) {
    if let Some(shell) = shell {
        return classify(shell)
            .and_then(|kind| discover(kind).map(|path| (kind, path)))
            .unwrap_or_else(fallback);
    }
    let preferred = if cfg!(windows) {
        Some(Kind::PowerShell)
    } else {
        user_shell().and_then(|path| classify(&path.to_string_lossy()))
    };
    let alternatives = if cfg!(windows) {
        vec![]
    } else if cfg!(target_os = "macos") {
        vec![Kind::Zsh, Kind::Bash]
    } else {
        vec![Kind::Bash, Kind::Zsh]
    };
    preferred
        .into_iter()
        .chain(alternatives)
        .find_map(|kind| discover(kind).map(|path| (kind, path)))
        .unwrap_or_else(fallback)
}

fn arguments(kind: Kind, command: &str, login: bool) -> Vec<String> {
    let flags = match kind {
        Kind::Sh | Kind::Bash | Kind::Zsh => vec![if login { "-lc" } else { "-c" }],
        Kind::PowerShell => {
            if login {
                vec!["-Command"]
            } else {
                vec!["-NoProfile", "-Command"]
            }
        }
        Kind::Cmd => vec!["/c"],
    };
    flags
        .into_iter()
        .map(str::to_owned)
        .chain([command.to_owned()])
        .collect()
}

pub(super) fn resolve(shell: Option<&str>, command: &str, login: bool) -> (PathBuf, Vec<String>) {
    let (kind, program) = select(shell);
    (program, arguments(kind, command, login))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments_follow_shell_type_on_every_os() {
        for name in [
            "/bin/bash",
            r"C:\Program Files\Git\bin\bash.exe",
            "zsh",
            "sh",
        ] {
            let kind = classify(name).unwrap();
            assert_eq!(arguments(kind, "", true), ["-lc", ""]);
            assert_eq!(arguments(kind, "echo ok", false), ["-c", "echo ok"]);
        }
        for name in ["pwsh", "powershell.exe", "/usr/bin/pwsh"] {
            let kind = classify(name).unwrap();
            assert_eq!(
                arguments(kind, "echo ok", false),
                ["-NoProfile", "-Command", "echo ok"]
            );
            assert_eq!(arguments(kind, "echo ok", true), ["-Command", "echo ok"]);
        }
        assert_eq!(arguments(Kind::Cmd, "echo ok", true), ["/c", "echo ok"]);
    }
}
