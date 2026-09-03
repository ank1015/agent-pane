use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

const FILE_NAME: &str = "gateway-credential.json";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostCredential {
    pub host_id: Uuid,
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

pub async fn load(state_directory: &Path) -> Result<Option<HostCredential>, CredentialError> {
    let path = state_directory.join(FILE_NAME);
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
    credential: &HostCredential,
) -> Result<(), CredentialError> {
    tokio::fs::create_dir_all(state_directory)
        .await
        .map_err(|source| CredentialError::Io {
            path: state_directory.to_owned(),
            source,
        })?;
    let destination = state_directory.join(FILE_NAME);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn credentials_round_trip_without_being_returned_from_metadata() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let value = HostCredential {
            host_id: Uuid::now_v7(),
            credential: "ehc_private".to_owned(),
            gateway_url: "https://execution.example.com".to_owned(),
            websocket_url: "wss://execution.example.com/v1/registered-hosts/id/connect".to_owned(),
        };
        save(directory.path(), &value)
            .await
            .expect("save credential");
        assert_eq!(
            load(directory.path()).await.expect("load credential"),
            Some(value)
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(directory.path().join(FILE_NAME))
                .expect("credential metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
