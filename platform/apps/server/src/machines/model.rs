use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::ApiError;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateMachineInput {
    pub name: String,
}

impl UpdateMachineInput {
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.name.trim().is_empty()
            || self.name.trim() != self.name
            || self.name.chars().count() > 200
            || self.name.chars().any(char::is_control)
        {
            return Err(ApiError::InvalidRequest(
                "Machine name must contain 1–200 characters without surrounding whitespace or control characters.",
            ));
        }
        Ok(())
    }
}

// Deliberately no Debug implementation: this payload contains a credential.
#[derive(Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct CreateE2bAccountInput {
    pub name: String,
    pub api_key: String,
}

impl CreateE2bAccountInput {
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.name.trim().is_empty()
            || self.name.trim() != self.name
            || self.name.chars().count() > 200
            || self.name.chars().any(char::is_control)
        {
            return Err(ApiError::InvalidRequest(
                "Account name must contain 1–200 characters without surrounding whitespace or control characters.",
            ));
        }
        if self.api_key.is_empty()
            || self.api_key.len() > 4096
            || self.api_key.trim() != self.api_key
            || self.api_key.chars().any(char::is_control)
        {
            return Err(ApiError::InvalidRequest(
                "API key must contain 1–4096 bytes without surrounding whitespace or control characters.",
            ));
        }
        Ok(())
    }
}
