use basic_cc_tools_harness::{Config, config_schema};
use serde_json::{Value, json};
use uuid::Uuid;

#[test]
fn workspace_paths_translate_to_private_execution_roots() {
    use basic_cc_tools_harness::ExecutionTarget;
    use execution_core::ExecutionHostDescriptor;
    let mut host: ExecutionHostDescriptor = serde_json::from_value(json!({
        "host_id":"test", "supervisor_generation_id":"test", "operating_system":{"type":"windows"},
        "architecture":"test", "path_convention":"windows",
        "roots":[{"id":"one","name":"One","native_path":"C:\\work","read_only":false}, {"id":"two","name":"Two","native_path":"D:\\work","read_only":false}],
        "features":{"pty":false,"process_signals":false,"file_revisions":true}, "limits":{}
    })).unwrap();
    let mut target = ExecutionTarget {
        host_id: host.host_id.clone(),
        workspace_root: "D:\\work".into(),
        path: "repo".into(),
    };
    assert_eq!(target.cwd(&host).unwrap().root_id.as_str(), "two");
    assert_eq!(target.cwd(&host).unwrap().path, "repo");
    target.workspace_root = "two".into();
    assert_eq!(
        target.cwd(&host).unwrap().root_id.as_str(),
        "two",
        "historical target IDs remain readable"
    );
    target.workspace_root = "D:\\work".into();
    host.roots.push(host.roots[1].clone());
    assert!(
        target.cwd(&host).is_err(),
        "ambiguous paths are not guessed"
    );
    target.workspace_root = "C:\\other".into();
    assert!(target.cwd(&host).is_err());
    let mut config = value("openai", "gpt-5.6-terra", "high");
    config["environment"]["workspace_root"] = json!("work");
    assert!(
        !jsonschema::validator_for(&config_schema())
            .unwrap()
            .is_valid(&config)
    );
    assert!(Config::parse(config.as_object().unwrap()).is_ok());
    config["environment"]["workspace_root"] = json!("/work/../other");
    assert!(Config::parse(config.as_object().unwrap()).is_err());
}

#[test]
fn registered_models_match_compiled_capabilities() {
    let initial = include_str!(
        "../../../../apps/server/migrations/20260905040000_basic_harness_supported_models.sql"
    );
    let initial = initial
        .split("supported_models = '")
        .nth(1)
        .unwrap()
        .split("'::jsonb")
        .next()
        .unwrap();
    let mut stored = serde_json::from_str::<Value>(initial).unwrap();
    let migration =
        include_str!("../../../../apps/server/migrations/20260910010000_add_gpt_6_astra.sql");
    let update = migration
        .split("supported_models || '")
        .nth(1)
        .unwrap()
        .split("'::jsonb")
        .next()
        .unwrap();
    for (provider, ids) in serde_json::from_str::<Value>(update)
        .unwrap()
        .as_object()
        .unwrap()
    {
        stored[provider] = ids.clone();
    }
    assert_eq!(
        stored,
        serde_json::to_value(basic_cc_tools_harness::supported_models()).unwrap()
    );
}

fn value(provider: &str, model: &str, effort: &str) -> Value {
    json!({"model":{"provider":provider,"id":model},"reasoning_level":effort,
        "environment":{"type":"machine","machine_id":Uuid::new_v4().to_string(),"workspace_root":"/work","path":"."}})
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
    config["environment"]["path"] = json!("../outside");
    assert!(Config::parse(config.as_object().unwrap()).is_err());
}

#[test]
fn sandbox_descriptors_are_typed_and_legacy_run_targets_remain_readable() {
    let mut config = value("openai", "gpt-5.6-terra", "high");
    config["environment"] =
        json!({"type":"sandbox","snapshot_id":Uuid::new_v4(),"workspace_root":"/work","path":"."});
    assert!(Config::parse(config.as_object().unwrap()).is_ok());
    config["environment"]["machine_id"] = json!(Uuid::new_v4());
    assert!(Config::parse(config.as_object().unwrap()).is_err());
    config.as_object_mut().unwrap().remove("environment");
    config["execution"] = json!({"host_id":Uuid::new_v4(),"workspace_root":"work","path":"."});
    assert!(
        Config::parse(config.as_object().unwrap()).is_ok(),
        "historical snapshots can recover"
    );
    assert!(
        !jsonschema::validator_for(&config_schema())
            .unwrap()
            .is_valid(&config),
        "new sessions require environment descriptors"
    );
}
