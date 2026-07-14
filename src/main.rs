mod app;
mod config;
mod database;
mod handles;
mod middleware;
mod models;

use crate::config::{jwt, logger};
use crate::database::{postgres, redis};

#[tokio::main]
async fn main() {
    // logger
    logger::init_tracing();
    // .env
    dotenvy::dotenv().ok();
    // jwt
    let jwt = jwt::JwtConfig::from_env();
    // postgresql, redis
    let pool = postgres::connect().await;
    sqlx::migrate!("./migrations").run(&pool).await.expect("Database migration failed");
    let redis = redis::connect().await;
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
    };

    // router
    let app = app::router::create_router(state);
    // port
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8100").await.unwrap();
    tracing::info!("Server is running on http://localhost:8100");
    // run axum web server
    axum::serve(listener, app).await.unwrap();
}
