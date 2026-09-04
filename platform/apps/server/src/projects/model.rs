use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::ProjectError;

const MAX_NAME_CHARACTERS: usize = 128;
const MAX_AVATAR_CHARACTERS: usize = 800_000;
const MAX_AVATAR_BYTES: usize = 512 * 1024;
const AVATAR_PREFIXES: [&str; 4] = [
    "data:image/png;base64,",
    "data:image/jpeg;base64,",
    "data:image/webp;base64,",
    "data:image/gif;base64,",
];

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub avatar: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateProjectInput {
    pub name: String,
    #[serde(default)]
    pub avatar: Option<String>,
}

impl CreateProjectInput {
    pub fn validate(&self) -> Result<(), ProjectError> {
        if self.name.trim().is_empty()
            || self.name.trim() != self.name
            || self.name.chars().count() > MAX_NAME_CHARACTERS
            || self.name.chars().any(char::is_control)
        {
            return Err(ProjectError::InvalidRequest(
                "Project name must contain 1–128 characters without surrounding whitespace or control characters.",
            ));
        }
        if let Some(avatar) = self.avatar.as_deref() {
            validate_avatar(avatar)?;
        }
        Ok(())
    }
}

fn validate_avatar(avatar: &str) -> Result<(), ProjectError> {
    if avatar.trim() != avatar || avatar.chars().count() > MAX_AVATAR_CHARACTERS {
        return Err(ProjectError::InvalidRequest(
            "Project avatar must be a PNG, JPEG, WebP, or GIF image no larger than 512 KB.",
        ));
    }
    let encoded = AVATAR_PREFIXES
        .iter()
        .find_map(|prefix| avatar.strip_prefix(prefix))
        .ok_or(ProjectError::InvalidRequest(
            "Project avatar must be a PNG, JPEG, WebP, or GIF image no larger than 512 KB.",
        ))?;
    let decoded = STANDARD.decode(encoded).map_err(|_| {
        ProjectError::InvalidRequest(
            "Project avatar must be a valid base64-encoded PNG, JPEG, WebP, or GIF image.",
        )
    })?;
    if decoded.is_empty() || decoded.len() > MAX_AVATAR_BYTES {
        return Err(ProjectError::InvalidRequest(
            "Project avatar must be a PNG, JPEG, WebP, or GIF image no larger than 512 KB.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::CreateProjectInput;

    #[test]
    fn validates_project_inputs() {
        assert!(
            CreateProjectInput {
                name: "Agent Pane".into(),
                avatar: None
            }
            .validate()
            .is_ok()
        );
        for name in ["", " padded", "line\nbreak"] {
            assert!(
                CreateProjectInput {
                    name: name.into(),
                    avatar: None
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            CreateProjectInput {
                name: "Avatar".into(),
                avatar: Some("data:image/png;base64,aGVsbG8=".into())
            }
            .validate()
            .is_ok()
        );
        assert!(
            CreateProjectInput {
                name: "Bad".into(),
                avatar: Some("https://example.com/image.png".into())
            }
            .validate()
            .is_err()
        );
    }
}
