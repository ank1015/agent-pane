use environments_harness::{Config, config_schema};
use serde_json::{Value, json};
use uuid::Uuid;

#[test]
fn registered_capabilities_match_harness_catalogs() {
    let models = environments_harness::supported_models();
    let migration = include_str!(
        "../../../../apps/server/migrations/20260905030000_harness_supported_models.sql"
    );
    let stored = migration
        .split("supported_models = '")
        .nth(1)
        .unwrap()
        .split("'::jsonb")
        .next()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(stored).unwrap(),
        serde_json::to_value(&models).unwrap()
    );
    assert_eq!(models.len(), 3);
    for (provider, ids) in models {
        assert!(!ids.is_empty());
        for id in ids {
            for effort in ["low", "medium", "high", "xhigh", "max"] {
                Config::parse(value(&provider, &id, effort).as_object().unwrap()).unwrap();
            }
        }
    }
}

fn value(provider: &str, model: &str, effort: &str) -> Value {
    json!({"model":{"provider":provider,"id":model},"reasoning_level":effort})
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
    config["project_id"] = json!(Uuid::new_v4());
    assert!(Config::parse(config.as_object().unwrap()).is_err());
}
#[test]
fn schemas_have_only_host_targeting_and_valid_environment_variants() {
    use environments_harness::tool_definitions;
    use llm_contracts::{ToolDefinition, Validate};
    let tools = tool_definitions(true);
    assert_eq!(tools.len(), 12);
    assert_eq!(tool_definitions(false).len(), 10);
    for definition in tools {
        definition.validate().unwrap();
        let ToolDefinition::Function(tool) = definition else {
            unreachable!()
        };
        let schema = Value::Object(tool.parameters.clone());
        let validator = jsonschema::validator_for(&schema).unwrap();
        if matches!(tool.name.as_str(), "read" | "write" | "edit" | "bash") {
            assert!(
                schema["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("host_id"))
            );
            assert!(schema["properties"].get("workspace_root").is_none());
        }
        if tool.name == "create_environment" {
            let mut args = json!({"name":"Test","type":"machine","machine_id":Uuid::new_v4(),"workspace_root":"/work","path":"."});
            assert!(validator.is_valid(&args));
            args["snapshot_id"] = json!(Uuid::new_v4());
            assert!(!validator.is_valid(&args));
            args["type"] = json!("sandbox");
            args.as_object_mut().unwrap().remove("machine_id");
            assert!(validator.is_valid(&args));
        }
        if tool.name == "create_sandbox" {
            for ram in [1024, 2048, 4096, 8192] {
                assert!(
                    validator.is_valid(
                        &json!({"source":{"type":"base","ram":ram},"network_access":false})
                    )
                );
            }
            for ram in [json!(512), json!(16384), json!("2048"), json!(2048.5)] {
                assert!(!validator.is_valid(&json!({"source":{"type":"base","ram":ram}})));
            }
            assert!(validator.is_valid(&json!({"source":{"type":"snapshot","snapshot_id":Uuid::new_v4()},"network_access":false})));
            assert!(!validator.is_valid(
                &json!({"source":{"type":"snapshot","snapshot_id":Uuid::new_v4(),"ram":2048}})
            ));
            assert!(
                !validator.is_valid(&json!({"source":{"type":"base"},"network_access":"false"}))
            );
            assert!(validator.is_valid(&json!({"source":{"type":"base"}})));
            assert!(
                validator
                    .is_valid(&json!({"source":{"type":"snapshot","snapshot_id":Uuid::new_v4()}}))
            );
            assert!(!validator.is_valid(&json!({"source":{"type":"snapshot","snapshot_id":Uuid::new_v4(),"e2b_account_id":Uuid::new_v4()}})));
        }
    }
    let config = Config::parse(
        value("openai", "gpt-5.6-terra", "high")
            .as_object()
            .unwrap(),
    )
    .unwrap();
    assert!(config.provider_options(Uuid::new_v4()).is_ok());
    let mut config = value("openai", "gpt-5.6-terra", "high");
    config["web_search_enabled"] = json!(false);
    assert!(Config::parse(config.as_object().unwrap()).is_err());
}
