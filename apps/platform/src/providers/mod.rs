pub mod chatgpt_oauth;
mod http;
pub mod model;

use uuid::Uuid;

use crate::upstream::llm_gateway::{LlmGatewayClient, LlmGatewayError};
use model::{
    CreateProviderRequest, Provider, ProviderAccountSummary, ProviderRequestPage,
    ProviderRequestsQuery, ProviderUsageSummary, RotateCredentialsRequest, UpdateProviderRequest,
};

#[derive(Clone)]
pub struct ProviderService {
    gateway: LlmGatewayClient,
}

impl ProviderService {
    #[must_use]
    pub const fn new(gateway: LlmGatewayClient) -> Self {
        Self { gateway }
    }

    async fn list_accounts(&self) -> Result<Vec<ProviderAccountSummary>, LlmGatewayError> {
        let mut accounts: Vec<_> = self
            .gateway
            .list_providers(None)
            .await?
            .accounts
            .into_iter()
            .map(ProviderAccountSummary::from)
            .collect();
        sort_accounts(&mut accounts);
        Ok(accounts)
    }

    async fn get(&self, provider_id: Uuid) -> Result<Provider, LlmGatewayError> {
        Ok(self.gateway.get_provider(provider_id).await?.account)
    }

    async fn usage(&self, provider_id: Uuid) -> Result<ProviderUsageSummary, LlmGatewayError> {
        self.gateway.provider_usage(provider_id).await
    }

    async fn requests(
        &self,
        provider_id: Uuid,
        query: &ProviderRequestsQuery,
    ) -> Result<ProviderRequestPage, LlmGatewayError> {
        self.gateway.provider_requests(provider_id, query).await
    }

    async fn create(&self, request: &CreateProviderRequest) -> Result<Provider, LlmGatewayError> {
        Ok(self.gateway.create_provider(request).await?.account)
    }

    async fn update(
        &self,
        provider_id: Uuid,
        request: &UpdateProviderRequest,
    ) -> Result<Provider, LlmGatewayError> {
        Ok(self
            .gateway
            .update_provider(provider_id, request)
            .await?
            .account)
    }

    async fn set_default(&self, provider_id: Uuid) -> Result<Provider, LlmGatewayError> {
        Ok(self
            .gateway
            .set_default_provider(provider_id)
            .await?
            .account)
    }

    async fn rotate_credentials(
        &self,
        provider_id: Uuid,
        request: &RotateCredentialsRequest,
    ) -> Result<Provider, LlmGatewayError> {
        Ok(self
            .gateway
            .rotate_credentials(provider_id, request)
            .await?
            .account)
    }

    async fn delete(&self, provider_id: Uuid) -> Result<(), LlmGatewayError> {
        self.gateway.delete_provider(provider_id).await
    }
}

fn sort_accounts(accounts: &mut [ProviderAccountSummary]) {
    accounts.sort_by(|left, right| {
        left.provider
            .as_str()
            .cmp(right.provider.as_str())
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| left.id.cmp(&right.id))
    });
}

pub(crate) fn router(
    service: ProviderService,
    chatgpt_login: chatgpt_oauth::ChatGptLoginService,
) -> axum::Router<crate::AppState> {
    http::router(service, chatgpt_login)
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use uuid::Uuid;

    use super::{
        model::{ProviderAccountStatus, ProviderAccountSummary, ProviderKind},
        sort_accounts,
    };

    #[test]
    fn accounts_are_sorted_by_provider_then_newest_creation_date() {
        let mut accounts = vec![
            account(
                "00000000-0000-0000-0000-000000000001",
                ProviderKind::Openai,
                "2026-08-20T00:00:00Z",
            ),
            account(
                "00000000-0000-0000-0000-000000000002",
                ProviderKind::Anthropic,
                "2026-08-19T00:00:00Z",
            ),
            account(
                "00000000-0000-0000-0000-000000000003",
                ProviderKind::Openai,
                "2026-08-22T00:00:00Z",
            ),
        ];

        sort_accounts(&mut accounts);

        assert_eq!(accounts[0].provider, ProviderKind::Anthropic);
        assert_eq!(
            accounts[1].id,
            Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap()
        );
        assert_eq!(
            accounts[2].id,
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
        );
    }

    fn account(id: &str, provider: ProviderKind, created_at: &str) -> ProviderAccountSummary {
        ProviderAccountSummary {
            id: Uuid::parse_str(id).unwrap(),
            name: "account".to_owned(),
            provider,
            status: ProviderAccountStatus::Enabled,
            created_at: created_at.parse::<DateTime<Utc>>().unwrap(),
            is_default: false,
        }
    }
}
