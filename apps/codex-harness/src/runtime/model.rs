use std::fmt;

use provider_openai::find_model;
use serde::{Deserialize, Serialize};

pub const SUPPORTED_MODEL_IDS: &[&str] = &["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"];

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum CodexModel {
    #[serde(rename = "gpt-5.6-sol")]
    Sol,
    #[serde(rename = "gpt-5.6-terra")]
    Terra,
    #[serde(rename = "gpt-5.6-luna")]
    Luna,
}

impl CodexModel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sol => "gpt-5.6-sol",
            Self::Terra => "gpt-5.6-terra",
            Self::Luna => "gpt-5.6-luna",
        }
    }

    #[must_use]
    pub fn profile(self) -> ModelProfile {
        let model = find_model(self.as_str()).expect("supported Codex model is in OpenAI catalog");
        debug_assert!(provider_chatgpt::find_model(self.as_str()).is_some());
        ModelProfile {
            model: self,
            name: model.name,
            // Codex deliberately starts these models below the provider's
            // physical limit and permits a larger configured ceiling.
            context_window: 272_000,
            max_context_window: 872_000,
            max_output_tokens: model.max_tokens,
            code_mode: true,
        }
    }
}

impl fmt::Display for CodexModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelProfile {
    pub model: CodexModel,
    pub name: &'static str,
    pub context_window: u64,
    pub max_context_window: u64,
    pub max_output_tokens: u64,
    pub code_mode: bool,
}

#[cfg(test)]
mod tests {
    use super::{CodexModel, SUPPORTED_MODEL_IDS};

    #[test]
    fn all_three_models_have_codex_profiles_and_code_mode() {
        let models = [CodexModel::Sol, CodexModel::Terra, CodexModel::Luna];
        assert_eq!(
            models.map(CodexModel::as_str).as_slice(),
            SUPPORTED_MODEL_IDS
        );
        for model in models {
            let profile = model.profile();
            assert_eq!(profile.model, model);
            assert_eq!(profile.context_window, 272_000);
            assert_eq!(profile.max_context_window, 872_000);
            assert_eq!(profile.max_output_tokens, 128_000);
            assert!(profile.code_mode);
        }
    }
}
