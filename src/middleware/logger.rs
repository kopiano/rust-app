use axum::{
    extract::Request,
    middleware::Next,
    response::Response,
};
use std::time::Instant;
use uuid::Uuid;

use crate::middleware::jwt::Claims;

pub async fn logger(req: Request, next: Next) -> Response {
    let request_id = req
        .headers()
        .get("X-Request-ID")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let method = req.method().as_str().to_owned();
    let path = req.uri().path().to_owned();
    let ip = req
        .headers()
        .get("CF-Connecting-IP")
        .or_else(|| req.headers().get("X-Real-IP"))
        .or_else(|| req.headers().get("X-Forwarded-For"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
        .to_owned();
    let start = Instant::now();

    let response = next.run(req).await;
    let status = response.status().as_u16();
    let latency_ms = start.elapsed().as_secs_f64() * 1000.0;
    let user_id = response
        .extensions()
        .get::<Claims>()
        .map(|claims| claims.sub.clone());
    let mut response = response;
    if let Ok(value) = request_id.parse() {
        response.headers_mut().insert("X-Request-ID", value);
    }
    let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S");
    let level = if status >= 500 {
        "ERROR"
    } else if status >= 400 {
        "WARN"
    } else {
        "INFO"
    };
    let identity = user_id
        .map(|id| format!(" user={id}"))
        .unwrap_or_default();
    println!(
        "[{timestamp}] {level:<5} {method:<6} {path:<36} ({status:<3}) {latency_ms:<4.0}ms ip={ip}{identity} request_id={request_id}"
    );
    response
}
