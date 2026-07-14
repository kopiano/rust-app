use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, FromRow)]
#[allow(dead_code)]
pub struct Message {
    pub id: i64,
    pub message_id: Uuid,
    pub chat_type: String,
    pub send_id: Uuid,
    pub receiver_id: Option<Uuid>,
    pub content: Option<String>,
    pub message_type: i16,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub update_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub file_name: Option<String>,
    pub file_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct MessageUserInfo {
    pub user_id: Uuid,
    pub avatar: Option<String>,
    pub username: String,
    pub status: bool,
    pub content: Option<String>,
    pub last_message_time: Option<DateTime<Utc>>,
}
