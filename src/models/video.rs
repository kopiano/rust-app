use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, types::Json};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct VideoCategory {
    pub id: Uuid,
    pub slug: String,
    pub name_zh: String,
    pub name_en: String,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Video {
    pub id: Uuid,
    pub user_id: Uuid,
    pub username: String,
    pub avatar: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub cover_url: String,
    pub duration: i32,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub fps: Option<f64>,
    pub size: Option<i64>,
    pub origin_file_url: Option<String>,
    pub hls_master_url: Option<String>,
    pub status: String,
    pub visibility: String,
    pub processing_progress: i16,
    pub processing_error: Option<String>,
    pub view_count: i64,
    pub like_count: i64,
    pub comment_count: i64,
    pub favorite_count: i64,
    pub liked: bool,
    pub favorited: bool,
    pub owned: bool,
    pub categories: Json<Vec<VideoCategory>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct VideoListPage {
    pub items: Vec<Video>,
    pub has_more: bool,
    pub next_before_created_at: Option<DateTime<Utc>>,
    pub next_before_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct VideoComment {
    pub id: Uuid,
    pub video_id: Uuid,
    pub user_id: Uuid,
    pub username: String,
    pub avatar: Option<String>,
    pub parent_id: Option<Uuid>,
    pub reply_to_user_id: Option<Uuid>,
    pub reply_to_username: Option<String>,
    pub content: String,
    pub like_count: i64,
    pub liked: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct VideoReactionState {
    pub video_id: Uuid,
    pub active: bool,
    pub count: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct VideoCommentLikeState {
    pub comment_id: Uuid,
    pub liked: bool,
    pub like_count: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct VideoViewState {
    pub video_id: Uuid,
    pub counted: bool,
    pub view_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct VideoUploadSession {
    pub upload_id: Uuid,
    pub video: Video,
    pub chunk_size: u64,
    pub uploaded_bytes: u64,
    pub total_bytes: u64,
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct VideoCollection {
    pub id: Uuid,
    pub user_id: Uuid,
    pub username: String,
    pub avatar: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub visibility: String,
    pub video_count: i64,
    pub total_views: i64,
    pub cover_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateVideoComment {
    pub content: String,
    pub parent_id: Option<Uuid>,
    pub reply_to_user_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct CreateVideoCollection {
    pub title: String,
    pub description: Option<String>,
    pub visibility: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateVideoCollection {
    pub title: Option<String>,
    pub description: Option<String>,
    pub visibility: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddVideoCollectionItem {
    pub video_id: Uuid,
    pub position: Option<i32>,
}
