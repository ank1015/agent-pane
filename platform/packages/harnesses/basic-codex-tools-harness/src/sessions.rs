//! Lease-scoped session storage. Output cursors are published with tool results.
use llm_contracts::{JsonObject, Message};
use platform_runtime_client::{Error, Result, RunClient, types::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tool_unified_exec::ExecSession;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProcessSession {
    pub session: ExecSession,
    pub created_by_run: Uuid,
}

#[derive(Default)]
pub(crate) struct Sessions {
    entries: BTreeMap<String, (i64, Option<JsonObject>)>,
    pending: BTreeMap<String, Option<JsonObject>>,
    inherited_max: i32,
}
impl Sessions {
    pub async fn load(client: &RunClient, messages: &[SessionMessage]) -> Result<Self> {
        let mut state = Self::default();
        let mut after_key = None;
        loop {
            let page = client
                .session_state(&SessionStateQuery {
                    namespace: crate::ID.into(),
                    after_key,
                    limit: Some(50),
                    ..Default::default()
                })
                .await?;
            state.accept(page.items);
            after_key = page.next_after_key;
            if after_key.is_none() {
                break;
            }
        }
        // Fork history retains exposed aliases but private state starts empty.
        for message in messages {
            if let Message::ToolResult(result) = &message.message {
                if result.tool_name == "exec_command" {
                    if let Some(id) = result
                        .details
                        .as_ref()
                        .and_then(|d| d.get("exec_session_id"))
                        .and_then(Value::as_i64)
                        .and_then(|id| i32::try_from(id).ok())
                    {
                        state.inherited_max = state.inherited_max.max(id);
                    }
                }
            }
        }
        Ok(state)
    }
    pub fn accept(&mut self, entries: Vec<SessionStateEntry>) {
        for entry in entries {
            self.entries.insert(entry.key, (entry.version, entry.value));
        }
        self.pending.clear();
    }
    pub fn writes(&self) -> Vec<SessionStateWrite> {
        self.pending
            .iter()
            .map(|(key, value)| SessionStateWrite {
                namespace: crate::ID.into(),
                key: key.clone(),
                expected_version: self.entries.get(key).map_or(0, |e| e.0),
                mutation: match value {
                    Some(value) => SessionStateMutation::Set {
                        value: value.clone(),
                    },
                    None => SessionStateMutation::Delete {},
                },
            })
            .collect()
    }
    fn value(&self, key: &str) -> Option<&JsonObject> {
        self.pending
            .get(key)
            .map(Option::as_ref)
            .unwrap_or_else(|| self.entries.get(key).and_then(|e| e.1.as_ref()))
    }
    pub fn all(&self) -> Result<Vec<ProcessSession>> {
        self.entries
            .keys()
            .filter(|key| key.starts_with("exec:"))
            .filter_map(|key| self.value(key))
            .map(|value| {
                serde_json::from_value(Value::Object(value.clone()))
                    .map_err(|_| Error::Invalid("invalid saved process session"))
            })
            .collect()
    }
    pub fn get(&self, id: i32) -> Result<Option<ProcessSession>> {
        self.value(&format!("exec:{id}"))
            .map(|value| {
                serde_json::from_value(Value::Object(value.clone()))
                    .map_err(|_| Error::Invalid("invalid saved process session"))
            })
            .transpose()
    }
    pub fn allocate(&mut self) -> Result<i32> {
        if self.all()?.len() >= 32 {
            return Err(Error::Invalid(
                "32 process sessions retained; poll existing sessions before starting another",
            ));
        }
        let previous = match self.value("exec-allocator") {
            Some(value) => value
                .get("last_id")
                .and_then(Value::as_i64)
                .and_then(|id| i32::try_from(id).ok())
                .ok_or(Error::Invalid("invalid process allocator"))?,
            None => 0,
        };
        let id = previous
            .max(self.inherited_max)
            .checked_add(1)
            .ok_or(Error::Invalid("process session IDs exhausted"))?;
        self.pending.insert(
            "exec-allocator".into(),
            Some(json!({"last_id":id}).as_object().unwrap().clone()),
        );
        Ok(id)
    }
    pub fn set(&mut self, id: i32, session: Option<ProcessSession>) {
        self.pending.insert(
            format!("exec:{id}"),
            session.map(|s| json!(s).as_object().unwrap().clone()),
        );
    }
}
