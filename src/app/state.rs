use redis::aio::MultiplexedConnection;
use sqlx::PgPool;

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
}
