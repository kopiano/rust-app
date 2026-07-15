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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
