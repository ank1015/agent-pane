use platform_runtime_contracts::*;
use serde_json::json;

#[test]
fn harness_ids_share_one_ascii_grammar() {
    for id in ["a", "pi", "codex-v2", "test_harness-09", &"a".repeat(128)] {
        assert!(is_valid_harness_id(id), "expected valid: {id:?}");
    }
    for id in [
        "",
        "A",
        "aB",
        "0abc",
        "_abc",
        "-abc",
        "a.b",
        "a/b",
        "../a",
        "a%2fb",
        "a?b",
        "a#b",
        " a",
        "a ",
        "a\n",
        "a\0",
        "é",
        "aé",
        &"a".repeat(129),
    ] {
        assert!(!is_valid_harness_id(id), "expected invalid: {id:?}");
    }
}

#[test]
fn conflict_codes_have_one_stable_wire_spelling() {
    for code in [
        ConflictCode::LeaseLost,
        ConflictCode::RunVersionConflict,
        ConflictCode::SessionRevisionConflict,
        ConflictCode::CheckpointVersionConflict,
        ConflictCode::IdempotencyKeyConflict,
        ConflictCode::WorkerIdentityConflict,
        ConflictCode::WorkerStateConflict,
        ConflictCode::InputStateConflict,
        ConflictCode::WaitStateConflict,
        ConflictCode::RunStateConflict,
        ConflictCode::SessionStateConflict,
        ConflictCode::HarnessDisabled,
        ConflictCode::RuntimeConstraintConflict,
        ConflictCode::RuntimeConflict,
    ] {
        assert_eq!(serde_json::to_value(code).unwrap(), json!(code.as_str()));
        let error: ErrorEnvelope = serde_json::from_value(
            json!({"error":{"code":code.as_str(),"message":"arbitrary wording"}}),
        )
        .unwrap();
        assert_eq!(error.error.conflict(), Some(code));
    }
    let error: ErrorEnvelope =
        serde_json::from_value(json!({"error":{"code":"FUTURE_CODE","message":"future"}})).unwrap();
    assert_eq!(error.error.code, "FUTURE_CODE");
    assert_eq!(error.error.conflict(), None);
}

#[test]
fn requests_keep_defaults_and_reject_unknown_fields() {
    let commit: Commit =
        serde_json::from_value(json!({"expected_run_version":2,"expected_session_revision":0}))
            .unwrap();
    assert!(matches!(commit.disposition, Disposition::Running));
    assert!(commit.messages.is_empty());
    assert!(commit.checkpoint.is_none());
    assert!(
        serde_json::from_value::<Commit>(
            json!({"expected_run_version":2,"expected_session_revision":0,"unknown":true})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<Claim>(json!({"limit":1,"unknown":true})).is_err());
}
