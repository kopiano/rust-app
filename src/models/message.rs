use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SendMessageRequest {
    pub chat_type: String,
    pub receiver_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    pub content: String,
    pub message_type: Option<i16>,
    pub client_message_id: Uuid,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CreateGroupRequest {
    pub name: String,
    pub member_ids: Vec<Uuid>,
    pub avatar: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AddGroupMembersRequest {
    pub member_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateGroupResponse {
    pub group_id: Uuid,
    pub name: String,
    pub avatar: Option<String>,
    pub member_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct AddGroupMembersResponse {
    pub added_count: u64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[allow(dead_code)]
pub struct Message {
    pub id: i64,
    pub conversation_id: Uuid,
    pub chat_type: String,
    pub send_id: Uuid,
    pub client_message_id: Option<Uuid>,
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

#[derive(Debug, Clone, Serialize)]
pub struct MessageBroadcast {
    pub event: &'static str,
    pub message: Message,
    pub recipients: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct MessageUserInfo {
    pub user_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    pub chat_type: String,
    pub avatar: Option<String>,
    pub username: String,
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub online: Option<bool>,
    pub is_pro: bool,
    pub content: Option<String>,
    pub last_message_time: Option<DateTime<Utc>>,
    pub members: sqlx::types::Json<Vec<ChatMemberInfo>>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ChatMemberInfo {
    pub user_id: Uuid,
    pub avatar: Option<String>,
    pub username: String,
    pub online: bool,
    pub created_at: DateTime<Utc>,
}
