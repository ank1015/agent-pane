use execution_core::{CommandSpec, OperatingSystem};

use crate::BashConfig;

pub(crate) fn command(os: OperatingSystem, config: &BashConfig, command: String) -> CommandSpec {
    if os == OperatingSystem::Windows && config.shell.is_none() {
        // Use argv: the supervisor's Shell variant invokes COMSPEC-style /C,
        // which is not the PowerShell command contract. Do not shell-escape or
        // interpolate the model's script into another shell command.
        CommandSpec::Argv {
            program: "powershell.exe".into(),
            arguments: vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                command,
            ],
        }
    } else {
        CommandSpec::Shell {
            command,
            shell: config.shell.clone(),
            login: config.login,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_uses_powershell_argv_without_rewriting_script() {
        let script = "$ErrorActionPreference = 'Stop'; Write-Output $env:USERPROFILE; Get-Location";
        let result = command(
            OperatingSystem::Windows,
            &BashConfig::default(),
            script.into(),
        );
        let CommandSpec::Argv { program, arguments } = result else {
            panic!("expected argv")
        };
        assert_eq!(program, "powershell.exe");
        assert_eq!(
            arguments,
            [
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                script
            ]
        );
    }

    #[test]
    fn unix_and_explicit_overrides_keep_the_supervisor_shell_contract() {
        for (os, config) in [
            (OperatingSystem::Linux, BashConfig::default()),
            (
                OperatingSystem::Windows,
                BashConfig {
                    shell: Some("cmd.exe".into()),
                    ..Default::default()
                },
            ),
        ] {
            let CommandSpec::Shell {
                command: script,
                shell,
                login,
            } = command(os, &config, "echo test".into())
            else {
                panic!("expected shell")
            };
            assert_eq!(script, "echo test");
            assert_eq!(shell, config.shell);
            assert!(!login);
        }
    }
}
