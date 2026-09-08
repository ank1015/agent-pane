use platform_runtime_contracts::*;
use serde_json::{Value, json};
use uuid::Uuid;

#[test]
fn declarations_resolve_nested_and_escaped_pointers_without_rewriting_config() {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let contract: HarnessContract = serde_json::from_value(json!({"environment_inputs":[
        {"config_pointer":"/trainer/environment","cardinality":"single","required":true},
        {"config_pointer":"/eval~1workers","cardinality":"multiple"}
    ]}))
    .unwrap();
    let config = json!({"trainer":{"environment":a},"eval/workers":[b,a]});
    let mut expected = vec![a, b];
    expected.sort();
    assert_eq!(contract.environment_ids(&config).unwrap(), expected);
    assert!(contract.environment_ids(&json!({})).is_err());
    assert!(
        contract
            .environment_ids(&json!({"trainer":{"environment":a},"eval/workers":null}))
            .is_err()
    );
    assert_eq!(
        HarnessContract::default()
            .environment_ids(&json!({"unrelated":"not-an-id"}))
            .unwrap(),
        Vec::<Uuid>::new()
    );
    for pointer in ["", "plain", "/bad~", "/bad~2"] {
        let c: HarnessContract = serde_json::from_value(
            json!({"environment_inputs":[{"config_pointer":pointer,"cardinality":"single"}]}),
        )
        .unwrap();
        assert!(c.validate().is_err());
    }
}

#[test]
fn output_validation_rejects_scope_overrides_and_unsafe_reference_shapes() {
    for body in [
        json!({"name":"a","output":{"kind":"json","value":1},"run_id":Uuid::new_v4()}),
        json!({"name":"a","output":{"kind":"execution_workspace","value":{"host_id":Uuid::new_v4(),"workspace_root":"/work","path":".","token":"secret"}}}),
        json!({"name":"a","output":{"kind":"other","value":1}}),
    ] {
        assert!(serde_json::from_value::<PublishRunOutput>(body).is_err());
    }
    for path in ["../work", "/absolute", "a/../b", r"C:\work", "a//b", ""] {
        let output = PublishRunOutput {
            name: "workspace".into(),
            output: OutputValue::ExecutionWorkspace(ExecutionWorkspace {
                host_id: Uuid::new_v4(),
                workspace_root: "/work".into(),
                path: path.into(),
                environment_id: None,
                sandbox_id: None,
            }),
        };
        assert!(output.validate().is_err(), "{path}");
    }
    let too_big = PublishRunOutput {
        name: "report".into(),
        output: OutputValue::Json(json!("x".repeat(RUN_OUTPUT_MAX_BYTES))),
    };
    assert!(too_big.validate().is_err());
    for value in [Value::Null, json!([1, 2]), json!({"a":true})] {
        let request = PublishRunOutput {
            name: "report".into(),
            output: OutputValue::Json(value),
        };
        request.validate().unwrap();
        assert_eq!(
            serde_json::from_value::<PublishRunOutput>(json!(request)).unwrap(),
            request
        );
    }
    let artifact = ArtifactReference {
        artifact_id: Uuid::new_v4(),
        sha256: "A".repeat(64),
        size_bytes: 0,
        media_type: "text/plain".into(),
    };
    assert!(artifact.validate().is_err());
}

#[test]
fn capability_arguments_share_camel_case_and_reject_extra_scope() {
    let options: capabilities::OutputOptions =
        serde_json::from_value(json!({"afterSequence":4,"limit":2})).unwrap();
    let query: RunOutputsQuery = options.into();
    assert_eq!(query.after_sequence, Some(4));
    assert_eq!(query.limit, Some(2));
    assert!(
        serde_json::from_value::<capabilities::CreateSandboxFromSnapshot>(
            json!({"environmentId":Uuid::new_v4(),"projectId":Uuid::new_v4()})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<capabilities::AbortRun>(
            json!({"runId":Uuid::new_v4(),"config":{}})
        )
        .is_err()
    );
}
