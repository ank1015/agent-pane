use execution_core::{
    ExecutionErrorCode, ExecutionHostId, ExecutionPath, ExecutionRuntime, OperationContext,
    OperationId, RootId,
};
use execution_supervisor_core::{
    SupervisorConfig, SupervisorLimits, SupervisorRoot, SupervisorRuntime,
};
use llm_contracts::{GrammarSyntax, ToolArguments, ToolDefinition};
use serde_json::{Map, Value};
use std::time::Duration;
use tool_apply_patch::{
    ApplyPatchConfig, ApplyPatchFileUpdateMode, ApplyPatchState, ApplyPatchTool, DESCRIPTION,
    LARK_GRAMMAR, NAME, PatchProgress, PreparedPatch, RunningPatch, definition, parse_patch,
};

struct Host {
    temp: tempfile::TempDir,
    runtime: SupervisorRuntime,
}

impl Host {
    async fn new() -> Self {
        let temp = tempfile::tempdir().expect("create temporary host");
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(root.join("project")).expect("create project");
        let runtime = SupervisorRuntime::new(SupervisorConfig {
            host_id: ExecutionHostId::generate(),
            state_directory: temp.path().join("state"),
            roots: vec![SupervisorRoot {
                id: RootId::new("work").expect("root ID"),
                name: "Work".into(),
                path: root,
                read_only: false,
            }],
            limits: SupervisorLimits::default(),
        })
        .await
        .expect("start supervisor");
        Self { temp, runtime }
    }

    fn path(&self, path: &str) -> std::path::PathBuf {
        self.temp.path().join("workspace/project").join(path)
    }

    fn write(&self, path: &str, content: impl AsRef<[u8]>) {
        let path = self.path(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture parent");
        }
        std::fs::write(path, content).expect("write fixture");
    }

    fn tool(&self, config: ApplyPatchConfig) -> ApplyPatchTool<'_> {
        ApplyPatchTool::new(
            &self.runtime,
            ExecutionPath::new(RootId::new("work").expect("root ID"), "project").expect("cwd"),
            config,
        )
        .expect("construct tool")
    }
}

fn context() -> OperationContext {
    OperationContext::with_timeout(Duration::from_secs(10))
}

fn state() -> ApplyPatchState {
    ApplyPatchState {
        operation_id: OperationId::generate(),
    }
}

#[test]
fn exports_the_codex_custom_tool_contract_and_parser() {
    let ToolDefinition::Custom(tool) = definition() else {
        panic!("apply_patch must be a custom tool")
    };
    assert_eq!(tool.name, NAME);
    assert_eq!(tool.description, DESCRIPTION);
    assert_eq!(tool.format.syntax, GrammarSyntax::Lark);
    assert_eq!(tool.format.definition, LARK_GRAMMAR);
    assert_eq!(LARK_GRAMMAR, include_str!("fixtures/apply_patch.lark"));
    assert_eq!(
        DESCRIPTION,
        "The `apply_patch` tool can be used to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON."
    );

    let parsed = parse_patch(
        "<<'EOF'\n*** Begin Patch\n*** Add File: hello.txt\n+hello\n*** End Patch\nEOF",
    )
    .expect("parse Codex-compatible heredoc wrapper");
    assert_eq!(
        parsed.patch,
        "*** Begin Patch\n*** Add File: hello.txt\n+hello\n*** End Patch"
    );
    assert_eq!(parsed.hunks.len(), 1);
}

#[tokio::test]
async fn prepares_without_mutating_then_applies_and_replays_all_hunk_types() {
    let host = Host::new().await;
    host.write("update.txt", "before\nkeep\n");
    host.write("delete.txt", "gone\n");
    host.write("overwrite.txt", "old add target\n");
    host.write("move.txt", "old move source\n");
    host.write("moved.txt", "old move destination\n");
    let patch = r#"*** Begin Patch
*** Add File: nested/new.txt
+new
*** Add File: overwrite.txt
+overwritten
*** Update File: update.txt
@@
-before
+after
 keep
*** Delete File: delete.txt
*** Update File: move.txt
*** Move to: moved.txt
@@
-old move source
+moved content
*** End Patch"#;

    let prepared = host
        .tool(ApplyPatchConfig::default())
        .prepare(&context(), patch, state())
        .await
        .expect("prepare patch");
    assert_eq!(prepared.operation_count(), 6);
    assert!(!host.path("nested/new.txt").exists());
    assert_eq!(
        std::fs::read(host.path("update.txt")).unwrap(),
        b"before\nkeep\n"
    );
    assert!(host.path("delete.txt").exists());

    let prepared: PreparedPatch =
        serde_json::from_value(serde_json::to_value(&prepared).expect("serialize prepared patch"))
            .expect("restore prepared patch");
    let tool = host.tool(ApplyPatchConfig::default());
    let mut running = tool.start(&prepared).unwrap();
    let output = tool
        .apply(&context(), &mut running)
        .await
        .expect("apply patch");
    assert_eq!(
        std::fs::read(host.path("nested/new.txt")).unwrap(),
        b"new\n"
    );
    assert_eq!(
        std::fs::read(host.path("overwrite.txt")).unwrap(),
        b"overwritten\n"
    );
    assert_eq!(
        std::fs::read(host.path("update.txt")).unwrap(),
        b"after\nkeep\n"
    );
    assert!(!host.path("delete.txt").exists());
    assert!(!host.path("move.txt").exists());
    assert_eq!(
        std::fs::read(host.path("moved.txt")).unwrap(),
        b"moved content\n"
    );
    assert_eq!(
        output.to_text(),
        "Success. Updated the following files:\nA nested/new.txt\nA overwrite.txt\nM update.txt\nM moved.txt\nD delete.txt\n"
    );
    assert_eq!(output.code_mode_result(), serde_json::json!({}));
    assert_eq!(output.host_id, host.runtime.descriptor().host_id);

    let mut running: RunningPatch =
        serde_json::from_value(serde_json::to_value(running).unwrap()).unwrap();
    let replay = tool
        .apply(&context(), &mut running)
        .await
        .expect("replay prepared patch");
    assert_eq!(replay, output);
}

#[tokio::test]
async fn changed_context_fails_without_overwriting_external_changes() {
    let host = Host::new().await;
    host.write("target.txt", "old\n");
    let prepared = host
        .tool(ApplyPatchConfig::default())
        .prepare(
            &context(),
            "*** Begin Patch\n*** Update File: target.txt\n@@\n-old\n+new\n*** End Patch",
            state(),
        )
        .await
        .expect("prepare update");
    host.write("target.txt", "external\n");

    let tool = host.tool(ApplyPatchConfig::default());
    let failure = tool
        .apply(&context(), &mut tool.start(&prepared).unwrap())
        .await
        .expect_err("stale patch must fail");
    assert_eq!(failure.code, ExecutionErrorCode::InvalidRequest);
    assert_eq!(failure.details["applied_operations"], 0);
    assert_eq!(failure.details["total_operations"], 1);
    assert_eq!(
        std::fs::read(host.path("target.txt")).unwrap(),
        b"external\n"
    );
}

#[tokio::test]
async fn later_failures_report_the_committed_prefix() {
    let host = Host::new().await;
    host.write("first.txt", "old first\n");
    host.write("second.txt", "old second\n");
    let patch = "*** Begin Patch\n*** Update File: first.txt\n@@\n-old first\n+new first\n*** Update File: second.txt\n@@\n-old second\n+new second\n*** End Patch";
    let prepared = host
        .tool(ApplyPatchConfig::default())
        .prepare(&context(), patch, state())
        .await
        .expect("prepare two-file patch");
    host.write("second.txt", "external second\n");

    let tool = host.tool(ApplyPatchConfig::default());
    let failure = tool
        .apply(&context(), &mut tool.start(&prepared).unwrap())
        .await
        .expect_err("second stale write must fail");
    assert_eq!(failure.code, ExecutionErrorCode::InvalidRequest);
    assert_eq!(failure.details["applied_operations"], 1);
    assert_eq!(failure.details["total_operations"], 2);
    assert_eq!(
        std::fs::read(host.path("first.txt")).unwrap(),
        b"new first\n"
    );
    assert_eq!(
        std::fs::read(host.path("second.txt")).unwrap(),
        b"external second\n"
    );
}

#[tokio::test]
async fn preparation_verifies_every_hunk_before_any_mutation() {
    let host = Host::new().await;
    host.write("target.txt", "present\n");
    let patch = "*** Begin Patch\n*** Add File: early.txt\n+would be written\n*** Update File: target.txt\n@@\n-missing\n+replacement\n*** End Patch";
    let failure = host
        .tool(ApplyPatchConfig::default())
        .prepare(&context(), patch, state())
        .await
        .expect_err("missing update context must fail preparation");
    assert_eq!(failure.code, ExecutionErrorCode::InvalidRequest);
    assert!(failure.message.contains("Failed to find expected lines"));
    assert!(!host.path("early.txt").exists());
    assert_eq!(
        std::fs::read(host.path("target.txt")).unwrap(),
        b"present\n"
    );
}

#[tokio::test]
async fn preserves_line_endings_when_configured_and_requires_raw_arguments() {
    let host = Host::new().await;
    host.write("target.txt", b"one\r\ntwo\r\n");
    let config = ApplyPatchConfig {
        update_file_mode: ApplyPatchFileUpdateMode::PreserveLineEndings,
        ..Default::default()
    };
    let patch = "*** Begin Patch\n*** Update File: target.txt\n@@\n-one\n+uno\n*** End Patch";
    let prepared = host
        .tool(config.clone())
        .prepare_arguments(&context(), &ToolArguments::String(patch.into()), state())
        .await
        .expect("prepare raw custom arguments");
    // The saved mode controls replay, even if the caller's defaults change.
    let tool = host.tool(ApplyPatchConfig::default());
    tool.apply(&context(), &mut tool.start(&prepared).unwrap())
        .await
        .expect("apply CRLF patch");
    assert_eq!(
        std::fs::read(host.path("target.txt")).unwrap(),
        b"uno\r\ntwo\r\n"
    );

    let failure = host
        .tool(ApplyPatchConfig::default())
        .prepare_arguments(
            &context(),
            &ToolArguments::Object(Map::<String, Value>::new()),
            state(),
        )
        .await
        .expect_err("JSON arguments must fail");
    assert_eq!(failure.code, ExecutionErrorCode::InvalidRequest);
}

#[cfg(unix)]
#[tokio::test]
async fn deletes_and_moves_leaf_symlinks_without_modifying_their_targets() {
    use std::os::unix::fs::symlink;

    let host = Host::new().await;
    host.write("delete-target.txt", "delete target\n");
    host.write("move-target.txt", "move target\n");
    symlink("delete-target.txt", host.path("delete-link.txt")).expect("delete symlink");
    symlink("move-target.txt", host.path("move-link.txt")).expect("move symlink");
    let patch = "*** Begin Patch\n*** Delete File: delete-link.txt\n*** Update File: move-link.txt\n*** Move to: moved.txt\n@@\n-move target\n+moved copy\n*** End Patch";

    let tool = host.tool(ApplyPatchConfig::default());
    let prepared = tool
        .prepare(&context(), patch, state())
        .await
        .expect("prepare symlink patch");
    tool.apply(&context(), &mut tool.start(&prepared).unwrap())
        .await
        .expect("apply symlink patch");

    assert!(!host.path("delete-link.txt").exists());
    assert!(!host.path("move-link.txt").exists());
    assert_eq!(
        std::fs::read(host.path("delete-target.txt")).unwrap(),
        b"delete target\n"
    );
    assert_eq!(
        std::fs::read(host.path("move-target.txt")).unwrap(),
        b"move target\n"
    );
    assert_eq!(
        std::fs::read(host.path("moved.txt")).unwrap(),
        b"moved copy\n"
    );
}

async fn run(
    host: &Host,
    patch: &str,
) -> execution_core::ExecutionResult<tool_apply_patch::ApplyPatchOutput> {
    let tool = host.tool(ApplyPatchConfig::default());
    let prepared = tool.prepare(&context(), patch, state()).await?;
    tool.apply(&context(), &mut tool.start(&prepared)?).await
}

#[tokio::test]
async fn overlapping_eof_deletions_do_not_panic() {
    let host = Host::new().await;
    host.write("f", "a\nb\nc\n");
    run(
        &host,
        "*** Begin Patch\n*** Update File: f\n@@\n-b\n-c\n@@\n-c\n*** End of File\n*** End Patch",
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(host.path("f")).unwrap(), b"a\n");
}

#[tokio::test]
async fn later_hunks_observe_earlier_move_destinations() {
    let host = Host::new().await;
    for path in ["a", "b", "c"] {
        host.write(path, "old\nkeep\n");
    }
    run(&host, "*** Begin Patch\n*** Update File: a\n*** Move to: b\n@@\n-old\n+new\n*** Update File: b\n@@\n-keep\n+retained\n*** End Patch").await.unwrap();
    assert_eq!(std::fs::read(host.path("b")).unwrap(), b"new\nretained\n");
    assert!(!host.path("a").exists());
    run(&host, "*** Begin Patch\n*** Update File: c\n*** Move to: b\n@@\n-old\n+last\n*** Delete File: b\n*** End Patch").await.unwrap();
    assert!(!host.path("b").exists());
    assert!(!host.path("c").exists());
}

#[tokio::test]
async fn multiple_moves_can_overwrite_one_destination_and_same_path_move_unlinks() {
    let host = Host::new().await;
    host.write("a", "old\n");
    host.write("b", "old\n");
    run(&host, "*** Begin Patch\n*** Update File: a\n*** Move to: dest\n@@\n-old\n+first\n*** Update File: b\n*** Move to: dest\n@@\n-old\n+second\n*** End Patch").await.unwrap();
    assert_eq!(std::fs::read(host.path("dest")).unwrap(), b"second\n");
    run(&host, "*** Begin Patch\n*** Update File: dest\n*** Move to: ./dest\n@@\n-second\n+third\n*** End Patch").await.unwrap();
    assert!(!host.path("dest").exists());
}

#[tokio::test]
async fn updates_reread_unrelated_external_edits() {
    let host = Host::new().await;
    host.write("a", "old\nkeep\n");
    let tool = host.tool(ApplyPatchConfig::default());
    let prepared = tool
        .prepare(
            &context(),
            "*** Begin Patch\n*** Update File: a\n@@\n-old\n+new\n*** End Patch",
            state(),
        )
        .await
        .unwrap();
    host.write("a", "old\nexternal\n");
    tool.apply(&context(), &mut tool.start(&prepared).unwrap())
        .await
        .unwrap();
    assert_eq!(std::fs::read(host.path("a")).unwrap(), b"new\nexternal\n");
}

#[tokio::test]
async fn add_directory_error_occurs_after_earlier_mutations() {
    let host = Host::new().await;
    std::fs::create_dir(host.path("dir")).unwrap();
    let failure = run(&host, "*** Begin Patch\n*** Add File: early\n+created\n*** Add File: dir\n+cannot write\n*** End Patch").await.unwrap_err();
    assert_eq!(failure.code, ExecutionErrorCode::IsDirectory);
    assert_eq!(failure.details["applied_operations"], 1);
    assert_eq!(std::fs::read(host.path("early")).unwrap(), b"created\n");
}

#[tokio::test]
async fn nul_utf8_contents_and_normalized_paths_match_codex() {
    let host = Host::new().await;
    run(
        &host,
        "*** Begin Patch\n*** Add File: missing/../nul\n+a\0b\n*** End Patch",
    )
    .await
    .unwrap();
    run(
        &host,
        "*** Begin Patch\n*** Update File: ../project/nul\n@@\n-a\0b\n+c\0d\n*** End Patch",
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(host.path("nul")).unwrap(), b"c\0d\n");
    run(
        &host,
        "*** Begin Patch\n*** Delete File: nul\n*** End Patch",
    )
    .await
    .unwrap();
    assert!(!host.path("nul").exists());
    let failure = run(
        &host,
        "*** Begin Patch\n*** Add File: ../../escape\n+x\n*** End Patch",
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, ExecutionErrorCode::PathOutsideRoot);
}

#[cfg(unix)]
#[tokio::test]
async fn writes_preserve_hardlinks_open_handles_and_permissions() {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let host = Host::new().await;
    host.write("a", "old\n");
    std::fs::set_permissions(host.path("a"), std::fs::Permissions::from_mode(0o751)).unwrap();
    std::fs::hard_link(host.path("a"), host.path("alias")).unwrap();
    let mut open = std::fs::File::open(host.path("a")).unwrap();
    let before = open.metadata().unwrap();
    run(
        &host,
        "*** Begin Patch\n*** Update File: a\n@@\n-old\n+new\n*** End Patch",
    )
    .await
    .unwrap();
    let mut observed = String::new();
    open.read_to_string(&mut observed).unwrap();
    assert_eq!(observed, "new\n");
    assert_eq!(std::fs::read(host.path("alias")).unwrap(), b"new\n");
    let after = std::fs::metadata(host.path("a")).unwrap();
    assert_eq!(before.ino(), after.ino());
    assert_eq!(before.mode(), after.mode());

    host.write("readonly", "old\n");
    std::fs::set_permissions(
        host.path("readonly"),
        std::fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    // Root can write mode-0444 files; compare against the host's native behavior.
    if std::fs::OpenOptions::new()
        .write(true)
        .open(host.path("readonly"))
        .is_err()
    {
        let failure = run(
            &host,
            "*** Begin Patch\n*** Add File: readonly\n+new\n*** End Patch",
        )
        .await
        .unwrap_err();
        assert_eq!(failure.code, ExecutionErrorCode::PermissionDenied);
        assert_eq!(std::fs::read(host.path("readonly")).unwrap(), b"old\n");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn add_follows_dangling_symlinks_with_containment() {
    use std::os::unix::fs::symlink;
    let host = Host::new().await;
    symlink("second", host.path("link")).unwrap();
    symlink("target", host.path("second")).unwrap();
    run(
        &host,
        "*** Begin Patch\n*** Add File: link\n+created\n*** End Patch",
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(host.path("target")).unwrap(), b"created\n");
    assert!(
        std::fs::symlink_metadata(host.path("link"))
            .unwrap()
            .is_symlink()
    );
    symlink("missing-parent/target", host.path("missing-parent-link")).unwrap();
    let failure = run(
        &host,
        "*** Begin Patch\n*** Add File: missing-parent-link\n+x\n*** End Patch",
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, ExecutionErrorCode::NotFound);
    assert!(!host.path("missing-parent").exists());
    symlink("missing-dir", host.path("dir-link")).unwrap();
    assert!(
        run(
            &host,
            "*** Begin Patch\n*** Add File: dir-link/file\n+x\n*** End Patch"
        )
        .await
        .is_err()
    );
    assert!(!host.path("missing-dir").exists());
    symlink(host.temp.path().join("outside"), host.path("outside-link")).unwrap();
    let failure = run(
        &host,
        "*** Begin Patch\n*** Add File: outside-link\n+escape\n*** End Patch",
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, ExecutionErrorCode::PathOutsideRoot);
    assert!(!host.temp.path().join("outside").exists());
    symlink("cycle-b", host.path("cycle-a")).unwrap();
    symlink("cycle-a", host.path("cycle-b")).unwrap();
    assert!(
        run(
            &host,
            "*** Begin Patch\n*** Add File: cycle-a\n+x\n*** End Patch"
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn pending_checkpoint_replay_does_not_rewrite_or_remove_recreated_files() {
    let host = Host::new().await;
    host.write("a", "old\n");
    let tool = host.tool(ApplyPatchConfig::default());
    let prepared = tool
        .prepare(
            &context(),
            "*** Begin Patch\n*** Update File: a\n*** Move to: b\n@@\n-old\n+new\n*** End Patch",
            state(),
        )
        .await
        .unwrap();
    let mut running = tool.start(&prepared).unwrap();
    assert_eq!(
        tool.step(&context(), &mut running).await.unwrap(),
        PatchProgress::CheckpointRequired
    );
    let mut before_write: RunningPatch =
        serde_json::from_value(serde_json::to_value(&running).unwrap()).unwrap();
    tool.step(&context(), &mut running).await.unwrap();
    host.write("b", "external\n");
    tool.step(&context(), &mut before_write).await.unwrap();
    assert_eq!(std::fs::read(host.path("b")).unwrap(), b"external\n");
    let mut before_remove = running.clone();
    tool.step(&context(), &mut running).await.unwrap();
    host.write("a", "recreated\n");
    tool.step(&context(), &mut before_remove).await.unwrap();
    assert_eq!(std::fs::read(host.path("a")).unwrap(), b"recreated\n");
    assert!(before_remove.is_complete());
}

#[tokio::test]
async fn restart_fences_saved_progress() {
    let mut host = Host::new().await;
    host.write("a", "old\n");
    let tool = host.tool(ApplyPatchConfig::default());
    let prepared = tool
        .prepare(
            &context(),
            "*** Begin Patch\n*** Delete File: a\n*** End Patch",
            state(),
        )
        .await
        .unwrap();
    let mut running = tool.start(&prepared).unwrap();
    tool.apply(&context(), &mut running).await.unwrap();
    host.write("a", "old\n");
    host.runtime = SupervisorRuntime::new(SupervisorConfig {
        host_id: host.runtime.descriptor().host_id.clone(),
        state_directory: host.temp.path().join("state"),
        roots: vec![SupervisorRoot {
            id: RootId::new("work").unwrap(),
            name: "Work".into(),
            path: host.temp.path().join("workspace"),
            read_only: false,
        }],
        limits: SupervisorLimits::default(),
    })
    .await
    .unwrap();
    let failure = host
        .tool(ApplyPatchConfig::default())
        .apply(&context(), &mut running)
        .await
        .unwrap_err();
    assert_eq!(failure.code, ExecutionErrorCode::ExecutionLost);
    assert_eq!(std::fs::read(host.path("a")).unwrap(), b"old\n");
}

// Vendored upstream fixtures run against this tool and the real supervisor,
// including exact bytes, CRLF, and directories left behind by a move.
#[tokio::test]
async fn upstream_filesystem_scenarios() {
    use std::{collections::BTreeMap, path::Path};
    fn tree(root: &Path) -> BTreeMap<std::path::PathBuf, Option<Vec<u8>>> {
        fn visit(
            root: &Path,
            dir: &Path,
            entries: &mut BTreeMap<std::path::PathBuf, Option<Vec<u8>>>,
        ) {
            if !dir.exists() {
                return;
            }
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                if path.is_dir() {
                    entries.insert(relative, None);
                    visit(root, &path, entries);
                } else {
                    entries.insert(relative, Some(std::fs::read(&path).unwrap()));
                }
            }
        }
        let mut entries = BTreeMap::new();
        visit(root, root, &mut entries);
        entries
    }
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut count = 0;
    for entry in std::fs::read_dir(fixtures).unwrap() {
        let scenario = entry.unwrap().path();
        if !scenario.is_dir() {
            continue;
        }
        let host = Host::new().await;
        for (path, content) in tree(&scenario.join("input")) {
            if let Some(bytes) = content {
                host.write(path.to_str().unwrap(), bytes);
            } else {
                std::fs::create_dir_all(host.path(path.to_str().unwrap())).unwrap();
            }
        }
        let tool = host.tool(ApplyPatchConfig {
            update_file_mode: ApplyPatchFileUpdateMode::PreserveLineEndings,
            ..Default::default()
        });
        let patch = std::fs::read_to_string(scenario.join("patch.txt")).unwrap();
        let result = match tool.prepare(&context(), &patch, state()).await {
            Ok(prepared) => {
                tool.apply(&context(), &mut tool.start(&prepared).unwrap())
                    .await
            }
            Err(failure) => Err(failure),
        };
        let name = scenario.file_name().unwrap().to_str().unwrap();
        let should_fail = ["005_", "006_", "007_", "008_", "009_", "012_", "013_"]
            .iter()
            .any(|prefix| name.starts_with(prefix));
        assert_eq!(result.is_err(), should_fail, "{name}: {result:?}");
        assert_eq!(
            tree(&host.path("")),
            tree(&scenario.join("expected")),
            "{name}"
        );
        count += 1;
    }
    assert_eq!(count, 24);
}

#[tokio::test]
async fn preflight_rejects_missing_deletes_and_duplicate_normalized_sources() {
    let host = Host::new().await;
    for patch in [
        "*** Begin Patch\n*** Add File: early\n+x\n*** Delete File: missing\n*** End Patch",
        "*** Begin Patch\n*** Add File: early\n+x\n*** Add File: sub/../early\n+y\n*** End Patch",
    ] {
        assert!(run(&host, patch).await.is_err());
        assert!(!host.path("early").exists());
    }
}
