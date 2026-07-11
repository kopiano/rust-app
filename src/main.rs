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
    let redis = redis::connect().await;
    // state
    let state = app::AppState {
        db: pool,
        redis,
        jwt_secret: jwt.secret,
        jwt_max_age: jwt.max_age,
    };

    // router
    let app = app::router::create_router(state);
    // port
    let listener = tokio::net::TcpListener::bind("0.0.0.0:2026").await.unwrap();
    tracing::info!("Server is running on http://localhost:2026");
    // run axum web server
    axum::serve(listener, app).await.unwrap();
}
