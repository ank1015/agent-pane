use futures_util::future::BoxFuture;
use platform_runtime_client::{Command, RequestKey, RunClient, types::*};
use serde_json::Value;
use tokio::sync::Mutex;
use tool_code_mode::{Entry, Error, Journal, Page, Result};
use uuid::Uuid;

/// Existing session-state transactions provide run ownership fencing and CAS.
/// A namespace per source run prevents unrelated session cells sharing identities.
pub struct RunJournal {
    client: RunClient,
    namespace: String,
    owner: String,
    writes: Mutex<()>,
}
impl RunJournal {
    pub fn new(client: RunClient) -> Self {
        let lease = client.lease();
        Self {
            namespace: format!("code_mode.{}", lease.run_id),
            owner: format!("{}:{}", lease.run_id, lease.lease_epoch),
            client,
            writes: Mutex::new(()),
        }
    }
}
fn entry(e: SessionStateEntry) -> Result<Entry> {
    Ok(Entry {
        key: e.key,
        version: e.version.try_into().map_err(|_| Error::Storage)?,
        value: Value::Object(e.value.ok_or(Error::Conflict)?),
    })
}
impl Journal for RunJournal {
    fn owner(&self) -> &str {
        &self.owner
    }
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Entry>>> {
        Box::pin(async move {
            let page = self
                .client
                .session_state(&SessionStateQuery {
                    namespace: self.namespace.clone(),
                    key: Some(key.into()),
                    ..Default::default()
                })
                .await
                .map_err(|_| Error::Storage)?;
            page.items.into_iter().next().map(entry).transpose()
        })
    }
    fn put<'a>(
        &'a self,
        key: &'a str,
        expected: u64,
        value: Value,
    ) -> BoxFuture<'a, Result<Entry>> {
        Box::pin(async move {
            let _lock = self.writes.lock().await;
            let context = self
                .client
                .context(&ContextQuery {
                    limit: Some(1),
                    ..Default::default()
                })
                .await
                .map_err(|_| Error::Storage)?;
            let mut commit = Commit::new(context.run.run.version, context.session.current_revision);
            commit.session_state.push(SessionStateWrite {
                namespace: self.namespace.clone(),
                key: key.into(),
                expected_version: expected
                    .try_into()
                    .map_err(|_| Error::Invalid("Invalid journal version"))?,
                mutation: SessionStateMutation::Set {
                    value: value
                        .as_object()
                        .ok_or(Error::Invalid("Journal values must be objects"))?
                        .clone(),
                },
            });
            // Trace references share the exact journal transaction; arguments,
            // results and source stay in bounded private session-state records.
            commit.events.push(HarnessEvent {
                r#type:"code_mode.journal".into(),
                payload:serde_json::json!({"key":key,"version":expected+1,"status":value["status"],"tool":value["tool"]}).as_object().unwrap().clone(),
                occurred_at:None,
            });
            let command = Command::new(
                RequestKey::new(format!("cm-journal:{}", Uuid::now_v7()))
                    .map_err(|_| Error::Storage)?,
                commit,
            );
            let result = self.client.commit(&command).await.map_err(|e| {
                if e.conflict().is_some() {
                    Error::Conflict
                } else {
                    Error::Storage
                }
            })?;
            result
                .session_state
                .into_iter()
                .next()
                .ok_or(Error::Storage)
                .and_then(entry)
        })
    }
    fn list<'a>(
        &'a self,
        prefix: &'a str,
        after: Option<&'a str>,
        limit: u32,
    ) -> BoxFuture<'a, Result<Page>> {
        Box::pin(async move {
            if !(1..=50).contains(&limit) {
                return Err(Error::Invalid("Invalid trace page limit"));
            }
            let mut cursor = after.unwrap_or(prefix).to_owned();
            let mut items = vec![];
            let mut bytes = 0;
            // Fetch one bounded state record per request. Large trace entries
            // cannot cause an unbounded session-state response in the transport.
            loop {
                let page = self
                    .client
                    .session_state(&SessionStateQuery {
                        namespace: self.namespace.clone(),
                        after_key: Some(cursor.clone()),
                        limit: Some(1),
                        ..Default::default()
                    })
                    .await
                    .map_err(|_| Error::Storage)?;
                let Some(next) = page.items.into_iter().next() else {
                    return Ok(Page {
                        items,
                        next_cursor: None,
                    });
                };
                if !next.key.starts_with(prefix) {
                    return Ok(Page {
                        items,
                        next_cursor: None,
                    });
                }
                let next = entry(next)?;
                let size = serde_json::to_vec(&next).map_err(|_| Error::Storage)?.len();
                if !items.is_empty() && (items.len() >= limit as usize || bytes + size > 224 * 1024)
                {
                    return Ok(Page {
                        items,
                        next_cursor: Some(cursor),
                    });
                }
                bytes += size;
                cursor = next.key.clone();
                items.push(next);
                if page.next_after_key.is_none() {
                    return Ok(Page {
                        items,
                        next_cursor: None,
                    });
                }
            }
        })
    }
}
