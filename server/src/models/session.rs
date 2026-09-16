use serde::Serialize;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Session {
    pub id: String,
    pub admin_id: i64,
    pub expires_at: i64,
    pub created_at: i64,
    pub last_seen_at: i64,
}
