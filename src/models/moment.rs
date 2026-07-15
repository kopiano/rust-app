use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, types::Json};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Moment {
    pub id: Uuid,
    pub user_id: Uuid,
    pub content: Option<String>,
    pub media: Json<Vec<MomentMedia>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentMedia {
    #[serde(rename = "type")]
    pub media_type: String,
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateMoment {
    pub content: Option<String>,
    #[serde(default)]
    pub media: Vec<MomentMedia>,
}
