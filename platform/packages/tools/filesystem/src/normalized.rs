use super::{ParentSegments, error, resolve_path, split_parts, validate_cwd};
use execution_core::{
    ExecutionErrorCode as Code, ExecutionHostDescriptor, ExecutionPath, ExecutionResult,
    PathConvention,
};

/// Codex-style lexical joining of native paths, followed by registered-root
/// mapping. Parent segments stop at the native drive/share/POSIX root. Actual
/// symlinks and containment remain the execution supervisor's responsibility.
pub fn resolve_path_normalized(
    host: &ExecutionHostDescriptor,
    cwd: &ExecutionPath,
    value: &str,
) -> ExecutionResult<ExecutionPath> {
    validate_cwd(host, cwd)?;
    let windows = host.path_convention == PathConvention::Windows;
    let root = host
        .roots
        .iter()
        .find(|root| root.id == cwd.root_id)
        .ok_or_else(|| error(Code::RootNotFound, "working directory root is unavailable"))?;
    let (base_prefix, mut base) =
        split_parts(&root.native_path, windows, ParentSegments::Preserve)?;
    if cwd.path != "." {
        base.extend(cwd.path.split('/').map(str::to_owned));
    }
    let value = if windows {
        value.replace('\\', "/")
    } else {
        value.to_owned()
    };
    let value = if value.is_empty() { "." } else { &value };
    let qualified;
    let value = if windows && value.starts_with('/') && !value.starts_with("//") {
        qualified = format!("{base_prefix}{value}");
        qualified.as_str()
    } else if windows
        && value.as_bytes().get(1) == Some(&b':')
        && value.as_bytes().get(2) != Some(&b'/')
    {
        if !value[..2].eq_ignore_ascii_case(&base_prefix) {
            return Err(error(
                Code::InvalidPath,
                "relative path belongs to another Windows drive",
            ));
        }
        if value.len() == 2 { "." } else { &value[2..] }
    } else {
        value
    };
    let (prefix, components) = split_parts(value, windows, ParentSegments::Preserve)?;
    let (prefix, mut resolved) = if prefix.is_empty() {
        (base_prefix, base)
    } else {
        (prefix, Vec::new())
    };
    for component in components {
        if component == ".." {
            resolved.pop();
        } else {
            resolved.push(component);
        }
    }
    let absolute = if prefix == "/" {
        format!("/{}", resolved.join("/"))
    } else {
        format!("{prefix}/{}", resolved.join("/"))
    };
    resolve_path(host, cwd, &absolute)
}
