/// ChatGPT uses the exact same generated model entries as `provider-openai`.
pub type ChatGptModel = provider_openai::OpenAiModel;

/// Complete allowlist of models accepted by this provider.
pub const CHATGPT_MODELS: &[ChatGptModel] = provider_openai::OPENAI_MODELS;

/// Finds an allowed model by its exact provider-owned identifier.
#[must_use]
pub fn find_model(model_id: &str) -> Option<&'static ChatGptModel> {
    CHATGPT_MODELS.iter().find(|model| model.id == model_id)
}
