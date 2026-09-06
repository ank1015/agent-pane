use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use execution_core::{
    ExecutionError, ExecutionErrorCode, ExecutionPath, ExecutionRoot, RootId, Validate,
};

use crate::{
    config::SupervisorRoot,
    error::{SupervisorInitError, error, io_error},
};

#[derive(Clone, Debug)]
struct RootEntry {
    descriptor: ExecutionRoot,
    canonical_path: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedPath {
    pub path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct PathResolver {
    roots: HashMap<RootId, RootEntry>,
}

impl PathResolver {
    pub async fn new(roots: Vec<SupervisorRoot>) -> Result<Self, SupervisorInitError> {
        if roots.is_empty() {
            return Err(SupervisorInitError::InvalidConfiguration(
                "at least one execution root is required".to_string(),
            ));
        }

        let mut entries = HashMap::new();
        for root in roots {
            if root.name.trim().is_empty() {
                return Err(SupervisorInitError::InvalidConfiguration(format!(
                    "execution root `{}` has an empty name",
                    root.id
                )));
            }
            let canonical_path = tokio::fs::canonicalize(&root.path).await?;
            let metadata = tokio::fs::metadata(&canonical_path).await?;
            if !metadata.is_dir() {
                return Err(SupervisorInitError::InvalidConfiguration(format!(
                    "execution root {} is not a directory",
                    root.path.display()
                )));
            }
            let descriptor = ExecutionRoot {
                id: root.id.clone(),
                name: root.name,
                native_path: canonical_path.to_string_lossy().into_owned(),
                read_only: root.read_only,
            };
            if entries
                .insert(
                    root.id,
                    RootEntry {
                        descriptor,
                        canonical_path,
                    },
                )
                .is_some()
            {
                return Err(SupervisorInitError::InvalidConfiguration(
                    "execution root identifiers must be unique".to_string(),
                ));
            }
        }
        Ok(Self { roots: entries })
    }

    pub fn descriptors(&self) -> Vec<ExecutionRoot> {
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
        spec: &ExecutionPath,
        follow_final_symlink: bool,
    ) -> Result<ResolvedPath, ExecutionError> {
        let (candidate, boundary, _) = self.candidate(spec)?;
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
            ensure_contained(&canonical_parent, &boundary)?;
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
        Ok(ResolvedPath { path })
    }

    pub async fn resolve_for_write(
        &self,
        spec: &ExecutionPath,
        follow_final_symlink: bool,
    ) -> Result<ResolvedPath, ExecutionError> {
        let (candidate, boundary, read_only) = self.candidate(spec)?;
        if read_only {
            return Err(error(
                ExecutionErrorCode::ReadOnlyRoot,
                format!("execution root `{}` is read-only", spec.root_id),
            ));
        }

        let candidate_metadata = tokio::fs::symlink_metadata(&candidate).await;
        let path = match (follow_final_symlink, candidate_metadata) {
            (true, Ok(_)) => tokio::fs::canonicalize(&candidate)
                .await
                .map_err(|source| io_error(&candidate, source))?,
            (false, Ok(_)) => {
                let parent = candidate
                    .parent()
                    .ok_or_else(|| error(ExecutionErrorCode::InvalidPath, "path has no parent"))?;
                let canonical_parent = tokio::fs::canonicalize(parent)
                    .await
                    .map_err(|source| io_error(parent, source))?;
                ensure_contained(&canonical_parent, &boundary)?;
                let name = candidate.file_name().ok_or_else(|| {
                    error(ExecutionErrorCode::InvalidPath, "path has no file name")
                })?;
                canonical_parent.join(name)
            }
            (_, Err(source)) if source.kind() == std::io::ErrorKind::NotFound => {
                resolve_through_existing_ancestor(&candidate, &boundary).await?
            }
            (_, Err(source)) => return Err(io_error(&candidate, source)),
        };
        ensure_contained(&path, &boundary)?;
        Ok(ResolvedPath { path })
    }

    pub async fn resolve_for_in_place_write(
        &self,
        spec: &ExecutionPath,
    ) -> Result<ResolvedPath, ExecutionError> {
        let (candidate, boundary, read_only) = self.candidate(spec)?;
        if read_only {
            return Err(error(
                ExecutionErrorCode::ReadOnlyRoot,
                format!("execution root `{}` is read-only", spec.root_id),
            ));
        }
        let path = resolve_for_creation(candidate, &boundary, 0).await?;
        Ok(ResolvedPath { path })
    }

    // Retain the logical parent spelling for mkdir: a dangling symlink is not
    // itself a missing directory. Validate its eventual target before use.
    pub async fn resolve_for_directory_creation(
        &self,
        spec: &ExecutionPath,
    ) -> Result<ResolvedPath, ExecutionError> {
        self.resolve_for_in_place_write(spec).await?;
        let (path, _, _) = self.candidate(spec)?;
        Ok(ResolvedPath { path })
    }

    fn candidate(&self, spec: &ExecutionPath) -> Result<(PathBuf, PathBuf, bool), ExecutionError> {
        spec.validate().map_err(|validation| {
            error(
                ExecutionErrorCode::InvalidPath,
                format!("invalid execution path: {validation}"),
            )
        })?;
        let root = self.roots.get(&spec.root_id).ok_or_else(|| {
            error(
                ExecutionErrorCode::RootNotFound,
                format!("execution root `{}` was not found", spec.root_id),
            )
        })?;
        let relative = if spec.path == "." {
            Path::new("")
        } else {
            Path::new(&spec.path)
        };
        Ok((
            root.canonical_path.join(relative),
            root.canonical_path.clone(),
            root.descriptor.read_only,
        ))
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

// Resolve links even when their eventual target does not exist yet. Every
// existing ancestor is canonicalized and containment is checked before callers
// create parents or open a file. Link traversal is bounded, including cycles.
fn resolve_for_creation(
    candidate: PathBuf,
    boundary: &Path,
    links: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<PathBuf, ExecutionError>> + Send + '_>>
{
    Box::pin(async move {
        if links > 40 {
            return Err(error(
                ExecutionErrorCode::InvalidPath,
                "too many symbolic links in write path",
            ));
        }
        let resolved = match tokio::fs::symlink_metadata(&candidate).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = tokio::fs::read_link(&candidate)
                    .await
                    .map_err(|source| io_error(&candidate, source))?;
                let target = if target.is_absolute() {
                    target
                } else {
                    candidate
                        .parent()
                        .ok_or_else(|| {
                            error(ExecutionErrorCode::InvalidPath, "link has no parent")
                        })?
                        .join(target)
                };
                resolve_for_creation(target, boundary, links + 1).await?
            }
            Ok(_) => tokio::fs::canonicalize(&candidate)
                .await
                .map_err(|source| io_error(&candidate, source))?,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                let parent = candidate.parent().ok_or_else(|| {
                    error(
                        ExecutionErrorCode::InvalidPath,
                        "path has no existing ancestor",
                    )
                })?;
                let name = candidate.file_name().ok_or_else(|| {
                    error(ExecutionErrorCode::InvalidPath, "path has no file name")
                })?;
                resolve_for_creation(parent.to_path_buf(), boundary, links)
                    .await?
                    .join(name)
            }
            Err(source) => return Err(io_error(&candidate, source)),
        };
        ensure_contained(&resolved, boundary)?;
        Ok(resolved)
    })
}

fn ensure_contained(path: &Path, boundary: &Path) -> Result<(), ExecutionError> {
    if path.starts_with(boundary) {
        Ok(())
    } else {
        Err(error(
            ExecutionErrorCode::PathOutsideRoot,
            format!(
                "resolved path {} escapes execution root {}",
                path.display(),
                boundary.display()
            ),
        ))
    }
}
