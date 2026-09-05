use std::{
    fs::{File, OpenOptions},
    os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use execution_core::{ExecutionHostDescriptor, ExecutionRuntime, OperationContext};
use execution_supervisor_core::{SupervisorConfig, SupervisorRuntime};
use execution_wire::{NdjsonCodec, dispatch_request};
use tokio::{
    io::BufReader,
    net::{UnixListener, UnixStream},
    task::JoinSet,
};

/// Configuration for one local supervisor endpoint.
#[derive(Clone, Debug)]
pub struct ServeConfig {
    pub socket_path: PathBuf,
    pub supervisor: SupervisorConfig,
}

/// Runs one supervisor generation until `shutdown` is cancelled.
pub async fn serve(config: ServeConfig, shutdown: OperationContext) -> Result<()> {
    prepare_private_directory(&config.supervisor.state_directory).await?;
    let socket_parent = config
        .socket_path
        .parent()
        .context("supervisor socket must have a parent directory")?;
    prepare_socket_directory(socket_parent).await?;

    let state_lock_path = config.supervisor.state_directory.join("supervisor.lock");
    let socket_lock_path = socket_lock_path(&config.socket_path)?;
    let state_lock = acquire_lock(&state_lock_path).with_context(|| {
        format!(
            "another supervisor is using state directory {}",
            config.supervisor.state_directory.display()
        )
    })?;
    let socket_lock = acquire_lock(&socket_lock_path).with_context(|| {
        format!(
            "another supervisor is using socket {}",
            config.socket_path.display()
        )
    })?;

    remove_stale_socket(&config.socket_path)?;
    let listener = UnixListener::bind(&config.socket_path)
        .with_context(|| format!("bind supervisor socket {}", config.socket_path.display()))?;
    tokio::fs::set_permissions(&config.socket_path, std::fs::Permissions::from_mode(0o600))
        .await
        .with_context(|| {
            format!(
                "restrict supervisor socket permissions {}",
                config.socket_path.display()
            )
        })?;
    let endpoint = EndpointGuard {
        socket_path: config.socket_path,
        _state_lock: state_lock,
        _socket_lock: socket_lock,
    };

    let runtime = Arc::new(
        SupervisorRuntime::new(config.supervisor)
            .await
            .context("initialize supervisor runtime")?,
    );
    log_ready(runtime.descriptor(), &endpoint.socket_path);

    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("accept supervisor connection")?;
                let runtime = Arc::clone(&runtime);
                let connection_shutdown = shutdown.child();
                connections.spawn(async move {
                    if let Err(error) = handle_connection(runtime, stream, connection_shutdown).await {
                        tracing::warn!(%error, "supervisor connection failed");
                    }
                });
            }
            completed = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(error)) = completed {
                    tracing::warn!(%error, "supervisor connection task failed");
                }
            }
        }
    }

    shutdown.cancel();
    while let Some(completed) = connections.join_next().await {
        if let Err(error) = completed {
            tracing::warn!(%error, "supervisor connection task failed during shutdown");
        }
    }
    runtime.shutdown().await;
    drop(listener);
    drop(endpoint);
    Ok(())
}

async fn handle_connection(
    runtime: Arc<SupervisorRuntime>,
    stream: UnixStream,
    shutdown: OperationContext,
) -> Result<()> {
    let codec = NdjsonCodec::default();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    loop {
        let request = tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            request = codec.read_request(&mut reader) => request.context("read supervisor request")?,
        };
        let Some(request) = request else {
            return Ok(());
        };

        let operation = shutdown.child();
        let response = tokio::select! {
            () = shutdown.cancelled() => {
                operation.cancel();
                return Ok(());
            }
            response = dispatch_request(runtime.as_ref(), &operation, request) => response,
        };
        tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            result = codec.write_response(&mut write_half, &response) => {
                result.context("write supervisor response")?;
            }
        }
    }
}

async fn prepare_private_directory(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        bail!("supervisor directory path must not be empty");
    }
    tokio::fs::create_dir_all(path)
        .await
        .with_context(|| format!("create supervisor directory {}", path.display()))?;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .await
        .with_context(|| {
            format!(
                "restrict supervisor directory permissions {}",
                path.display()
            )
        })?;
    Ok(())
}

async fn prepare_socket_directory(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        bail!("supervisor socket directory path must not be empty");
    }
    tokio::fs::create_dir_all(path)
        .await
        .with_context(|| format!("create supervisor socket directory {}", path.display()))?;
    let metadata = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("inspect supervisor socket directory {}", path.display()))?;
    if !metadata.is_dir() {
        bail!(
            "supervisor socket parent {} is not a directory",
            path.display()
        );
    }
    Ok(())
}

fn acquire_lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("open supervisor lock {}", path.display()))?;
    fs2::FileExt::try_lock_exclusive(&file).with_context(|| format!("lock {}", path.display()))?;
    Ok(file)
}

fn socket_lock_path(socket_path: &Path) -> Result<PathBuf> {
    let file_name = socket_path
        .file_name()
        .context("supervisor socket must have a file name")?;
    let mut lock_name = file_name.to_os_string();
    lock_name.push(".lock");
    Ok(socket_path.with_file_name(lock_name))
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(path)
            .with_context(|| format!("remove stale supervisor socket {}", path.display())),
        Ok(_) => bail!(
            "refusing to replace non-socket supervisor path {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("inspect supervisor socket path {}", path.display()))
        }
    }
}

fn log_ready(descriptor: &ExecutionHostDescriptor, socket_path: &Path) {
    tracing::info!(
        host_id = %descriptor.host_id,
        generation_id = %descriptor.supervisor_generation_id,
        socket = %socket_path.display(),
        "execution supervisor ready"
    );
}

struct EndpointGuard {
    socket_path: PathBuf,
    _state_lock: File,
    _socket_lock: File,
}

impl Drop for EndpointGuard {
    fn drop(&mut self) {
        let should_remove = std::fs::symlink_metadata(&self.socket_path)
            .is_ok_and(|metadata| metadata.file_type().is_socket());
        if should_remove {
            if let Err(error) = std::fs::remove_file(&self.socket_path) {
                tracing::warn!(
                    %error,
                    socket = %self.socket_path.display(),
                    "failed to remove supervisor socket"
                );
            }
        }
    }
}
