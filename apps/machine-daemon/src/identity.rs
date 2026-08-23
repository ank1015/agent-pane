use std::path::{Path, PathBuf};

use execution_contracts::MachineId;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("failed to access machine identity {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("machine identity {} is invalid: {source}", path.display())]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("machine identifier is invalid: {0}")]
    Invalid(String),
}

#[derive(Deserialize, Serialize)]
struct StoredIdentity {
    machine_id: String,
}

pub async fn load_or_create(
    state_directory: &Path,
    configured: Option<&str>,
) -> Result<MachineId, IdentityError> {
    if let Some(value) = configured {
        return MachineId::new(value.to_owned())
            .map_err(|source| IdentityError::Invalid(source.to_string()));
    }
    tokio::fs::create_dir_all(state_directory)
        .await
        .map_err(|source| IdentityError::Io {
            path: state_directory.to_owned(),
            source,
        })?;
    let path = state_directory.join("identity.json");
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let identity: StoredIdentity =
                serde_json::from_slice(&bytes).map_err(|source| IdentityError::Json {
                    path: path.clone(),
                    source,
                })?;
            MachineId::new(identity.machine_id)
                .map_err(|source| IdentityError::Invalid(source.to_string()))
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            let machine_id = MachineId::new(Uuid::now_v7().to_string())
                .expect("UUID machine identifier is valid");
            let bytes = serde_json::to_vec_pretty(&StoredIdentity {
                machine_id: machine_id.to_string(),
            })
            .expect("stored identity serializes");
            let temporary = state_directory.join("identity.json.tmp");
            tokio::fs::write(&temporary, bytes)
                .await
                .map_err(|source| IdentityError::Io {
                    path: temporary.clone(),
                    source,
                })?;
            tokio::fs::rename(&temporary, &path)
                .await
                .map_err(|source| IdentityError::Io {
                    path: path.clone(),
                    source,
                })?;
            Ok(machine_id)
        }
        Err(source) => Err(IdentityError::Io { path, source }),
    }
}
