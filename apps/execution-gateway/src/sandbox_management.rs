use execution_contracts::MachineId;
use execution_protocol::MachineSummary;
use uuid::Uuid;

use crate::{
    db::DbError,
    sandbox_accounts::{SandboxAccount, SandboxCredentials, SandboxProvider},
    sandbox_machines::NewSandboxMachine,
    sandbox_materialization::{SandboxMaterializationError, SandboxMaterializer},
    sandbox_provider::ProvisionedSandbox,
    sandbox_runtime::{build_runtime, sandbox_machine_record},
    snapshots::{CreateSnapshotRequest, Snapshot},
};

#[derive(Clone, Debug)]
pub struct CreatedSandboxMachine {
    pub machine: MachineSummary,
    pub sandbox_account_id: Uuid,
    pub sandbox_id: String,
    pub created_from: Option<String>,
}

impl SandboxMaterializer {
    pub async fn create_sandbox(
        &self,
        account_id: Uuid,
        source: Option<&str>,
        name: Option<&str>,
    ) -> Result<CreatedSandboxMachine, SandboxMaterializationError> {
        let provider = self
            .accounts
            .find(account_id)
            .await?
            .ok_or(crate::sandbox_accounts::SandboxAccountError::AccountNotFound)?
            .provider;
        let account = self.accounts.resolve(provider, Some(account_id)).await?;
        let credentials = self
            .accounts
            .credentials(account.id)
            .await?
            .ok_or(SandboxMaterializationError::AccountCredentialsMissing)?;
        let machine_id = machine_id()?;
        let machine_name = name
            .map(str::to_owned)
            .unwrap_or_else(|| default_machine_name(provider, &machine_id));
        let target_name = format!("agent-pane-{}", Uuid::now_v7().simple());
        let provisioned = self
            .provider
            .create_sandbox(&account, &credentials, &target_name, source)
            .await?;
        self.record_created_sandbox(
            machine_id,
            machine_name,
            &account,
            &credentials,
            provisioned,
        )
        .await
    }

    pub async fn create_sandbox_from_snapshot(
        &self,
        account_id: Uuid,
        snapshot_id: Uuid,
        name: Option<&str>,
    ) -> Result<CreatedSandboxMachine, SandboxMaterializationError> {
        let snapshot = self
            .database
            .snapshot(snapshot_id)
            .await?
            .ok_or(SandboxMaterializationError::SnapshotNotFound)?;
        if snapshot.sandbox_account_id != account_id {
            return Err(SandboxMaterializationError::SnapshotAccountMismatch);
        }
        let account = self
            .accounts
            .resolve(snapshot.provider, Some(account_id))
            .await?;
        let credentials = self
            .accounts
            .credentials(account.id)
            .await?
            .ok_or(SandboxMaterializationError::AccountCredentialsMissing)?;
        let machine_id = machine_id()?;
        let machine_name = name
            .map(str::to_owned)
            .unwrap_or_else(|| default_machine_name(snapshot.provider, &machine_id));
        let target_name = format!("agent-pane-{}", Uuid::now_v7().simple());
        let provisioned = self
            .provider
            .create_from_snapshot(&snapshot, &account, &credentials, &target_name)
            .await?;
        self.record_created_sandbox(
            machine_id,
            machine_name,
            &account,
            &credentials,
            provisioned,
        )
        .await
    }

    async fn record_created_sandbox(
        &self,
        machine_id: MachineId,
        machine_name: String,
        account: &SandboxAccount,
        credentials: &SandboxCredentials,
        provisioned: ProvisionedSandbox,
    ) -> Result<CreatedSandboxMachine, SandboxMaterializationError> {
        let runtime = self
            .build_created_runtime(
                &machine_id,
                &machine_name,
                account,
                credentials,
                &provisioned,
            )
            .await?;
        let encrypted_secret = match provisioned
            .connection_secret
            .as_deref()
            .map(|secret| self.secrets.encrypt(&machine_id, secret))
            .transpose()
        {
            Ok(secret) => secret,
            Err(error) => {
                self.cleanup_unrecorded(account, credentials, &provisioned)
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
            self.cleanup_unrecorded(account, credentials, &provisioned)
                .await;
            return Err(error.into());
        }
        let mut machine = match self.database.machine(machine_id.as_str()).await {
            Ok(Some(machine)) => machine.summary,
            Ok(None) => {
                let error = DbError::Contract("created sandbox machine disappeared".to_owned());
                self.cleanup_recorded(
                    &machine_id,
                    account,
                    credentials,
                    &provisioned,
                    &error.to_string(),
                )
                .await;
                return Err(error.into());
            }
            Err(error) => {
                self.cleanup_recorded(
                    &machine_id,
                    account,
                    credentials,
                    &provisioned,
                    &error.to_string(),
                )
                .await;
                return Err(error.into());
            }
        };
        machine.online = true;
        Ok(CreatedSandboxMachine {
            machine,
            sandbox_account_id: account.id,
            sandbox_id: provisioned.provider_resource_id,
            created_from: provisioned
                .provider_metadata
                .get("created_from")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        })
    }

    pub async fn create_sandbox_snapshot(
        &self,
        account_id: Uuid,
        sandbox_id: &str,
        name: &str,
    ) -> Result<Snapshot, SandboxMaterializationError> {
        let provider = self
            .accounts
            .find(account_id)
            .await?
            .ok_or(crate::sandbox_accounts::SandboxAccountError::AccountNotFound)?
            .provider;
        let account = self.accounts.resolve(provider, Some(account_id)).await?;
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
        let requested_snapshot_name = format!("agent-pane-{}", Uuid::now_v7().simple());
        let provider_snapshot_id = self
            .provider
            .create_snapshot(&account, sandbox_id, &requested_snapshot_name, &credentials)
            .await?;
        self.database
            .create_snapshot(
                Uuid::now_v7(),
                account.id,
                &CreateSnapshotRequest {
                    name: name.to_owned(),
                    provider,
                    sandbox_account_id: Some(account.id),
                    provider_snapshot_id,
                    sandbox_id: sandbox_id.to_owned(),
                },
            )
            .await
            .map_err(Into::into)
    }

    pub async fn create_sandbox_snapshot_for_machine(
        &self,
        machine_id: &MachineId,
    ) -> Result<Snapshot, SandboxMaterializationError> {
        let sandbox = self
            .database
            .sandbox_machine(machine_id.as_str())
            .await?
            .ok_or(SandboxMaterializationError::SandboxNotFound)?;
        let name = format!("Snapshot of {}", machine_id.as_str());
        self.create_sandbox_snapshot(
            sandbox.sandbox_account_id,
            &sandbox.provider_resource_id,
            &name,
        )
        .await
    }

    async fn build_created_runtime(
        &self,
        machine_id: &MachineId,
        machine_name: &str,
        account: &crate::sandbox_accounts::SandboxAccount,
        credentials: &crate::sandbox_accounts::SandboxCredentials,
        provisioned: &ProvisionedSandbox,
    ) -> Result<std::sync::Arc<dyn execution_runtime::ExecutionRuntime>, SandboxMaterializationError>
    {
        let record = sandbox_machine_record(
            machine_id.clone(),
            account.id,
            account.provider,
            provisioned.provider_resource_id.clone(),
            provisioned.connection_config.clone(),
            provisioned.provider_metadata.clone(),
            account.config.clone(),
        );
        match build_runtime(
            &record,
            machine_name,
            credentials.api_key(),
            provisioned.connection_secret.as_deref(),
        ) {
            Ok(runtime) => Ok(runtime),
            Err(error) => {
                self.cleanup_unrecorded(account, credentials, provisioned)
                    .await;
                Err(error.into())
            }
        }
    }
}

fn machine_id() -> Result<MachineId, SandboxMaterializationError> {
    MachineId::try_from(Uuid::now_v7().to_string())
        .map_err(|error| SandboxMaterializationError::Identifier(error.to_string()))
}

fn default_machine_name(provider: SandboxProvider, machine_id: &MachineId) -> String {
    let provider = match provider {
        SandboxProvider::E2b => "E2B",
        SandboxProvider::Daytona => "Daytona",
        SandboxProvider::Blaxel => "Blaxel",
        SandboxProvider::Tensorlake => "Tensorlake",
    };
    let suffix = machine_id.as_str().split('-').next().unwrap_or("new");
    format!("{provider} sandbox {suffix}")
}

#[cfg(test)]
mod tests {
    use execution_contracts::MachineId;

    use super::*;

    #[test]
    fn default_names_include_provider_and_machine_suffix() {
        let machine_id =
            MachineId::try_from("01992aa0-1234-7000-8000-000000000000".to_owned()).unwrap();
        assert_eq!(
            default_machine_name(SandboxProvider::Daytona, &machine_id),
            "Daytona sandbox 01992aa0"
        );
    }
}
