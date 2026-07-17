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
    pub processing_status: String,
    pub processing_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct MusicListItem {
    pub id: Uuid,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: i64,
    pub cover_url: String,
    pub is_favorite: bool,
    pub processing_status: String,
    pub processing_error: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MusicListPage {
    pub items: Vec<MusicListItem>,
    pub page: i64,
    pub page_size: i64,
    pub total: i64,
    pub total_pages: i64,
    pub total_duration_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MusicLibraryItem {
    pub collection: String,
    pub user_id: Uuid,
    pub username: String,
    pub avatar: Option<String>,
    pub playlist_name: String,
    pub track_count: i64,
    pub total_duration_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MusicProcessingBroadcast {
    #[serde(skip)]
    pub user_id: Uuid,
    pub event: &'static str,
    pub id: Uuid,
    pub status: String,
    #[allow(dead_code)]
    #[serde(skip)]
    pub audio_url: String,
    #[serde(serialize_with = "serialize_music_list_item")]
    pub music: Music,
}

fn serialize_music_list_item<S>(music: &Music, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    MusicListItem {
        id: music.id,
        title: music.title.clone(),
        artist: music.artist.clone(),
        album: music.album.clone(),
        duration_ms: music.duration_ms,
        cover_url: music.cover_url.clone(),
        is_favorite: music.is_favorite,
        processing_status: music.processing_status.clone(),
        processing_error: music.processing_error.clone(),
        created_at: music.created_at,
    }
    .serialize(serializer)
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

#[cfg(test)]
mod tests {
    use super::{Music, MusicProcessingBroadcast};
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn processing_event_does_not_serialize_audio_urls() {
        let now = Utc::now();
        let id = Uuid::new_v4();
        let event = MusicProcessingBroadcast {
            user_id: Uuid::new_v4(),
            event: "music.processing",
            id,
            status: "ready".to_string(),
            audio_url: "/api/assets/music/example/audio.m4a".to_string(),
            music: Music {
                id,
                title: "Example".to_string(),
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                duration_ms: 42_000,
                bitrate: 256_000,
                sample_rate: 44_100,
                cover_url: "/api/assets/music/example/cover.webp".to_string(),
                audio_url: "/api/assets/music/example/audio.m4a".to_string(),
                original_url: "/api/assets/music/example/original.ncm".to_string(),
                format: "m4a".to_string(),
                original_format: "ncm".to_string(),
                size: 1,
                original_size: 2,
                is_favorite: false,
                processing_status: "ready".to_string(),
                processing_error: None,
                created_at: now,
                updated_at: now,
            },
        };

        let payload = serde_json::to_value(event).expect("processing event should serialize");
        assert!(payload.get("audio_url").is_none());
        assert!(payload["music"].get("audio_url").is_none());
        assert!(payload["music"].get("original_url").is_none());
        assert_eq!(payload["music"]["duration_ms"], 42_000);
    }
}
