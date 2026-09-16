use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub retention_days: i64,
    pub session_days: i64,
    pub metrics_retention_days: i64,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettings {
    pub retention_days: Option<i64>,
    pub session_days: Option<i64>,
    pub metrics_retention_days: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
    pub confirm_password: String,
}

#[derive(Debug, Deserialize)]
pub struct SetupRequest {
    pub username: String,
    pub password: String,
    pub confirm_password: String,
}
