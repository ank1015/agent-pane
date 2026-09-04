use crate::{Error, Result};
use serde::{Deserialize, Deserializer, Serialize};

/// Caller-chosen identity of one logical mutation. Never silently regenerated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct RequestKey(String);
impl RequestKey {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > 256 || !value.bytes().all(|b| (33..=126).contains(&b))
        {
            return Err(Error::Invalid(
                "request keys require 1–256 printable ASCII characters without spaces",
            ));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for RequestKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Persist this whole value before recovery-critical coordination. Reusing only
/// its key with a reconstructed/different payload is not a safe retry.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command<T> {
    key: RequestKey,
    body: T,
}
impl<T> Command<T> {
    pub fn new(key: RequestKey, body: T) -> Self {
        Self { key, body }
    }
    pub fn key(&self) -> &RequestKey {
        &self.key
    }
    pub fn body(&self) -> &T {
        &self.body
    }
}
