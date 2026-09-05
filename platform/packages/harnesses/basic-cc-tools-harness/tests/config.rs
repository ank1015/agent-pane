use basic_cc_tools_harness::{Config, config_schema};
use serde_json::{Value, json};
use uuid::Uuid;

fn value(provider: &str, model: &str, effort: &str) -> Value {
    json!({"model":{"provider":provider,"id":model},"reasoning_level":effort,
        "execution":{"host_id":Uuid::new_v4().to_string(),"workspace_root":"work","path":"."}})
}

#[test]
fn every_gateway_catalog_model_and_reasoning_level_resolves() {
    for provider in ["openai", "chatgpt", "fireworks"] {
        let models: Vec<_> = match provider {
            "openai" => provider_openai::OPENAI_MODELS
                .iter()
                .map(|model| model.id)
                .collect(),
            "chatgpt" => provider_chatgpt::CHATGPT_MODELS
                .iter()
                .map(|model| model.id)
                .collect(),
            _ => provider_fireworks::FIREWORKS_MODELS
                .iter()
                .map(|model| model.id)
                .collect(),
        };
        for model in models {
            for effort in ["low", "medium", "high", "xhigh", "max"] {
                let config =
                    Config::parse(value(provider, model, effort).as_object().unwrap()).unwrap();
                let session = Uuid::new_v4();
                let options = config.provider_options(session).unwrap();
                assert_eq!(options["prompt_cache_key"], session.to_string());
                match provider {
                    "openai" => {
                        assert!(options.contains_key("max_output_tokens"));
                        assert_eq!(options["reasoning"]["effort"], effort);
                    }
                    "chatgpt" => {
                        assert!(!options.contains_key("max_output_tokens"));
                        assert!(!options.contains_key("prompt_cache_options"));
                    }
                    _ => assert_eq!(
                        options["reasoning_effort"],
                        provider_fireworks::reasoning_effort(model, effort).unwrap()
                    ),
                }
            }
        }
    }
}

#[test]
fn validates_config_and_rejects_raw_provider_options() {
    let mut config = value("openai", "gpt-5.6-terra", "high");
    assert!(
        jsonschema::validator_for(&config_schema())
            .unwrap()
            .is_valid(&config)
    );
    config["provider_options"] = json!({});
    assert!(Config::parse(config.as_object().unwrap()).is_err());
    for invalid in [
        value("unknown", "model", "high"),
        value("openai", "unknown", "high"),
        value("openai", "gpt-5.6-terra", "ultra"),
    ] {
        assert!(Config::parse(invalid.as_object().unwrap()).is_err());
    }
    let mut config = value("openai", "gpt-5.6-terra", "high");
    config["execution"]["path"] = json!("../outside");
    assert!(Config::parse(config.as_object().unwrap()).is_err());
}
