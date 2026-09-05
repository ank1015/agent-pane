use super::analytics::{ProviderRequestPage, ProviderUsage, RequestsQuery};
use super::model::{ApiKeyCredentials, CreateProviderInput, GatewayCreateAccount};
use super::{LlmGatewayClient, ProviderAccountSummary, ProviderDetail, error::ProviderError};
use uuid::Uuid;

#[derive(Clone)]
pub struct ProviderService {
    gateway: LlmGatewayClient,
}

impl ProviderService {
    pub fn new(gateway: LlmGatewayClient) -> Self {
        Self { gateway }
    }

    pub async fn create_account(
        &self,
        input: CreateProviderInput,
    ) -> Result<ProviderAccountSummary, ProviderError> {
        input.validate()?;
        let credentials = ApiKeyCredentials {
            api_key: &input.api_key,
        };
        let request = GatewayCreateAccount {
            provider: input.provider,
            name: &input.name,
            credentials: &credentials,
        };
        Ok(self.gateway.create_account(&request).await?.account.into())
    }

    pub async fn list_accounts(&self) -> Result<Vec<ProviderAccountSummary>, ProviderError> {
        let mut accounts: Vec<_> = self
            .gateway
            .list_accounts()
            .await?
            .accounts
            .into_iter()
            .map(ProviderAccountSummary::from)
            .collect();
        // Match the previous providers page: provider alphabetically, newest first,
        // then ID for deterministic ordering when creation timestamps tie.
        accounts.sort_by(|left, right| {
            left.provider
                .cmp(&right.provider)
                .then_with(|| right.created_at.cmp(&left.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(accounts)
    }

    pub async fn get_account(&self, account_id: Uuid) -> Result<ProviderDetail, ProviderError> {
        Ok(self.gateway.get_account(account_id).await?.account.into())
    }

    pub async fn usage(&self, account_id: Uuid) -> Result<ProviderUsage, ProviderError> {
        Ok(ProviderUsage {
            account_id,
            totals: self.gateway.usage(account_id).await?.totals,
        })
    }

    pub async fn requests(
        &self,
        account_id: Uuid,
        query: RequestsQuery,
    ) -> Result<ProviderRequestPage, ProviderError> {
        query.validate()?;
        let page = self.gateway.requests(account_id, &query).await?;
        if page.items.iter().any(|item| item.account_id != account_id) {
            return Err(ProviderError::InvalidResponse);
        }
        Ok(page)
    }
}
