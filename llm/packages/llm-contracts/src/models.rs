use serde::{Deserialize, Serialize};

use crate::{
    ModelId, ProviderId, Validate, ValidationError,
    validation::{finish, issue, require_non_negative_finite},
};

/// A reference to a model owned by a provider.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    pub provider: ProviderId,
    pub id: ModelId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Validate for ModelRef {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if self
            .name
            .as_ref()
            .is_some_and(|name| name.trim().is_empty())
        {
            issue(&mut issues, "model.name", "must not be empty when set");
        }
        finish(issues)
    }
}

/// Per-million-token prices in USD.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl Validate for ModelCost {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        require_non_negative_finite(&mut issues, "cost.input", self.input);
        require_non_negative_finite(&mut issues, "cost.output", self.output);
        require_non_negative_finite(&mut issues, "cost.cache_read", self.cache_read);
        require_non_negative_finite(&mut issues, "cost.cache_write", self.cache_write);
        finish(issues)
    }
}

/// Long-context pricing applied above a prompt-token threshold.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPricingAbove {
    pub prompt_tokens: u64,
    pub cost: ModelCost,
}

/// Model pricing with an optional long-context override.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPricing {
    pub base: ModelCost,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub above: Option<ModelPricingAbove>,
}

impl Validate for ModelPricing {
    fn validate(&self) -> Result<(), ValidationError> {
        self.base.validate()?;
        if let Some(above) = self.above {
            above.cost.validate()?;
        }
        Ok(())
    }
}
