use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, types::Json};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Moment {
    pub id: Uuid,
    pub user_id: Uuid,
    pub username: String,
    pub avatar: Option<String>,
    pub content: Option<String>,
    pub media: Json<Vec<MomentMedia>>,
    pub processing_status: String,
    pub processing_progress: i16,
    pub processing_error: Option<String>,
    pub like_count: i64,
    pub comment_count: i64,
    pub view_count: i64,
    pub liked: bool,
    pub comments: Json<Vec<MomentComment>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct MomentComment {
    pub id: Uuid,
    pub moment_id: Uuid,
    pub user_id: Uuid,
    pub username: String,
    pub avatar: Option<String>,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct MomentLikeState {
    pub moment_id: Uuid,
    pub liked: bool,
    pub like_count: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct MomentViewState {
    pub moment_id: Uuid,
    pub counted: bool,
    pub like_count: i64,
    pub comment_count: i64,
    pub view_count: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateMomentComment {
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentMedia {
    #[serde(rename = "type")]
    pub media_type: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poster_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct CreateMoment {
    pub content: Option<String>,
    #[serde(default)]
    pub media: Vec<MomentMedia>,
}
