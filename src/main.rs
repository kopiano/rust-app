mod app;
mod common;
mod config;
mod database;
mod handles;
mod middleware;
mod models;
mod services;

use crate::config::{jwt, logger};
use crate::database::{postgres, redis};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    // .env
    dotenvy::dotenv().ok();
    // logger
    logger::init_tracing();
    tracing::info!(target: "app::server", "Server started");
    // jwt
    let jwt = jwt::JwtConfig::from_env();
    // postgresql, redis
    let pool = postgres::connect().await;
    tracing::info!(target: "app::db", "PostgreSQL connected");
    // Rebuild this migration bundle when migration files change.
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("Database migration failed");
    let redis = redis::connect().await;
    tracing::info!(target: "app::redis", "Redis connected");
    let (music_tx, _) = tokio::sync::broadcast::channel(256);
    let (video_tx, _) = tokio::sync::broadcast::channel(512);
    let limits = Arc::new(app::runtime::RuntimeLimits::from_env());
    let metrics = Arc::new(app::runtime::AppMetrics::default());
    let message_hub = Arc::new(services::message_hub::MessageHub::from_env());
    tracing::info!(
        target: "app::limits",
        http = limits.http_max,
        upload = limits.upload_max,
        bcrypt = limits.bcrypt_max,
        transcode = limits.transcode_max,
        websocket_queue = message_hub.queue_capacity(),
        "Runtime concurrency limits configured"
    );
    let allowed_llm_hosts = std::env::var("ALLOWED_LLM_HOSTS")
        .unwrap_or_else(|_| "api.deepseek.com".to_string())
        .split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_lowercase)
        .collect();
    let llm_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("Unable to build LLM HTTP client");
    let voice_training_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("Unable to build voice training HTTP client");
    // state
    let state = app::AppState {
        db: pool,
        redis,
        jwt_secret: jwt.secret,
        jwt_max_age: jwt.max_age,
        frontend_url: std::env::var("FRONTEND_URL")
            .unwrap_or_else(|_| "http://localhost:1420".to_string()),
        voice_api_url: std::env::var("VOICE_API_URL")
            .unwrap_or_else(|_| "http://localhost:8200".to_string()),
        voice_training_url: std::env::var("VOICE_TRAINING_URL")
            .unwrap_or_else(|_| "http://localhost:9881".to_string()),
        voice_training_token: std::env::var("VOICE_TRAINING_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        voice_train_dir: std::env::var("VOICE_TRAIN_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("src/assets/train")),
        voice_training_client,
        llm_client,
        allowed_llm_hosts: Arc::new(allowed_llm_hosts),
        character_system_prompt: std::env::var("CHARACTER_SYSTEM_PROMPT").unwrap_or_else(|_| {
            "Stay in character. Reply naturally and concisely in the user's language.".to_string()
        }),
        github_client_id: std::env::var("GITHUB_CLIENT_ID").expect("GITHUB_CLIENT_ID not found"),
        github_client_secret: std::env::var("GITHUB_CLIENT_SECRET")
            .expect("GITHUB_CLIENT_SECRET not found"),
        github_redirect_uri: std::env::var("GITHUB_REDIRECT_URI")
            .unwrap_or_else(|_| "http://localhost:8100/api/auth/github/callback".to_string()),
        pro_checkout_url: std::env::var("PRO_CHECKOUT_URL")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        subscription_webhook_secret: std::env::var("SUBSCRIPTION_WEBHOOK_SECRET")
            .ok()
            .filter(|value| !value.trim().is_empty()),
        message_hub,
        music_tx,
        video_tx,
        limits,
        metrics,
    };
    handles::voice_training::resume_pending_jobs(&state).await;
    handles::subscription::reconcile_pending_payment_reviewer_notifications(&state).await;

    // router
    let app = app::router::create_router(state);
    // port
    let port = std::env::var("PORT")
        .unwrap_or_else(|_| "8100".to_owned())
        .parse::<u16>()
        .expect("PORT must be a valid TCP port");
    let address = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&address).await.unwrap();
    tracing::info!(target: "app::http", %address, "Listening");
    // run axum web server
    axum::serve(listener, app).await.unwrap();
}
