use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

const FILE_NAME: &str = "installation-identity.json";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredIdentity {
    installation_id: Uuid,
}

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("failed to access identity {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("identity {} is invalid: {source}", path.display())]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
}

pub async fn load_or_create(state_directory: &Path) -> Result<Uuid, IdentityError> {
    let destination = state_directory.join(FILE_NAME);
    match tokio::fs::read(&destination).await {
        Ok(bytes) => serde_json::from_slice::<StoredIdentity>(&bytes)
            .map(|value| value.installation_id)
            .map_err(|source| IdentityError::Json {
                path: destination,
                source,
            }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir_all(state_directory)
                .await
                .map_err(|source| IdentityError::Io {
                    path: state_directory.to_owned(),
                    source,
                })?;
            let value = StoredIdentity {
                installation_id: Uuid::now_v7(),
            };
            write_private_json(state_directory, &destination, &value).await?;
            Ok(value.installation_id)
        }
        Err(source) => Err(IdentityError::Io {
            path: destination,
            source,
        }),
    }
}

async fn write_private_json(
    state_directory: &Path,
    destination: &Path,
    value: &impl Serialize,
) -> Result<(), IdentityError> {
    let temporary = state_directory.join(format!(".{FILE_NAME}.{}.tmp", Uuid::now_v7()));
    let bytes = serde_json::to_vec_pretty(value).map_err(|source| IdentityError::Json {
        path: destination.to_owned(),
        source,
    })?;
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .await
        .map_err(|source| IdentityError::Io {
            path: temporary.clone(),
            source,
        })?;
    file.write_all(&bytes)
        .await
        .map_err(|source| IdentityError::Io {
            path: temporary.clone(),
            source,
        })?;
    file.flush().await.map_err(|source| IdentityError::Io {
        path: temporary.clone(),
        source,
    })?;
    drop(file);
    tokio::fs::rename(&temporary, destination)
        .await
        .map_err(|source| IdentityError::Io {
            path: destination.to_owned(),
            source,
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn installation_identity_is_stable() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let first = load_or_create(directory.path())
            .await
            .expect("create identity");
        let second = load_or_create(directory.path())
            .await
            .expect("load identity");
        assert_eq!(first, second);
    }
}
