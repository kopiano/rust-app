use axum::{extract::Request, middleware::Next, response::Response};
use std::time::Instant;
use tracing::info;

pub async fn logger(req: Request, next: Next) -> Response {
    let method = req.method().to_string();
    let uri = req.uri().to_string();
    let start = Instant::now();

    let response = next.run(req).await;

    let status = response.status();
    let elapsed = start.elapsed();

    info!("{method} {uri} -> {status} ({elapsed:?})");

    response
}
