use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct ProviderAccount {
    pub id: Uuid,
    pub provider: String,
    pub name: String,
    pub secret_id: Uuid,
    pub config: Value,
    pub enabled: bool,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, sqlx::FromRow)]
pub struct ResolvedAccount {
    pub id: Uuid,
    pub provider: String,
    pub name: String,
    pub secret_id: Uuid,
    pub config: Value,
    pub enabled: bool,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub encrypted_payload: Vec<u8>,
    pub nonce: Vec<u8>,
    pub encryption_key_version: i32,
    pub credential_version: i64,
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct AdminAccount {
    pub id: Uuid,
    pub provider: String,
    pub name: String,
    pub config: Value,
    pub enabled: bool,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub credential_version: i64,
    pub encryption_key_version: i32,
    pub credential_updated_at: DateTime<Utc>,
}
