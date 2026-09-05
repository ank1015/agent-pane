//! Private, session-scoped storage. Namespaces belong to the session's harness.
use crate::JsonObject;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const SESSION_STATE_MAX_VALUE_BYTES: usize = 256 * 1024;
pub const SESSION_STATE_MAX_WRITES: usize = 200;
pub const SESSION_STATE_MAX_PAGE_SIZE: u32 = 50;

pub fn valid_state_namespace(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
}
pub fn valid_state_key(value: &str) -> bool {
    (1..=256).contains(&value.len()) && value.bytes().all(|b| (33..=126).contains(&b))
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStateQuery {
    pub namespace: String,
    /// Exact lookup. A missing key returns an empty page, not a tombstone.
    pub key: Option<String>,
    /// Exclusive, byte-ordered cursor within this namespace. Includes tombstones.
    pub after_key: Option<String>,
    pub limit: Option<u32>,
}
impl SessionStateQuery {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !valid_state_namespace(&self.namespace)
            || self.key.as_deref().is_some_and(|v| !valid_state_key(v))
            || self
                .after_key
                .as_deref()
                .is_some_and(|v| !valid_state_key(v))
            || self.key.is_some() && self.after_key.is_some()
            || self
                .limit
                .is_some_and(|n| !(1..=SESSION_STATE_MAX_PAGE_SIZE).contains(&n))
        {
            return Err("Provide a valid namespace, key or after_key, and a limit of 1–50.");
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionStateMutation {
    Set { value: JsonObject },
    Delete {},
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStateWrite {
    pub namespace: String,
    pub key: String,
    /// Zero only for a key that has never existed. Deletions retain versions.
    pub expected_version: i64,
    pub mutation: SessionStateMutation,
}

#[derive(Serialize, Deserialize)]
pub struct SessionStateEntry {
    pub session_id: Uuid,
    pub namespace: String,
    pub key: String,
    pub version: i64,
    /// None is a tombstone, not an absent record. Use its version when recreating.
    pub value: Option<JsonObject>,
    pub saved_by_run_id: Uuid,
    pub saved_by_lease_epoch: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Serialize, Deserialize)]
pub struct SessionStatePage {
    pub session_id: Uuid,
    pub items: Vec<SessionStateEntry>,
    pub next_after_key: Option<String>,
}
