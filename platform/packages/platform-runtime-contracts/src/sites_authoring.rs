//! Direct live authoring. Snapshots are exclusively user-facing operations.
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Files {
    pub frontend: String,
    pub backend: String,
}
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Source {
    pub site_id: Uuid,
    pub files: Files,
    pub release_id: Option<Uuid>,
}
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub patch: String,
}
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize)]
pub struct Operation {
    pub id: Uuid,
    #[serde(flatten)]
    pub action: Action,
}
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Patch { patch: String },
    Snapshot { name: String },
    Restore { snapshot_id: Uuid },
}
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub id: Uuid,
    pub name: String,
    pub release_id: Uuid,
    pub created_at: String,
}
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sql {
    pub sql: String,
    #[serde(default)]
    pub params: Vec<serde_json::Value>,
}
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub body: serde_json::Value,
}

impl<'de> Deserialize<'de> for Operation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let mut value = serde_json::Value::deserialize(deserializer)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| D::Error::custom("Expected operation object"))?;
        let id = serde_json::from_value(
            object
                .remove("id")
                .ok_or_else(|| D::Error::missing_field("id"))?,
        )
        .map_err(D::Error::custom)?;
        let action = serde_json::from_value(value).map_err(D::Error::custom)?;
        Ok(Self { id, action })
    }
}
