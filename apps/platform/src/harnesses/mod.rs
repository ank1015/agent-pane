mod http;
pub mod model;

use std::collections::HashSet;

use crate::{
    providers::model::Provider,
    upstream::{
        agent::{AgentClient, AgentError},
        llm_gateway::{LlmGatewayClient, LlmGatewayError},
    },
};
use model::{AgentHarness, HarnessModelOptions, HarnessProviderModelOptions, HarnessSummary};

#[derive(Clone)]
pub struct HarnessService {
    agent: AgentClient,
    gateway: LlmGatewayClient,
}

impl HarnessService {
    #[must_use]
    pub const fn new(agent: AgentClient, gateway: LlmGatewayClient) -> Self {
        Self { agent, gateway }
    }

    async fn list(&self) -> Result<Vec<HarnessSummary>, AgentError> {
        let mut harnesses = Vec::new();
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();

        loop {
            let page = self.agent.list_harnesses(cursor.as_deref()).await?;
            harnesses.extend(page.items.into_iter().map(HarnessSummary::from));

            match page.next_cursor {
                Some(next_cursor) if seen_cursors.insert(next_cursor.clone()) => {
                    cursor = Some(next_cursor);
                }
                Some(_) => return Err(AgentError::InvalidPagination),
                None => break,
            }
        }

        Ok(harnesses)
    }

    async fn model_options(
        &self,
        harness_id: &str,
    ) -> Result<HarnessModelOptions, HarnessModelOptionsError> {
        let (harness, accounts) = tokio::join!(
            self.agent.get_harness(harness_id),
            self.gateway.list_providers(None)
        );
        Ok(build_model_options(harness?, accounts?.accounts))
    }
}

fn build_model_options(harness: AgentHarness, accounts: Vec<Provider>) -> HarnessModelOptions {
    let mut providers = Vec::new();
    for supported_provider in harness.supported_providers {
        providers.extend(
            accounts
                .iter()
                .filter(|account| {
                    account.enabled && account.provider.as_str() == supported_provider.provider_id
                })
                .map(|account| HarnessProviderModelOptions {
                    account_id: account.id,
                    name: account.name.clone(),
                    provider: account.provider,
                    model_ids: supported_provider.model_ids.clone(),
                }),
        );
    }

    HarnessModelOptions {
        providers,
        reasoning_levels: harness.supported_reasoning_levels,
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum HarnessModelOptionsError {
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error(transparent)]
    LlmGateway(#[from] LlmGatewayError),
}

pub(crate) fn router(service: HarnessService) -> axum::Router<crate::AppState> {
    http::router(service)
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use serde_json::json;
    use uuid::Uuid;

    use super::{build_model_options, model::AgentHarness};
    use crate::{
        harnesses::model::AgentHarnessProvider,
        providers::model::{CredentialMetadata, Provider, ProviderKind},
    };

    #[test]
    fn model_options_repeat_models_for_each_enabled_matching_account() {
        let enabled_openai = account(
            "00000000-0000-0000-0000-000000000001",
            ProviderKind::Openai,
            "Personal",
            true,
        );
        let second_openai = account(
            "00000000-0000-0000-0000-000000000002",
            ProviderKind::Openai,
            "Work",
            true,
        );
        let disabled_openai = account(
            "00000000-0000-0000-0000-000000000003",
            ProviderKind::Openai,
            "Disabled",
            false,
        );
        let unsupported_provider = account(
            "00000000-0000-0000-0000-000000000004",
            ProviderKind::Anthropic,
            "Anthropic",
            true,
        );

        let options = build_model_options(
            harness(),
            vec![
                enabled_openai,
                second_openai,
                disabled_openai,
                unsupported_provider,
            ],
        );

        assert_eq!(options.providers.len(), 2);
        assert_eq!(options.providers[0].name, "Personal");
        assert_eq!(options.providers[1].name, "Work");
        assert_eq!(
            options.providers[0].model_ids,
            ["gpt-5.6-sol", "gpt-5.6-terra"]
        );
        assert_eq!(
            options.reasoning_levels,
            ["low", "medium", "high", "xhigh", "max"]
        );
    }

    fn harness() -> AgentHarness {
        AgentHarness {
            harness_id: "environment".to_owned(),
            display_name: "Environment Harness".to_owned(),
            description: None,
            supported_providers: vec![AgentHarnessProvider {
                provider_id: "openai".to_owned(),
                model_ids: vec!["gpt-5.6-sol".to_owned(), "gpt-5.6-terra".to_owned()],
            }],
            supported_reasoning_levels: ["low", "medium", "high", "xhigh", "max"]
                .map(str::to_owned)
                .to_vec(),
            created_at: timestamp(),
            updated_at: timestamp(),
        }
    }

    fn account(id: &str, provider: ProviderKind, name: &str, enabled: bool) -> Provider {
        Provider {
            id: Uuid::parse_str(id).unwrap(),
            provider,
            name: name.to_owned(),
            config: json!({}),
            enabled,
            is_default: false,
            created_at: timestamp(),
            updated_at: timestamp(),
            credential: CredentialMetadata {
                version: 1,
                encryption_key_version: 1,
                updated_at: timestamp(),
            },
        }
    }

    fn timestamp() -> DateTime<Utc> {
        "2026-08-31T00:00:00Z".parse().unwrap()
    }
}
