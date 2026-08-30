use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub avatar: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateProjectRequest {
    pub name: String,
    #[serde(default)]
    pub avatar: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateProjectRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_present_nullable")]
    pub avatar: Option<Option<String>>,
}

fn deserialize_present_nullable<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::UpdateProjectRequest;

    #[test]
    fn update_distinguishes_omitted_and_null_avatar() {
        let omitted: UpdateProjectRequest =
            serde_json::from_value(json!({ "name": "Renamed" })).unwrap();
        let cleared: UpdateProjectRequest =
            serde_json::from_value(json!({ "avatar": null })).unwrap();
        let replaced: UpdateProjectRequest =
            serde_json::from_value(json!({ "avatar": "https://example.com/avatar.png" })).unwrap();

        assert!(omitted.avatar.is_none());
        assert_eq!(cleared.avatar, Some(None));
        assert_eq!(
            replaced.avatar,
            Some(Some("https://example.com/avatar.png".to_owned()))
        );
    }
}
