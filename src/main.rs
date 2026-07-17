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
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("Database migration failed");
    let redis = redis::connect().await;
    tracing::info!(target: "app::redis", "Redis connected");
    let (message_tx, _) = tokio::sync::broadcast::channel(256);
    let (music_tx, _) = tokio::sync::broadcast::channel(256);
    // state
    let state = app::AppState {
        db: pool,
        redis,
        jwt_secret: jwt.secret,
        jwt_max_age: jwt.max_age,
        frontend_url: std::env::var("FRONTEND_URL")
            .unwrap_or_else(|_| "http://localhost:1420".to_string()),
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
        message_tx,
        music_tx,
    };

    // router
    let app = app::router::create_router(state);
    // port
    let port = std::env::var("PORT").unwrap_or_else(|_| "8100".to_owned());
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .unwrap();
    tracing::info!(target: "app::http", address = %format!("0.0.0.0:{port}"), "Listening");
    // run axum web server
    axum::serve(listener, app).await.unwrap();
}
