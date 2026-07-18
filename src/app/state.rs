use redis::aio::MultiplexedConnection;
use sqlx::PgPool;
use tokio::sync::broadcast;

use crate::models::{message::MessageBroadcast, music::MusicProcessingBroadcast};

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    #[allow(dead_code)]
    pub redis: MultiplexedConnection,
    pub jwt_secret: String,
    pub jwt_max_age: i64,
    pub frontend_url: String,
    pub github_client_id: String,
    pub github_client_secret: String,
    pub github_redirect_uri: String,
    #[allow(dead_code)]
    pub pro_checkout_url: Option<String>,
    pub subscription_webhook_secret: Option<String>,
    pub message_tx: broadcast::Sender<MessageBroadcast>,
    pub music_tx: broadcast::Sender<MusicProcessingBroadcast>,
}
