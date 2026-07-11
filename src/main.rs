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
    logger::init_tracing();
    dotenvy::dotenv().ok();

    let jwt = jwt::JwtConfig::from_env();
    let pool = postgres::connect().await;
    let redis = redis::connect().await;

    let state = app::AppState {
        db: pool,
        redis,
        jwt_secret: jwt.secret,
        jwt_max_age: jwt.max_age,
    };

    let listener = tokio::net::TcpListener::bind("0.0.0.0:2026").await.unwrap();
    tracing::info!("Server is running on http://localhost:2026");
    axum::serve(listener, app::router::create_router(state)).await.unwrap();
}
