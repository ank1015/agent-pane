use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct ProviderAccount {
    pub id: Uuid,
    pub provider: String,
    pub name: String,
    pub config: Value,
    pub status: String,
    pub is_default: bool,
    pub runtime_revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Clone, sqlx::FromRow)]
pub struct ResolvedAccount {
    pub id: Uuid,
    pub provider: String,
    pub name: String,
    pub config: Value,
    pub status: String,
    pub is_default: bool,
    pub runtime_revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub encrypted_payload: Vec<u8>,
    pub nonce: Vec<u8>,
    pub encryption_key_version: i32,
    pub credential_version: i64,
    pub credential_expires_at: Option<DateTime<Utc>>,
    pub credential_refreshed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct AdminAccount {
    pub id: Uuid,
    pub provider: String,
    pub name: String,
    pub config: Value,
    pub status: String,
    pub is_default: bool,
    pub runtime_revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub credential_version: i64,
    pub encryption_key_version: i32,
    pub credential_expires_at: Option<DateTime<Utc>>,
    pub credential_refreshed_at: Option<DateTime<Utc>>,
    pub credential_updated_at: DateTime<Utc>,
}
