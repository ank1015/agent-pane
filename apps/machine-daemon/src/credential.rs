use std::path::{Path, PathBuf};

use execution_contracts::MachineId;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

const FILE_NAME: &str = "cloud-credential.json";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CloudCredential {
    pub machine_id: MachineId,
    pub credential: String,
    pub gateway_url: String,
    pub websocket_url: String,
}

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("failed to access credential {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("credential {} is invalid: {source}", path.display())]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
}

pub async fn load(state_directory: &Path) -> Result<Option<CloudCredential>, CredentialError> {
    let path = path(state_directory);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(CredentialError::Io { path, source }),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| CredentialError::Json { path, source })
}

pub async fn save(
    state_directory: &Path,
    credential: &CloudCredential,
) -> Result<(), CredentialError> {
    tokio::fs::create_dir_all(state_directory)
        .await
        .map_err(|source| CredentialError::Io {
            path: state_directory.to_owned(),
            source,
        })?;
    let destination = path(state_directory);
    let temporary = state_directory.join(format!(".{FILE_NAME}.{}.tmp", Uuid::now_v7()));
    let bytes = serde_json::to_vec_pretty(credential).map_err(|source| CredentialError::Json {
        path: destination.clone(),
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
        .map_err(|source| CredentialError::Io {
            path: temporary.clone(),
            source,
        })?;
    file.write_all(&bytes)
        .await
        .map_err(|source| CredentialError::Io {
            path: temporary.clone(),
            source,
        })?;
    file.flush().await.map_err(|source| CredentialError::Io {
        path: temporary.clone(),
        source,
    })?;
    drop(file);
    if let Err(source) = tokio::fs::rename(&temporary, &destination).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(CredentialError::Io {
            path: destination,
            source,
        });
    }
    Ok(())
}

fn path(state_directory: &Path) -> PathBuf {
    state_directory.join(FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn saves_and_loads_cloud_credentials() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let value = CloudCredential {
            machine_id: MachineId::new("machine-1").expect("machine id"),
            credential: "secret".to_owned(),
            gateway_url: "https://gateway.example.com".to_owned(),
            websocket_url: "wss://gateway.example.com/v1/machines/connect".to_owned(),
        };
        save(directory.path(), &value).await.expect("save");
        assert_eq!(load(directory.path()).await.expect("load"), Some(value));
    }
}
