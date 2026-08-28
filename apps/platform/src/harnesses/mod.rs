mod http;
pub mod model;

use std::collections::HashSet;

use crate::upstream::agent::{AgentClient, AgentError};
use model::HarnessSummary;

#[derive(Clone)]
pub struct HarnessService {
    agent: AgentClient,
}

impl HarnessService {
    #[must_use]
    pub const fn new(agent: AgentClient) -> Self {
        Self { agent }
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
}

pub(crate) fn router(service: HarnessService) -> axum::Router<crate::AppState> {
    http::router(service)
}
