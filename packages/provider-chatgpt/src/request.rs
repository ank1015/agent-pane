use llm_contracts::{LlmError, LlmRequest, Message, ProviderId, Validate};
use serde_json::{Value, json};

use crate::{
    CHATGPT_PROVIDER, DEFAULT_CHATGPT_INSTRUCTIONS,
    error::{invalid_model, invalid_request},
    find_model,
};

/// Tag for custom messages containing native ChatGPT Responses input items.
pub const CHATGPT_NATIVE_INPUT_TAG: &str = "chatgpt.native_input";

/// Builds the streaming wire request consumed internally by this provider.
///
/// The package still exposes a non-streaming `LlmTransport`; these fields are
/// required by the ChatGPT Codex backend and cannot be overridden through
/// `provider_options`.
pub fn build_response_request(request: &LlmRequest) -> Result<Value, LlmError> {
    request
        .validate()
        .map_err(|error| invalid_request(error.to_string()))?;
    if request.model.provider.as_str() != CHATGPT_PROVIDER {
        return Err(invalid_request(format!(
            "ChatGPT provider cannot handle model provider `{}`.",
            request.model.provider
        )));
    }
    if find_model(request.model.id.as_str()).is_none() {
        return Err(invalid_model(request.model.id.as_str()));
    }
    if request.provider_options.contains_key("max_output_tokens") {
        return Err(invalid_request(
            "ChatGPT backend does not support provider_options.max_output_tokens.",
        ));
    }

    // The portable Responses mapping is shared with provider-openai. Adapt
    // same-provider native messages so encrypted reasoning and tool-call items
    // are replayed verbatim instead of reconstructed from normalized content.
    let mut portable = request.clone();
    portable.model.provider = ProviderId::new(provider_openai::OPENAI_PROVIDER)
        .expect("the built-in OpenAI provider id is valid");
    for message in &mut portable.messages {
        match message {
            Message::Assistant(message) if message.model.provider.as_str() == CHATGPT_PROVIDER => {
                let output = message
                    .native_message
                    .get("output")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| {
                        invalid_request(
                            "ChatGPT assistant native_message must contain an output array.",
                        )
                    })?;
                message.model.provider = ProviderId::new(provider_openai::OPENAI_PROVIDER)
                    .expect("the built-in OpenAI provider id is valid");
                message.native_message = json!({
                    "object": "response",
                    "output": output,
                });
            }
            Message::Custom(message)
                if message.tag.as_deref() == Some(CHATGPT_NATIVE_INPUT_TAG) =>
            {
                message.tag = Some(provider_openai::OPENAI_NATIVE_INPUT_TAG.to_owned());
            }
            _ => {}
        }
    }

    let mut body = provider_openai::build_response_request(&portable).map_err(|mut error| {
        error.message = error.message.replace("OpenAI", "ChatGPT");
        error
    })?;
    let object = body
        .as_object_mut()
        .expect("provider-openai always constructs a JSON object");
    object.insert("store".into(), Value::Bool(false));
    object.insert("stream".into(), Value::Bool(true));
    object.insert("include".into(), json!(["reasoning.encrypted_content"]));
    object.insert(
        "instructions".into(),
        Value::String(
            request
                .instructions
                .clone()
                .unwrap_or_else(|| DEFAULT_CHATGPT_INSTRUCTIONS.to_owned()),
        ),
    );
    Ok(body)
}
