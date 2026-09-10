use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::sync::Arc;
use tool_code_mode::{
    Call, Dispatcher, Effect, Registry, Tool, ToolError,
    live::{
        ContentItem, Session, Status,
        protocol::{ExecInput, WaitInput},
    },
};

struct Echo;
impl Dispatcher for Echo {
    fn invoke<'a>(&'a self, call: &'a Call) -> BoxFuture<'a, Result<Value, ToolError>> {
        Box::pin(async move {
            if call.tool == "delayed" {
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            }
            Ok(call.input.clone())
        })
    }
}
fn session() -> (
    Session,
    tokio::sync::mpsc::Receiver<tool_code_mode::live::Notification>,
) {
    let mut registry = Registry::default();
    for (name, schema) in [
        ("echo-text", json!({"type":"string"})),
        ("delayed", json!({"type":"object"})),
    ] {
        registry
            .register(Tool {
                name: name.into(),
                description: "Echo input".into(),
                input_schema: schema,
                effect: Effect::Read,
            })
            .unwrap();
    }
    Session::new(
        env!("CARGO_BIN_EXE_code-mode-runtime"),
        registry,
        Arc::new(Echo),
        vec![],
    )
    .unwrap()
}
fn texts(items: &[ContentItem]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}
fn wait(id: &str) -> WaitInput {
    WaitInput {
        cell_id: id.into(),
        yield_time_ms: 2000,
        max_tokens: None,
        terminate: false,
    }
}

#[tokio::test]
async fn raw_module_registered_freeform_and_session_store() {
    let (mut session, _notifications) = session();
    let first = session
        .exec(
            "first".into(),
            ExecInput::parse("text(await tools.echo_text('hello')); store('answer', {n:42}); if (typeof ctx !== 'undefined') throw new Error('Unconfigured SDK context was exposed');")
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status, Status::Completed, "{first:?}");
    assert_eq!(texts(&first.content), vec!["hello"]);
    let second = session
        .exec(
            "second".into(),
            ExecInput::parse("text(load('answer')); text(typeof process); text(typeof fetch);")
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second.status, Status::Completed, "{second:?}");
    assert_eq!(
        texts(&second.content),
        vec!["{\"n\":42}", "undefined", "undefined"]
    );
}

#[tokio::test]
async fn yields_keep_cell_alive_and_wait_consumes_only_new_output() {
    let (mut session, _notifications) = session();
    let first=session.exec("first".into(),ExecInput::parse("// @exec: {\"yield_time_ms\": 1}\ntext('before'); await new Promise(r=>setTimeout(r,150)); text('after');").unwrap()).await.unwrap();
    assert_eq!(first.status, Status::Running, "{first:?}");
    let id = first.cell_id.clone();
    let second = session.wait(wait(&id)).await.unwrap();
    assert_eq!(second.status, Status::Completed, "{second:?}");
    let all: Vec<_> = first
        .content
        .iter()
        .chain(&second.content)
        .cloned()
        .collect();
    assert_eq!(texts(&all), vec!["before", "after"]);
    assert_eq!(
        session.wait(wait(&id)).await.unwrap().status,
        Status::Failed
    );
}

#[tokio::test]
async fn explicit_yield_notification_and_termination_of_cpu_loop() {
    let (mut session, mut notifications) = session();
    let first = session
        .exec(
            "call".into(),
            ExecInput::parse(
                "text('progress'); notify('notice'); await yield_control(); while(true) {}",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status, Status::Running, "{first:?}");
    assert_eq!(texts(&first.content), vec!["progress"]);
    let notice = notifications.recv().await.unwrap();
    assert_eq!(notice.call_id, "call");
    assert_eq!(notice.text, "notice");
    let terminated = session
        .wait(WaitInput {
            terminate: true,
            ..wait(&first.cell_id)
        })
        .await
        .unwrap();
    assert_eq!(terminated.status, Status::Terminated);
}

#[tokio::test]
async fn pending_calls_dispatch_concurrently() {
    // A two-party barrier deadlocks if the host serializes nested dispatch.
    struct Barrier(tokio::sync::Barrier);
    impl Dispatcher for Barrier {
        fn invoke<'a>(&'a self, _: &'a Call) -> BoxFuture<'a, Result<Value, ToolError>> {
            Box::pin(async move {
                self.0.wait().await;
                Ok(json!(true))
            })
        }
    }
    let mut registry = Registry::default();
    registry
        .register(Tool {
            name: "join".into(),
            description: "Barrier".into(),
            input_schema: json!({"type":"object"}),
            effect: Effect::Read,
        })
        .unwrap();
    let (mut session, _notifications) = Session::new(
        env!("CARGO_BIN_EXE_code-mode-runtime"),
        registry,
        Arc::new(Barrier(tokio::sync::Barrier::new(2))),
        vec![],
    )
    .unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        session.exec(
            "barrier".into(),
            ExecInput::parse("text(await Promise.all([tools.join({}), tools.join({})]));").unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.status, Status::Completed, "{result:?}");
    assert_eq!(texts(&result.content), vec!["[true,true]"]);
}

#[tokio::test]
async fn module_errors_exit_and_unawaited_timers() {
    let (mut session, _notifications) = session();
    for (source, status, expected) in [
        (
            "text('saved'); throw Error('failure');",
            Status::Failed,
            vec!["saved"],
        ),
        (
            "text('saved'); exit(); text('unreachable');",
            Status::Completed,
            vec!["saved"],
        ),
        (
            "text('saved'); try { exit(); } catch(e) {} text('unreachable');",
            Status::Completed,
            vec!["saved"],
        ),
        (
            "setTimeout(()=>text('unreachable'),5000); text('done');",
            Status::Completed,
            vec!["done"],
        ),
        ("return 1;", Status::Failed, vec![]),
    ] {
        let result = session
            .exec("case".into(), ExecInput::parse(source).unwrap())
            .await
            .unwrap();
        assert_eq!(result.status, status, "{result:?}");
        assert_eq!(texts(&result.content), expected);
    }
}

#[test]
fn pragma_validation_and_wait_defaults() {
    for input in [
        "",
        "// @exec: {}",
        "// @exec: {\"seconds\":1}\ntext(1)",
        "// @exec: {\"yield_time_ms\":-1}\ntext(1)",
    ] {
        assert!(ExecInput::parse(input).is_err());
    }
    let input: WaitInput = serde_json::from_value(json!({"cell_id":"1"})).unwrap();
    assert_eq!(input.yield_time_ms, 10000);
    assert!(!input.terminate);
}

#[tokio::test]
async fn overlapping_cells_read_live_store_and_globals_are_fresh() {
    let (mut session, _notifications) = session();
    let first = session.exec("reader".into(), ExecInput::parse("await yield_control(); await new Promise(r=>setTimeout(r,250)); text(load('shared')); text(typeof globalThis.other);").unwrap()).await.unwrap();
    assert_eq!(first.status, Status::Running);
    let second = session.exec("writer".into(), ExecInput::parse("globalThis.other = 42; store('shared', {ready:true}); store('__proto__', 'safe'); text(load('__proto__')); text(load('missing'));").unwrap()).await.unwrap();
    assert_eq!(second.status, Status::Completed, "{second:?}");
    assert_eq!(texts(&second.content), vec!["safe", "undefined"]);
    let result = session.wait(wait(&first.cell_id)).await.unwrap();
    assert_eq!(result.status, Status::Completed, "{result:?}");
    assert_eq!(
        texts(&result.content),
        vec!["{\"ready\":true}", "undefined"]
    );
}

#[tokio::test]
async fn media_helpers_and_schema_errors_preserve_shapes() {
    let (mut session, _notifications) = session();
    let result = session.exec("media".into(), ExecInput::parse(r#"
        image({type:'image',mimeType:'image/png',data:'AAAA',_meta:{'codex/imageDetail':'original'}},'low');
        audio({type:'audio',mimeType:'audio/wav',data:'AAAA'});
        generatedImage({image_url:'data:image/png;base64,AAAA',output_hint:'saved'});
        try { await tools.echo_text({bad:true}); } catch(e) { text(e.code); }
        const timer = setTimeout(()=>text('cancelled callback'),0); clearTimeout(timer);
        await new Promise(r=>setTimeout(r,20));
    "#).unwrap()).await.unwrap();
    assert_eq!(result.status, Status::Completed, "{result:?}");
    assert_eq!(
        result.content,
        vec![
            ContentItem::InputImage {
                image_url: "data:image/png;base64,AAAA".into(),
                detail: Some("low".into())
            },
            ContentItem::InputAudio {
                audio_url: "data:audio/wav;base64,AAAA".into()
            },
            ContentItem::InputImage {
                image_url: "data:image/png;base64,AAAA".into(),
                detail: None
            },
            ContentItem::InputText {
                text: "saved".into()
            },
            ContentItem::InputText {
                text: "INVALID_TOOL_INPUT".into()
            },
        ]
    );
}

#[test]
fn normalized_name_collisions_are_rejected() {
    let mut registry = Registry::default();
    for name in ["a.b", "a_b"] {
        registry
            .register(Tool {
                name: name.into(),
                description: "test".into(),
                input_schema: json!({"type":"object"}),
                effect: Effect::Read,
            })
            .unwrap();
    }
    assert!(
        Session::new(
            env!("CARGO_BIN_EXE_code-mode-runtime"),
            registry,
            Arc::new(Echo),
            vec![]
        )
        .is_err()
    );
}

#[test]
fn rendered_results_keep_status_and_budget_output() {
    use tool_code_mode::live::protocol::Report;
    let report = |content| Report {
        cell_id: "cell".into(),
        status: Status::Completed,
        content,
        error: None,
        wall_time: 0.12,
        max_tokens: Some(5),
    };
    assert_eq!(
        texts(
            &report(vec![ContentItem::InputText {
                text: "0123456789".repeat(4)
            }])
            .render()
        ),
        vec![
            "Script completed\nWall time 0.1 seconds\nOutput:\n",
            "Warning: truncated output (original token count: 10)\nTotal output lines: 1\n\n0123456789…5 tokens truncated…0123456789",
        ]
    );
    assert_eq!(
        texts(
            &report(vec![ContentItem::InputAudio {
                audio_url: format!("data:audio/wav;base64,{}", "A".repeat(100))
            }])
            .render()
        ),
        vec![
            "Script completed\nWall time 0.1 seconds\nOutput:\n",
            "[omitted 1 audio items ...]",
        ]
    );
    for budget in 0..8 {
        let mut result = report(vec![ContentItem::InputText {
            text: "🌍".repeat(12),
        }]);
        result.max_tokens = Some(budget);
        assert!(texts(&result.render())[1].contains("truncated"));
    }
}

#[tokio::test]
async fn explicit_yield_is_an_output_boundary() {
    let (mut session, _notifications) = session();
    let first = session
        .exec(
            "yield".into(),
            ExecInput::parse("text('first'); await yield_control(); text('second');").unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status, Status::Running);
    assert_eq!(texts(&first.content), vec!["first"]);
    let second = session.wait(wait(&first.cell_id)).await.unwrap();
    assert_eq!(second.status, Status::Completed);
    assert_eq!(texts(&second.content), vec!["second"]);
}

#[tokio::test]
async fn terminating_or_dropping_session_cancels_pending_dispatch() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Active(Arc<AtomicBool>);
    impl Drop for Active {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    struct Blocking {
        active: Arc<AtomicBool>,
        entered: tokio::sync::Notify,
    }
    impl Dispatcher for Blocking {
        fn invoke<'a>(&'a self, _: &'a Call) -> BoxFuture<'a, Result<Value, ToolError>> {
            Box::pin(async move {
                self.active.store(true, Ordering::SeqCst);
                let _active = Active(self.active.clone());
                self.entered.notify_one();
                std::future::pending().await
            })
        }
    }
    for terminate in [true, false] {
        let dispatcher = Arc::new(Blocking {
            active: Arc::new(AtomicBool::new(false)),
            entered: tokio::sync::Notify::new(),
        });
        let mut registry = Registry::default();
        registry
            .register(Tool {
                name: "block".into(),
                description: "Wait indefinitely".into(),
                input_schema: json!({"type":"object"}),
                effect: Effect::Read,
            })
            .unwrap();
        let (mut session, _notifications) = Session::new(
            env!("CARGO_BIN_EXE_code-mode-runtime"),
            registry,
            dispatcher.clone(),
            vec![],
        )
        .unwrap();
        let result = session
            .exec(
                "cancel".into(),
                ExecInput::parse("// @exec: {\"yield_time_ms\":1}\nawait tools.block({});")
                    .unwrap(),
            )
            .await
            .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            dispatcher.entered.notified(),
        )
        .await
        .unwrap();
        if terminate {
            assert_eq!(
                session
                    .wait(WaitInput {
                        terminate: true,
                        ..wait(&result.cell_id)
                    })
                    .await
                    .unwrap()
                    .status,
                Status::Terminated
            );
        }
        drop(session);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while dispatcher.active.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
