#![cfg(any(target_os = "macos", target_os = "linux"))]
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::{Mutex, Notify, watch};
use tool_code_mode::*;
use uuid::Uuid;

#[derive(Clone)]
struct Memory {
    owner: String,
    entries: Arc<Mutex<BTreeMap<String, Entry>>>,
    lose_result_ack: Arc<AtomicBool>,
}
impl Memory {
    fn new() -> Self {
        Self {
            owner: "first".into(),
            entries: Default::default(),
            lose_result_ack: Default::default(),
        }
    }
}
impl Journal for Memory {
    fn owner(&self) -> &str {
        &self.owner
    }
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Entry>>> {
        Box::pin(async move { Ok(self.entries.lock().await.get(key).cloned()) })
    }
    fn put<'a>(&'a self, key: &'a str, version: u64, value: Value) -> BoxFuture<'a, Result<Entry>> {
        Box::pin(async move {
            let mut map = self.entries.lock().await;
            if map.get(key).map_or(0, |e| e.version) != version {
                return Err(Error::Conflict);
            }
            let entry = Entry {
                key: key.into(),
                version: version + 1,
                value,
            };
            map.insert(key.into(), entry.clone());
            if key.starts_with("call.")
                && entry.value["status"] == "succeeded"
                && self.lose_result_ack.swap(false, Ordering::SeqCst)
            {
                return Err(Error::Storage);
            }
            Ok(entry)
        })
    }
    fn list<'a>(
        &'a self,
        prefix: &'a str,
        after: Option<&'a str>,
        limit: u32,
    ) -> BoxFuture<'a, Result<Page>> {
        Box::pin(async move {
            let map = self.entries.lock().await;
            let rows: Vec<_> = map
                .values()
                .filter(|e| e.key.starts_with(prefix) && after.is_none_or(|a| e.key.as_str() > a))
                .cloned()
                .collect();
            let items: Vec<Entry> = rows.iter().take(limit as usize).cloned().collect();
            let next_cursor = (rows.len() > items.len()).then(|| items.last().unwrap().key.clone());
            Ok(Page { items, next_cursor })
        })
    }
}
struct Tools {
    journal: Memory,
    effects: AtomicUsize,
    hold: bool,
    entered: Notify,
}
impl Dispatcher for Tools {
    fn invoke<'a>(
        &'a self,
        call: &'a Call,
    ) -> BoxFuture<'a, std::result::Result<Value, ToolError>> {
        Box::pin(async move {
            let saved = self
                .journal
                .get(&call_key(call.cell_id, call.sequence))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                saved.value["status"], "prepared",
                "journal precedes every dispatch"
            );
            if call.tool == "write.value" {
                self.effects.fetch_add(1, Ordering::SeqCst);
                if self.hold {
                    self.entered.notify_one();
                    std::future::pending::<()>().await;
                }
            }
            Ok(call.input.clone())
        })
    }
}
fn registry() -> Registry {
    let mut r = Registry::default();
    for (name, effect, schema) in [
        ("echo-freeform", Effect::Read, json!({"type":"string"})),
        (
            "write.value",
            Effect::Mutation,
            json!({"type":"object","required":["value"],"properties":{"value":{}},"additionalProperties":false}),
        ),
    ] {
        r.register(Tool {
            name: name.into(),
            description: "Test tool".into(),
            input_schema: schema,
            effect,
        })
        .unwrap();
    }
    r
}
fn engine() -> Engine {
    Engine::new(env!("CARGO_BIN_EXE_code-mode-runtime"))
}

#[tokio::test]
async fn lost_journal_acknowledgement_interrupts_source_and_retains_effect_receipt() {
    let journal = Memory::new();
    journal.lose_result_ack.store(true, Ordering::SeqCst);
    let tools = tools(&journal);
    let registry = registry();
    let engine = engine();
    let (_send, cancel) = watch::channel(false);
    let input = Input {
        id: Uuid::now_v7(),
        source: "await tools['write.value']({value:1}); await tools['write.value']({value:2});"
            .into(),
    };
    let cell = engine
        .execute(input.clone(), &registry, &journal, &tools, cancel.clone())
        .await
        .unwrap();
    assert_eq!(cell.status, CellStatus::Interrupted);
    assert_eq!(tools.effects.load(Ordering::SeqCst), 1);
    let trace = Engine::inspect(&journal, input.id, None, 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(trace.calls.items.len(), 1);
    assert_eq!(trace.calls.items[0].value["status"], "succeeded");
    assert_eq!(
        engine
            .execute(input, &registry, &journal, &tools, cancel)
            .await
            .unwrap()
            .status,
        CellStatus::Interrupted
    );
    assert_eq!(tools.effects.load(Ordering::SeqCst), 1);
}
fn tools(journal: &Memory) -> Tools {
    Tools {
        journal: journal.clone(),
        effects: AtomicUsize::new(0),
        hold: false,
        entered: Notify::new(),
    }
}

#[tokio::test]
async fn generic_registry_freeform_tools_and_durable_nested_traces() {
    let journal = Memory::new();
    let tools = tools(&journal);
    let engine = engine();
    let registry = registry();
    let (_send, cancel) = watch::channel(false);
    let input=Input {id:Uuid::now_v7(),source:"const a = await tools['echo-freeform']('hello'); const b = await tools['write.value']({value:a}); text(b); return ALL_TOOLS.map(t=>t.name);".into()};
    let cell = engine
        .execute(input.clone(), &registry, &journal, &tools, cancel.clone())
        .await
        .unwrap();
    assert_eq!(cell.status, CellStatus::Completed);
    assert_eq!(cell.output, vec![json!({"value":"hello"})]);
    let inspect = Engine::inspect(&journal, input.id, None, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(inspect.calls.items.len(), 1);
    assert!(inspect.calls.next_cursor.is_some());
    let next = Engine::inspect(&journal, input.id, inspect.calls.next_cursor.as_deref(), 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.calls.items[0].value["status"], "succeeded");
    assert_eq!(next.calls.items[0].value["input"], json!({"value":"hello"}));
    assert_eq!(
        engine
            .execute(input.clone(), &registry, &journal, &tools, cancel.clone())
            .await
            .unwrap()
            .status,
        CellStatus::Completed
    );
    assert_eq!(tools.effects.load(Ordering::SeqCst), 1);
    assert!(
        engine
            .execute(
                Input {
                    source: "return 2".into(),
                    ..input
                },
                &registry,
                &journal,
                &tools,
                cancel
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn partial_failures_keep_effects_and_guest_has_only_registered_capabilities() {
    let journal = Memory::new();
    let tools = tools(&journal);
    let engine = engine();
    let registry = registry();
    let (_send, cancel) = watch::channel(false);
    let input = Input {
        id: Uuid::now_v7(),
        source: r#"
      text([typeof process,typeof fetch,typeof require,typeof __codeHost,typeof __codeInput]);
      try { await tools['write.value']({unexpected:true}); } catch(e) { text(e.code); }
      await tools['write.value']({value:'accepted'});
      throw new Error('later failure');
    "#
        .into(),
    };
    let cell = engine
        .execute(input.clone(), &registry, &journal, &tools, cancel)
        .await
        .unwrap();
    assert_eq!(cell.status, CellStatus::Failed);
    assert_eq!(
        cell.output[0],
        json!([
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined"
        ])
    );
    assert_eq!(cell.output[1], "INVALID_TOOL_INPUT");
    assert_eq!(tools.effects.load(Ordering::SeqCst), 1);
    let trace = Engine::inspect(&journal, input.id, None, 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(trace.calls.items[0].value["status"], "rejected");
    assert_eq!(trace.calls.items[1].value["status"], "succeeded");
}

#[tokio::test]
async fn replacement_interrupts_lost_cell_without_replaying_source_or_effects() {
    let journal = Memory::new();
    let mut dispatcher = tools(&journal);
    dispatcher.hold = true;
    let engine = engine();
    let registry = registry();
    let (_send, cancel) = watch::channel(false);
    let input = Input {
        id: Uuid::now_v7(),
        source: "await tools['write.value']({value:'already accepted'}); return 1;".into(),
    };
    {
        let work = engine.execute(
            input.clone(),
            &registry,
            &journal,
            &dispatcher,
            cancel.clone(),
        );
        tokio::pin!(work);
        tokio::select! {_ = dispatcher.entered.notified()=>{},result=&mut work=>panic!("unexpected completion {result:?}"),_ = tokio::time::sleep(std::time::Duration::from_secs(5))=>panic!("no dispatch")}
        // Drop the activation while the tool's accepted result is unavailable.
    }
    let replacement = Memory {
        owner: "replacement".into(),
        ..journal.clone()
    };
    let cell = engine
        .execute(input.clone(), &registry, &replacement, &dispatcher, cancel)
        .await
        .unwrap();
    assert_eq!(cell.status, CellStatus::Interrupted);
    assert_eq!(dispatcher.effects.load(Ordering::SeqCst), 1);
    let trace = Engine::inspect(&replacement, input.id, None, 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(trace.calls.items[0].value["status"], "prepared");
    assert_eq!(
        trace.calls.items[0].value["operation_key"],
        format!("cm:{}:1", input.id)
    );
}

#[tokio::test]
async fn deadlines_cancellation_output_and_call_limits_are_enforced() {
    let journal = Memory::new();
    let tools = tools(&journal);
    let mut engine = engine();
    let registry = registry();
    let (_send, cancel) = watch::channel(false);
    engine.limits.timeout_ms = 2000;
    let cell = engine
        .execute(
            Input {
                id: Uuid::now_v7(),
                source: "await tools['write.value']({value:1}); while(true) {}".into(),
            },
            &registry,
            &journal,
            &tools,
            cancel.clone(),
        )
        .await
        .unwrap();
    assert_eq!(cell.status, CellStatus::Interrupted);
    assert_eq!(tools.effects.load(Ordering::SeqCst), 1);
    engine.limits.timeout_ms = 5000;
    engine.limits.output_bytes = 32;
    let cell = engine
        .execute(
            Input {
                id: Uuid::now_v7(),
                source: "text('saved'); text('x'.repeat(100));".into(),
            },
            &registry,
            &journal,
            &tools,
            cancel.clone(),
        )
        .await
        .unwrap();
    assert_eq!(cell.status, CellStatus::Interrupted);
    assert_eq!(cell.output, vec![json!("saved")]);
    engine.limits.max_calls = 1;
    let cell = engine
        .execute(
            Input {
                id: Uuid::now_v7(),
                source: "await tools['echo-freeform']('a'); await tools['write.value']({value:2});"
                    .into(),
            },
            &registry,
            &journal,
            &tools,
            cancel,
        )
        .await
        .unwrap();
    assert_eq!(cell.status, CellStatus::Interrupted);
    assert_eq!(tools.effects.load(Ordering::SeqCst), 1);
    let (_send, cancel) = watch::channel(true);
    let cell = engine
        .execute(
            Input {
                id: Uuid::now_v7(),
                source: "await tools['write.value']({value:3});".into(),
            },
            &registry,
            &journal,
            &tools,
            cancel,
        )
        .await
        .unwrap();
    assert_eq!(cell.status, CellStatus::Interrupted);
    assert_eq!(tools.effects.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn registry_changes_conflict_and_oversized_results_remain_inspectable() {
    let journal = Memory::new();
    let tools = tools(&journal);
    let mut engine = engine();
    engine.limits.result_bytes = 4;
    let mut registry = registry();
    let (_send, cancel) = watch::channel(false);
    let id = Uuid::now_v7();
    let input = Input {id,source:"try { await tools['write.value']({value:'large'}); } catch(e) { return {code:e.code,uncertain:e.uncertain}; }".into()};
    let cell = engine
        .execute(input.clone(), &registry, &journal, &tools, cancel.clone())
        .await
        .unwrap();
    assert_eq!(cell.status, CellStatus::Completed);
    assert_eq!(cell.value.unwrap()["uncertain"], true);
    let trace = Engine::inspect(&journal, id, None, 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(trace.calls.items[0].value["status"], "uncertain");
    registry
        .register(Tool {
            name: "another-tool".into(),
            description: "New tool after the cell completed".into(),
            input_schema: json!({"type":"string"}),
            effect: Effect::Read,
        })
        .unwrap();
    assert!(matches!(
        engine
            .execute(input, &registry, &journal, &tools, cancel)
            .await,
        Err(Error::Conflict)
    ));
    assert_eq!(tools.effects.load(Ordering::SeqCst), 1);
}
