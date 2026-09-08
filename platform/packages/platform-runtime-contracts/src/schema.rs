//! Admission accepts canonical user messages, not arbitrary conversation roles.
//! Reuse the actual LLM UserMessage and ContentPart wire structures for SDK schemas.
#[derive(schemars::JsonSchema)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum UserInput {
    User(llm_contracts::UserMessage),
}
