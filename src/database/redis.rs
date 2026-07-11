use redis::Client;
use redis::aio::MultiplexedConnection;

pub async fn connect() -> MultiplexedConnection {
    let redis_url = std::env::var("REDIS_URL").expect("REDIS_URL not found");
    Client::open(redis_url.as_str())
        .expect("Redis URL invalid")
        .get_multiplexed_tokio_connection()
        .await
        .expect("Redis connect failed")
}
