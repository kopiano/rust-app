use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, FromRow)]
#[allow(dead_code)]
pub struct Message {
    pub id: i64,
    pub conversation_id: Uuid,
    pub chat_type: String,
    pub send_id: Uuid,
    pub receiver_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
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
    pub user_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    pub chat_type: String,
    pub avatar: Option<String>,
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<bool>,
    pub content: Option<String>,
    pub last_message_time: Option<DateTime<Utc>>,
    pub members: sqlx::types::Json<Vec<ChatMemberInfo>>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ChatMemberInfo {
    pub user_id: Uuid,
    pub avatar: Option<String>,
    pub username: String,
    pub status: bool,
}
