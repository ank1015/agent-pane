//! Shared remote path resolution for filesystem tools.
use execution_core::{
    ExecutionErrorCode as Code, ExecutionHostDescriptor, ExecutionPath, ExecutionResult,
    PathConvention,
};

// Parse the remote host's paths without consulting the worker OS or filesystem.
// Reject '..' rather than lexically collapsing through a possible symlink.
fn parts(value: &str, windows: bool) -> ExecutionResult<(String, Vec<String>)> {
    if value.is_empty() || value.contains('\0') {
        return Err(error(
            Code::InvalidPath,
            "file_path must be nonempty and contain no NUL bytes",
        ));
    }
    let value = if windows {
        value.replace('\\', "/")
    } else {
        value.to_owned()
    };
    let (prefix, tail) = if windows && value.starts_with("//") {
        let tail = &value[2..];
        let mut segments = tail.splitn(3, '/');
        let server = segments.next().unwrap_or("");
        let share = segments.next().unwrap_or("");
        if server.is_empty()
            || share.is_empty()
            || [server, share]
                .iter()
                .any(|s| matches!(*s, "." | ".." | "?") || s.contains(':'))
        {
            return Err(error(
                Code::InvalidPath,
                "invalid UNC path or unsupported Windows device path",
            ));
        }
        (format!("//{server}/{share}"), segments.next().unwrap_or(""))
    } else if windows && value.as_bytes().get(1) == Some(&b':') {
        if !value.as_bytes()[0].is_ascii_alphabetic() || value.as_bytes().get(2) != Some(&b'/') {
            return Err(error(
                Code::InvalidPath,
                "use a fully qualified Windows drive path",
            ));
        }
        (value[..2].to_owned(), &value[3..])
    } else if let Some(tail) = value.strip_prefix('/') {
        if windows {
            return Err(error(
                Code::InvalidPath,
                "Windows absolute paths must include a drive or UNC share",
            ));
        }
        ("/".to_owned(), tail)
    } else {
        (String::new(), value.as_str())
    };
    let mut result = Vec::new();
    for part in tail.split('/') {
        if part == ".." {
            return Err(error(
                Code::InvalidPath,
                "parent (..) segments are unsupported; use an absolute path inside a registered root",
            ));
        }
        if part.is_empty() || part == "." {
            continue;
        }
        if windows && (part.contains(':') || part.ends_with(['.', ' '])) {
            return Err(error(
                Code::InvalidPath,
                "unsupported Windows path component",
            ));
        }
        result.push(part.to_owned());
    }
    Ok((prefix, result))
}

pub fn resolve_path(
    host: &ExecutionHostDescriptor,
    cwd: &ExecutionPath,
    value: &str,
) -> ExecutionResult<ExecutionPath> {
    validate_cwd(host, cwd)?;
    let windows = host.path_convention == PathConvention::Windows;
    let (prefix, components) = parts(value, windows)?;
    if prefix.is_empty() {
        let mut base = if cwd.path == "." {
            Vec::new()
        } else {
            cwd.path.split('/').map(str::to_owned).collect()
        };
        base.extend(components);
        return Ok(ExecutionPath::new(
            cwd.root_id.clone(),
            if base.is_empty() {
                ".".to_owned()
            } else {
                base.join("/")
            },
        )?);
    }
    // Exact component matching also respects case-sensitive directories on
    // Windows. Callers should use the root spelling advertised by the host.
    let mut candidates = Vec::new();
    for root in &host.roots {
        let (root_prefix, root_parts) = parts(&root.native_path, windows)?;
        let same_prefix = if windows {
            prefix.eq_ignore_ascii_case(&root_prefix)
        } else {
            prefix == root_prefix
        };
        if same_prefix && components.starts_with(&root_parts) {
            candidates.push((root, root_parts.len()));
        }
    }
    candidates.sort_by_key(|(_, len)| std::cmp::Reverse(*len));
    let Some((root, len)) = candidates.first() else {
        return Err(error(
            Code::PathOutsideRoot,
            "absolute path is outside the host's registered roots (use the advertised root spelling)",
        ));
    };
    if candidates
        .get(1)
        .is_some_and(|(_, other_len)| other_len == len)
    {
        return Err(error(
            Code::InvalidPath,
            "absolute path matches multiple registered roots; use a relative path in the selected root",
        ));
    }
    let relative = components[*len..].join("/");
    Ok(ExecutionPath::new(
        root.id.clone(),
        if relative.is_empty() {
            ".".to_owned()
        } else {
            relative
        },
    )?)
}

/// Validate an execution-root-relative working directory against a selected host.
pub fn validate_cwd(host: &ExecutionHostDescriptor, cwd: &ExecutionPath) -> ExecutionResult<()> {
    use execution_core::Validate;
    cwd.validate()?;
    if !host.roots.iter().any(|root| root.id == cwd.root_id) {
        return Err(error(
            Code::RootNotFound,
            "working directory root is not advertised by the host",
        ));
    }
    Ok(())
}

fn error(code: Code, message: impl Into<String>) -> execution_core::ExecutionError {
    execution_core::ExecutionError::new(code, message).with_detail("source", "tool-filesystem")
}

#[cfg(test)]
mod tests {
    use super::resolve_path as resolve;
    use super::*;
    use execution_core::*;

    fn host(convention: PathConvention, roots: &[(&str, &str)]) -> ExecutionHostDescriptor {
        ExecutionHostDescriptor {
            host_id: ExecutionHostId::generate(),
            supervisor_generation_id: SupervisorGenerationId::generate(),
            operating_system: if convention == PathConvention::Windows {
                OperatingSystem::Windows
            } else {
                OperatingSystem::Linux
            },
            architecture: "test".into(),
            path_convention: convention,
            roots: roots
                .iter()
                .map(|(id, path)| ExecutionRoot {
                    id: RootId::new(*id).unwrap(),
                    name: id.to_string(),
                    native_path: path.to_string(),
                    read_only: false,
                })
                .collect(),
            features: ExecutionFeatures {
                pty: false,
                process_signals: false,
                file_revisions: true,
            },
            limits: ExecutionLimits::default(),
        }
    }

    #[test]
    fn unix_paths_match_components_and_choose_the_most_specific_root() {
        let host = host(
            PathConvention::Unix,
            &[("work", "/work"), ("nested", "/work/nested")],
        );
        let cwd = ExecutionPath::new(RootId::new("work").unwrap(), "project").unwrap();
        let path = resolve(&host, &cwd, "./src//main.rs").unwrap();
        assert_eq!(path.path, "project/src/main.rs");
        assert_eq!(path.root_id, cwd.root_id);
        let path = resolve(&host, &cwd, "/work/nested/file").unwrap();
        assert_eq!(path.root_id.as_str(), "nested");
        assert_eq!(path.path, "file");
        assert_eq!(resolve(&host, &cwd, "/work").unwrap().path, ".");
        for value in ["/work-other/a", "/WORK/a", "../a", "sub/../a", "", "a\0b"] {
            assert!(resolve(&host, &cwd, value).is_err(), "{value:?}");
        }
        let duplicate = host_with_duplicate_root();
        assert!(resolve(&duplicate, &cwd, "/work/file").is_err());
        assert!(resolve(&duplicate, &cwd, "file").is_ok());
    }

    fn host_with_duplicate_root() -> ExecutionHostDescriptor {
        host(
            PathConvention::Unix,
            &[("work", "/work"), ("duplicate", "/work")],
        )
    }

    #[test]
    fn windows_drive_and_unc_paths_are_resolved_independently_of_worker_os() {
        let host = host(
            PathConvention::Windows,
            &[
                ("work", r"C:\Users\Ank\work"),
                ("share", r"\\server\share\project"),
            ],
        );
        let cwd = ExecutionPath::new(RootId::new("work").unwrap(), "src").unwrap();
        let path = resolve(&host, &cwd, r".\nested\file.rs").unwrap();
        assert_eq!(path.path, "src/nested/file.rs");
        let path = resolve(&host, &cwd, "c:/Users/Ank/work/file.rs").unwrap();
        assert_eq!(path.path, "file.rs");
        let path = resolve(&host, &cwd, r"\\SERVER\SHARE\project\file.txt").unwrap();
        assert_eq!(path.root_id.as_str(), "share");
        assert_eq!(path.path, "file.txt");
        for value in [
            r"C:relative.txt",
            r"\rooted.txt",
            r"\\?\C:\Users\Ank\work\a",
            r"\\.\pipe\name",
            r"C:\Users\Ank\work-other\a",
            r"..\a",
            "a:stream",
            "a.",
            "a ",
        ] {
            assert!(resolve(&host, &cwd, value).is_err(), "{value}");
        }
    }
}
