use redis::aio::MultiplexedConnection;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::{
    app::runtime::{AppMetrics, RuntimeLimits},
    models::music::MusicProcessingBroadcast,
    services::message_hub::MessageHub,
};

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
    pub message_hub: Arc<MessageHub>,
    pub music_tx: broadcast::Sender<MusicProcessingBroadcast>,
    pub limits: Arc<RuntimeLimits>,
    pub metrics: Arc<AppMetrics>,
}
