use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Music {
    pub id: Uuid,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: i64,
    pub bitrate: i32,
    pub sample_rate: i32,
    pub cover_url: String,
    pub audio_url: String,
    pub original_url: String,
    pub format: String,
    pub original_format: String,
    pub size: i64,
    pub original_size: i64,
    pub is_favorite: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMusicFavorite {
    pub favorite: bool,
}

#[derive(Debug, Serialize, FromRow)]
pub struct MusicFavoriteState {
    pub id: Uuid,
    pub is_favorite: bool,
}
