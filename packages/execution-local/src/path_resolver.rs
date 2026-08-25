use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use execution_contracts::{
    ExecutionError, ExecutionErrorCode, GrantId, PathSpec, Validate, WorkspaceRoot, WorkspaceRootId,
};
use url::Url;

use crate::{
    error::{LocalExecutionError, error, io_error},
    runtime::LocalWorkspaceRoot,
};

/// Explicit authorization for native file URIs outside configured workspaces.
#[derive(Clone, Debug)]
pub struct LocalNativeGrant {
    pub id: GrantId,
    pub path: PathBuf,
    pub read_only: bool,
}

#[derive(Clone, Debug)]
struct RootEntry {
    descriptor: WorkspaceRoot,
    canonical_path: PathBuf,
}

#[derive(Clone, Debug)]
struct GrantEntry {
    canonical_path: PathBuf,
    read_only: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedLocalPath {
    pub path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct PathResolver {
    roots: HashMap<WorkspaceRootId, RootEntry>,
    grants: HashMap<GrantId, GrantEntry>,
}

impl PathResolver {
    pub async fn new(
        roots: Vec<LocalWorkspaceRoot>,
        grants: Vec<LocalNativeGrant>,
    ) -> Result<Self, LocalExecutionError> {
        if roots.is_empty() {
            return Err(LocalExecutionError::InvalidConfiguration(
                "at least one workspace root is required".to_owned(),
            ));
        }

        let mut resolved_roots = HashMap::new();
        for root in roots {
            let canonical_path = tokio::fs::canonicalize(&root.path).await?;
            let metadata = tokio::fs::metadata(&canonical_path).await?;
            if !metadata.is_dir() {
                return Err(LocalExecutionError::InvalidConfiguration(format!(
                    "workspace root {} is not a directory",
                    root.path.display()
                )));
            }
            let uri = Url::from_file_path(&canonical_path)
                .map_err(|()| {
                    LocalExecutionError::InvalidConfiguration(format!(
                        "workspace root {} cannot be represented as a file URI",
                        canonical_path.display()
                    ))
                })?
                .to_string();
            let descriptor = WorkspaceRoot {
                id: root.id.clone(),
                name: root.name,
                uri,
                read_only: root.read_only,
            };
            if resolved_roots
                .insert(
                    root.id,
                    RootEntry {
                        descriptor,
                        canonical_path,
                    },
                )
                .is_some()
            {
                return Err(LocalExecutionError::InvalidConfiguration(
                    "workspace root identifiers must be unique".to_owned(),
                ));
            }
        }

        let mut resolved_grants = HashMap::new();
        for grant in grants {
            let canonical_path = tokio::fs::canonicalize(&grant.path).await?;
            if resolved_grants
                .insert(
                    grant.id,
                    GrantEntry {
                        canonical_path,
                        read_only: grant.read_only,
                    },
                )
                .is_some()
            {
                return Err(LocalExecutionError::InvalidConfiguration(
                    "native grant identifiers must be unique".to_owned(),
                ));
            }
        }

        Ok(Self {
            roots: resolved_roots,
            grants: resolved_grants,
        })
    }

    pub fn workspace_roots(&self) -> Vec<WorkspaceRoot> {
        let mut roots: Vec<_> = self
            .roots
            .values()
            .map(|entry| entry.descriptor.clone())
            .collect();
        roots.sort_by(|left, right| left.id.cmp(&right.id));
        roots
    }

    pub async fn resolve_existing(
        &self,
        spec: &PathSpec,
        follow_final_symlink: bool,
    ) -> Result<ResolvedLocalPath, ExecutionError> {
        let (candidate, boundary, _read_only) = self.candidate(spec)?;
        let path = if follow_final_symlink || candidate == boundary {
            tokio::fs::canonicalize(&candidate)
                .await
                .map_err(|source| io_error(&candidate, source))?
        } else {
            let parent = candidate
                .parent()
                .ok_or_else(|| error(ExecutionErrorCode::InvalidPath, "path has no parent"))?;
            let canonical_parent = tokio::fs::canonicalize(parent)
                .await
                .map_err(|source| io_error(parent, source))?;
            let name = candidate
                .file_name()
                .ok_or_else(|| error(ExecutionErrorCode::InvalidPath, "path has no file name"))?;
            let path = canonical_parent.join(name);
            tokio::fs::symlink_metadata(&path)
                .await
                .map_err(|source| io_error(&path, source))?;
            path
        };
        ensure_contained(&path, &boundary)?;
        Ok(ResolvedLocalPath { path })
    }

    pub async fn resolve_for_write(
        &self,
        spec: &PathSpec,
        follow_final_symlink: bool,
    ) -> Result<ResolvedLocalPath, ExecutionError> {
        let (candidate, boundary, read_only) = self.candidate(spec)?;
        if read_only {
            return Err(error(
                ExecutionErrorCode::PermissionDenied,
                "target belongs to a read-only root or grant",
            ));
        }

        let path = if follow_final_symlink && tokio::fs::symlink_metadata(&candidate).await.is_ok()
        {
            tokio::fs::canonicalize(&candidate)
                .await
                .map_err(|source| io_error(&candidate, source))?
        } else {
            resolve_through_existing_ancestor(&candidate, &boundary).await?
        };
        ensure_contained(&path, &boundary)?;
        Ok(ResolvedLocalPath { path })
    }

    fn candidate(&self, spec: &PathSpec) -> Result<(PathBuf, PathBuf, bool), ExecutionError> {
        spec.validate().map_err(|validation| {
            error(
                ExecutionErrorCode::InvalidPath,
                format!("invalid path specification: {validation}"),
            )
        })?;
        match spec {
            PathSpec::Workspace { root_id, path } => {
                let root = self.roots.get(root_id).ok_or_else(|| {
                    error(
                        ExecutionErrorCode::InvalidPath,
                        format!("unknown workspace root `{root_id}`"),
                    )
                })?;
                let relative = if path == "." {
                    Path::new("")
                } else {
                    Path::new(path)
                };
                Ok((
                    root.canonical_path.join(relative),
                    root.canonical_path.clone(),
                    root.descriptor.read_only,
                ))
            }
            PathSpec::Native { uri, grant_id } => {
                let grant = self.grants.get(grant_id).ok_or_else(|| {
                    error(
                        ExecutionErrorCode::PermissionDenied,
                        format!("unknown native path grant `{grant_id}`"),
                    )
                })?;
                let url = Url::parse(uri).map_err(|source| {
                    error(
                        ExecutionErrorCode::InvalidPath,
                        format!("invalid native URI: {source}"),
                    )
                })?;
                if url.scheme() != "file" {
                    return Err(error(
                        ExecutionErrorCode::Unsupported,
                        "the local executor only supports file:// native URIs",
                    ));
                }
                let path = url.to_file_path().map_err(|()| {
                    error(
                        ExecutionErrorCode::InvalidPath,
                        "native file URI cannot be converted to a local path",
                    )
                })?;
                Ok((path, grant.canonical_path.clone(), grant.read_only))
            }
        }
    }
}

async fn resolve_through_existing_ancestor(
    candidate: &Path,
    boundary: &Path,
) -> Result<PathBuf, ExecutionError> {
    let mut ancestor = candidate;
    let mut missing = Vec::new();
    while tokio::fs::symlink_metadata(ancestor).await.is_err() {
        let name = ancestor.file_name().ok_or_else(|| {
            error(
                ExecutionErrorCode::InvalidPath,
                "path has no existing ancestor",
            )
        })?;
        missing.push(name.to_owned());
        ancestor = ancestor.parent().ok_or_else(|| {
            error(
                ExecutionErrorCode::InvalidPath,
                "path has no existing ancestor",
            )
        })?;
    }
    let mut resolved = tokio::fs::canonicalize(ancestor)
        .await
        .map_err(|source| io_error(ancestor, source))?;
    ensure_contained(&resolved, boundary)?;
    for component in missing.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn ensure_contained(path: &Path, boundary: &Path) -> Result<(), ExecutionError> {
    if path.starts_with(boundary) {
        Ok(())
    } else {
        Err(error(
            ExecutionErrorCode::PermissionDenied,
            format!(
                "resolved path {} escapes authorized root {}",
                path.display(),
                boundary.display()
            ),
        ))
    }
}
