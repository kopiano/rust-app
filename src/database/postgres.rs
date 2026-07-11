use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub async fn connect() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL not found");

    PgPoolOptions::new()
        .max_connections(20)
        .connect(&database_url)
        .await
        .expect("PostgreSQL connect failed")
}
