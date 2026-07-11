use redis::aio::MultiplexedConnection;
use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    #[allow(dead_code)]
    pub redis: MultiplexedConnection,
    pub jwt_secret: String,
    pub jwt_max_age: i64,
}
