use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Character {
    pub id: Uuid,
    pub name: String,
    pub avatar_url: Option<String>,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub voice_model: Option<String>,
    pub ckpt_path: Option<String>,
    pub pth_path: Option<String>,
    pub train_status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
