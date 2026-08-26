use std::{sync::Arc, time::Duration};

use execution_contracts::{
    CommandSpec, CreateDirectoryRequest, EnvironmentId, ExecutionId, ExecutionPersistence,
    ExecutionPolicy, ExecutionState, MachineId, NetworkMode, OperationId, PathSpec,
    ProcessOutputPolicy, SandboxMode, StartExecutionRequest, StdinMode, WorkspaceRootId,
};
use execution_protocol::MachineSummary;
use execution_runtime::{ExecutionRuntime, OperationContext};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    db::{Database, DbError},
    sandbox_accounts::{
        SandboxAccount, SandboxAccountError, SandboxAccountService, SandboxCredentials,
        SandboxProvider,
    },
    sandbox_machines::{
        NewSandboxMachine, SandboxEnvironmentResource, SandboxSecretError, SandboxSecretStore,
    },
    sandbox_provider::{
        HttpSandboxProviderClient, ProvisionedSandbox, SandboxProviderClient, SandboxProviderError,
    },
    sandbox_runtime::{
        SANDBOX_ROOT_ID, SandboxRuntimeError, build_runtime, sandbox_machine_record,
    },
    sandbox_templates::{SandboxEnvironmentInstance, environment_path},
    snapshots::{CreateSnapshotRequest, Snapshot},
};

#[derive(Clone, Debug)]
pub struct CreatedE2bSandboxMachine {
    pub machine: MachineSummary,
    pub sandbox_account_id: Uuid,
    pub sandbox_id: String,
    pub template_id: String,
}

#[derive(Clone)]
pub struct SandboxMaterializer {
    database: Database,
    accounts: SandboxAccountService,
    secrets: SandboxSecretStore,
    provider: Arc<dyn SandboxProviderClient>,
}

impl SandboxMaterializer {
    pub fn new(
        database: Database,
        accounts: SandboxAccountService,
        secrets: SandboxSecretStore,
    ) -> Self {
        Self::with_provider(
            database,
            accounts,
            secrets,
            Arc::new(HttpSandboxProviderClient),
        )
    }

    pub const fn with_provider(
        database: Database,
        accounts: SandboxAccountService,
        secrets: SandboxSecretStore,
        provider: Arc<dyn SandboxProviderClient>,
    ) -> Self {
        Self {
            database,
            accounts,
            secrets,
            provider,
        }
    }

    pub async fn materialize(
        &self,
        template_id: Uuid,
    ) -> Result<SandboxEnvironmentInstance, SandboxMaterializationError> {
        let template = self
            .database
            .sandbox_environment_template(template_id)
            .await?
            .ok_or(SandboxMaterializationError::TemplateNotFound)?;
        let snapshot = self
            .database
            .snapshot(template.snapshot_id)
            .await?
            .ok_or(SandboxMaterializationError::SnapshotNotFound)?;
        let account = self
            .accounts
            .resolve(snapshot.provider, Some(snapshot.sandbox_account_id))
            .await?;
        let credentials = self
            .accounts
            .credentials(account.id)
            .await?
            .ok_or(SandboxMaterializationError::AccountCredentialsMissing)?;
        let machine_id = id::<MachineId>(Uuid::now_v7().to_string())?;
        let target_name = format!("agent-pane-{}", Uuid::now_v7().simple());
        let provisioned = self
            .provider
            .create(&snapshot, &account, &credentials, &target_name)
            .await?;
        let machine_name =
            materialized_machine_name(&template.name, &provisioned.provider_resource_id);
        let record = sandbox_machine_record(
            machine_id.clone(),
            account.id,
            snapshot.provider,
            provisioned.provider_resource_id.clone(),
            provisioned.connection_config.clone(),
            provisioned.provider_metadata.clone(),
            account.config.clone(),
        );
        let runtime = match build_runtime(
            &record,
            &machine_name,
            credentials.api_key(),
            provisioned.connection_secret.as_deref(),
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                self.cleanup_unrecorded(&account, &credentials, &provisioned)
                    .await;
                return Err(error.into());
            }
        };
        let encrypted_secret = match provisioned
            .connection_secret
            .as_deref()
            .map(|secret| self.secrets.encrypt(&machine_id, secret))
            .transpose()
        {
            Ok(secret) => secret,
            Err(error) => {
                self.cleanup_unrecorded(&account, &credentials, &provisioned)
                    .await;
                return Err(error.into());
            }
        };
        if let Err(error) = self
            .database
            .create_sandbox_machine(NewSandboxMachine {
                machine_id: &machine_id,
                name: &machine_name,
                descriptor: runtime.descriptor(),
                sandbox_account_id: account.id,
                provider_resource_id: &provisioned.provider_resource_id,
                connection_config: &provisioned.connection_config,
                provider_metadata: &provisioned.provider_metadata,
                connection_secret: encrypted_secret,
            })
            .await
        {
            self.cleanup_unrecorded(&account, &credentials, &provisioned)
                .await;
            return Err(error.into());
        }
        if let Err(error) =
            prepare_environment(runtime.as_ref(), &template.cwd, &template.creation_script).await
        {
            self.cleanup_recorded(&machine_id, &account, &credentials, &provisioned, &error)
                .await;
            return Err(SandboxMaterializationError::Setup(error));
        }
        let environment_id = id::<EnvironmentId>(Uuid::now_v7().to_string())?;
        let root_id = id::<WorkspaceRootId>(SANDBOX_ROOT_ID.to_owned())?;
        match self
            .database
            .complete_sandbox_materialization(
                &machine_id,
                &environment_id,
                &template.name,
                &root_id,
                &environment_path(&template.cwd),
                template.id,
            )
            .await
        {
            Ok(instance) => Ok(instance),
            Err(error) => {
                let message = error.to_string();
                self.cleanup_recorded(&machine_id, &account, &credentials, &provisioned, &message)
                    .await;
                Err(error.into())
            }
        }
    }

    pub async fn create_e2b_sandbox(
        &self,
        account_id: Uuid,
        template_id: &str,
        name: Option<&str>,
    ) -> Result<CreatedE2bSandboxMachine, SandboxMaterializationError> {
        let account = self
            .accounts
            .resolve(SandboxProvider::E2b, Some(account_id))
            .await?;
        let credentials = self
            .accounts
            .credentials(account.id)
            .await?
            .ok_or(SandboxMaterializationError::AccountCredentialsMissing)?;
        let machine_id = id::<MachineId>(Uuid::now_v7().to_string())?;
        let machine_name = name.map(str::to_owned).unwrap_or_else(|| {
            format!(
                "E2B sandbox {}",
                machine_id.as_str().split('-').next().unwrap_or("new")
            )
        });
        let provisioned = self.provider.create_e2b(template_id, &credentials).await?;
        let record = sandbox_machine_record(
            machine_id.clone(),
            account.id,
            SandboxProvider::E2b,
            provisioned.provider_resource_id.clone(),
            provisioned.connection_config.clone(),
            provisioned.provider_metadata.clone(),
            account.config.clone(),
        );
        let runtime = match build_runtime(
            &record,
            &machine_name,
            credentials.api_key(),
            provisioned.connection_secret.as_deref(),
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                self.cleanup_unrecorded(&account, &credentials, &provisioned)
                    .await;
                return Err(error.into());
            }
        };
        let encrypted_secret = match provisioned
            .connection_secret
            .as_deref()
            .map(|secret| self.secrets.encrypt(&machine_id, secret))
            .transpose()
        {
            Ok(secret) => secret,
            Err(error) => {
                self.cleanup_unrecorded(&account, &credentials, &provisioned)
                    .await;
                return Err(error.into());
            }
        };
        if let Err(error) = self
            .database
            .create_sandbox_machine(NewSandboxMachine {
                machine_id: &machine_id,
                name: &machine_name,
                descriptor: runtime.descriptor(),
                sandbox_account_id: account.id,
                provider_resource_id: &provisioned.provider_resource_id,
                connection_config: &provisioned.connection_config,
                provider_metadata: &provisioned.provider_metadata,
                connection_secret: encrypted_secret,
            })
            .await
        {
            self.cleanup_unrecorded(&account, &credentials, &provisioned)
                .await;
            return Err(error.into());
        }
        let mut machine = match self.database.machine(machine_id.as_str()).await {
            Ok(Some(machine)) => machine.summary,
            Ok(None) => {
                let error = DbError::Contract("created sandbox machine disappeared".to_owned());
                let message = error.to_string();
                self.cleanup_recorded(&machine_id, &account, &credentials, &provisioned, &message)
                    .await;
                return Err(error.into());
            }
            Err(error) => {
                let message = error.to_string();
                self.cleanup_recorded(&machine_id, &account, &credentials, &provisioned, &message)
                    .await;
                return Err(error.into());
            }
        };
        machine.online = true;
        Ok(CreatedE2bSandboxMachine {
            machine,
            sandbox_account_id: account.id,
            sandbox_id: provisioned.provider_resource_id,
            template_id: template_id.to_owned(),
        })
    }

    pub async fn create_e2b_snapshot(
        &self,
        account_id: Uuid,
        sandbox_id: &str,
        name: &str,
    ) -> Result<Snapshot, SandboxMaterializationError> {
        let account = self
            .accounts
            .resolve(SandboxProvider::E2b, Some(account_id))
            .await?;
        if !self
            .database
            .has_sandbox_machine(account.id, sandbox_id)
            .await?
        {
            return Err(SandboxMaterializationError::SandboxNotFound);
        }
        let credentials = self
            .accounts
            .credentials(account.id)
            .await?
            .ok_or(SandboxMaterializationError::AccountCredentialsMissing)?;
        let provider_snapshot_id = self
            .provider
            .create_e2b_snapshot(sandbox_id, &credentials)
            .await?;
        self.database
            .create_snapshot(
                Uuid::now_v7(),
                account.id,
                &CreateSnapshotRequest {
                    name: name.to_owned(),
                    provider: SandboxProvider::E2b,
                    sandbox_account_id: Some(account.id),
                    provider_snapshot_id,
                    sandbox_id: sandbox_id.to_owned(),
                },
            )
            .await
            .map_err(Into::into)
    }

    pub async fn delete_environment(
        &self,
        environment_id: &str,
    ) -> Result<bool, SandboxMaterializationError> {
        let Some(resource) = self
            .database
            .sandbox_environment_resource(environment_id)
            .await?
        else {
            return self
                .database
                .delete_environment(environment_id)
                .await
                .map_err(Into::into);
        };
        self.terminate_resource(&resource).await?;
        self.database
            .delete_materialized_sandbox_environment(&resource)
            .await
            .map_err(Into::into)
    }

    async fn terminate_resource(
        &self,
        resource: &SandboxEnvironmentResource,
    ) -> Result<(), SandboxMaterializationError> {
        let credentials = self
            .accounts
            .credentials(resource.sandbox_account_id)
            .await?
            .ok_or(SandboxMaterializationError::AccountCredentialsMissing)?;
        self.provider
            .terminate(
                resource.provider,
                &resource.account_config,
                &credentials,
                &resource.provider_resource_id,
            )
            .await?;
        Ok(())
    }

    async fn cleanup_unrecorded(
        &self,
        account: &SandboxAccount,
        credentials: &SandboxCredentials,
        provisioned: &ProvisionedSandbox,
    ) {
        if let Err(error) = self
            .provider
            .terminate(
                account.provider,
                &account.config,
                credentials,
                &provisioned.provider_resource_id,
            )
            .await
        {
            tracing::error!(%error, sandbox_id=%provisioned.provider_resource_id, "failed to clean up unrecorded sandbox");
        }
    }

    async fn cleanup_recorded(
        &self,
        machine_id: &MachineId,
        account: &SandboxAccount,
        credentials: &SandboxCredentials,
        provisioned: &ProvisionedSandbox,
        failure: &str,
    ) {
        self.cleanup_unrecorded(account, credentials, provisioned)
            .await;
        if let Err(error) = self
            .database
            .fail_sandbox_materialization(machine_id, failure)
            .await
        {
            tracing::error!(%error, %machine_id, "failed to record sandbox materialization failure");
        }
    }
}

fn materialized_machine_name(template_name: &str, provider_resource_id: &str) -> String {
    let suffix = provider_resource_id.chars().take(6).collect::<String>();
    format!("{template_name} {suffix}")
}

async fn prepare_environment(
    runtime: &dyn ExecutionRuntime,
    cwd: &str,
    creation_script: &str,
) -> Result<(), String> {
    let root_id =
        id::<WorkspaceRootId>(SANDBOX_ROOT_ID.to_owned()).map_err(|error| error.to_string())?;
    let cwd = PathSpec::workspace(root_id, environment_path(cwd));
    let context = OperationContext::with_timeout(Duration::from_secs(600));
    let filesystem = runtime
        .filesystem()
        .ok_or_else(|| "sandbox runtime does not implement filesystem operations".to_owned())?;
    filesystem
        .create_directory(
            &context,
            CreateDirectoryRequest {
                path: cwd.clone(),
                recursive: true,
            },
        )
        .await
        .map_err(|error| format!("could not create template cwd: {error:?}"))?;
    if creation_script.is_empty() {
        return Ok(());
    }
    let processes = runtime
        .process_runtime()
        .ok_or_else(|| "sandbox runtime does not implement process execution".to_owned())?;
    let execution_id =
        id::<ExecutionId>(Uuid::now_v7().to_string()).map_err(|error| error.to_string())?;
    processes
        .start(
            &context,
            StartExecutionRequest {
                operation_id: id::<OperationId>(Uuid::now_v7().to_string())
                    .map_err(|error| error.to_string())?,
                execution_id: execution_id.clone(),
                command: CommandSpec::Shell {
                    command: creation_script.to_owned(),
                    shell: Some("/bin/bash".to_owned()),
                    login: true,
                },
                cwd,
                environment: Default::default(),
                stdin: StdinMode::Closed,
                timeout_ms: Some(600_000),
                persistence: ExecutionPersistence::KeepUntilExit,
                policy: ExecutionPolicy {
                    sandbox: SandboxMode::Disabled,
                    network: NetworkMode::Inherit,
                    profile: None,
                    resource_limits: None,
                },
                output: ProcessOutputPolicy {
                    persist_full_output: true,
                    max_inline_bytes: 256 * 1024,
                    max_chunk_bytes: 64 * 1024,
                },
            },
        )
        .await
        .map_err(|error| format!("could not start creation script: {error:?}"))?;
    loop {
        let status = processes
            .inspect(
                &context,
                execution_contracts::InspectExecutionRequest {
                    execution_id: execution_id.clone(),
                },
            )
            .await
            .map_err(|error| format!("could not inspect creation script: {error:?}"))?;
        match status.state {
            ExecutionState::Exited if status.exit_code == Some(0) => return Ok(()),
            ExecutionState::Exited
            | ExecutionState::Failed
            | ExecutionState::Cancelled
            | ExecutionState::Lost => {
                return Err(format!(
                    "creation script ended in state {:?} with exit code {:?}",
                    status.state, status.exit_code
                ));
            }
            ExecutionState::Queued | ExecutionState::Starting | ExecutionState::Running => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

fn id<T>(value: String) -> Result<T, SandboxMaterializationError>
where
    T: TryFrom<String>,
    T::Error: std::fmt::Display,
{
    T::try_from(value).map_err(|error| SandboxMaterializationError::Identifier(error.to_string()))
}

#[derive(Debug, Error)]
pub enum SandboxMaterializationError {
    #[error("sandbox environment template was not found")]
    TemplateNotFound,
    #[error("template snapshot was not found")]
    SnapshotNotFound,
    #[error("sandbox was not found")]
    SandboxNotFound,
    #[error("sandbox account credentials are missing")]
    AccountCredentialsMissing,
    #[error("sandbox setup failed: {0}")]
    Setup(String),
    #[error("could not construct internal identifier: {0}")]
    Identifier(String),
    #[error(transparent)]
    Database(#[from] DbError),
    #[error(transparent)]
    Account(#[from] SandboxAccountError),
    #[error(transparent)]
    Secret(#[from] SandboxSecretError),
    #[error(transparent)]
    Vault(#[from] credential_vault::VaultError),
    #[error(transparent)]
    Provider(#[from] SandboxProviderError),
    #[error(transparent)]
    Runtime(#[from] SandboxRuntimeError),
}

#[cfg(test)]
mod tests {
    use super::materialized_machine_name;

    #[test]
    fn materialized_machine_names_include_a_stable_resource_suffix() {
        assert_eq!(
            materialized_machine_name("first", "i7xmksh4p0urbaafupl5q"),
            "first i7xmks"
        );
    }
}
