use llm_contracts::{Message, Validate, ValidationError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::validation::{append_nested, finish, issue};

/// A caller-identified message to append to the canonical session transcript.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NewRunMessage {
    pub session_message_id: Uuid,
    pub message: Message,
}

impl Validate for NewRunMessage {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        if self.session_message_id.is_nil() {
            issue(&mut issues, "session_message_id", "must not be nil");
        }
        if let Err(error) = self.message.validate() {
            append_nested(&mut issues, "message", error);
        }
        finish(issues)
    }
}
