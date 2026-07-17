use axum::{extract::Request, middleware::Next, response::Response};
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
    let terminal_path = log_path(&path);
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
    match status {
        500..=u16::MAX => tracing::error!(
            target: "app::http",
            http = true,
            %method,
            %path,
            %terminal_path,
            status,
            latency_ms,
            user_id = user_id.as_deref().unwrap_or(""),
            %ip,
            %request_id,
            "HTTP request"
        ),
        400..=499 => tracing::warn!(
            target: "app::http",
            http = true,
            %method,
            %path,
            %terminal_path,
            status,
            latency_ms,
            user_id = user_id.as_deref().unwrap_or(""),
            %ip,
            %request_id,
            "HTTP request"
        ),
        _ => tracing::info!(
            target: "app::http",
            http = true,
            %method,
            %path,
            %terminal_path,
            status,
            latency_ms,
            user_id = user_id.as_deref().unwrap_or(""),
            %ip,
            %request_id,
            "HTTP request"
        ),
    }
    response
}

#[allow(dead_code)]
fn short_id(value: &str) -> String {
    let value = value.replace(['\r', '\n'], "");
    let prefix = value.split('-').next().unwrap_or(&value);
    format!("{}-***", prefix.chars().take(8).collect::<String>())
}

fn log_path(path: &str) -> String {
    if let Some(resource_path) = path.strip_prefix("/api/assets/music/")
        && let Some((music_id, filename)) = resource_path.split_once('/')
        && Uuid::parse_str(music_id).is_ok()
        && !filename.is_empty()
    {
        return format!("/api/assets/music/**/{filename}");
    }

    let Some(filename) = path.strip_prefix("/api/assets/avatar/") else {
        return path.to_owned();
    };
    let Some((stem, extension)) = filename.rsplit_once('.') else {
        return path.to_owned();
    };
    let Some((name, timestamp)) = stem.rsplit_once('-') else {
        return path.to_owned();
    };
    if timestamp.is_empty()
        || !timestamp
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return path.to_owned();
    }
    format!("/api/assets/avatar/{name}-*.{extension}")
}

#[cfg(test)]
mod tests {
    use super::log_path;

    #[test]
    fn replaces_avatar_timestamp_in_log_path() {
        assert_eq!(
            log_path("/api/assets/avatar/avatar-shville-1784093781489.webp"),
            "/api/assets/avatar/avatar-shville-*.webp"
        );
    }

    #[test]
    fn replaces_music_uuid_in_log_path() {
        assert_eq!(
            log_path("/api/assets/music/5ec5d7bd-196a-43cf-b58f-d17e976bade5/cover.webp"),
            "/api/assets/music/**/cover.webp"
        );
        assert_eq!(
            log_path("/api/assets/music/5ec5d7bd-196a-43cf-b58f-d17e976bade5/audio.m4a"),
            "/api/assets/music/**/audio.m4a"
        );
    }

    #[test]
    fn leaves_other_paths_unchanged() {
        assert_eq!(log_path("/api/users"), "/api/users");
        assert_eq!(
            log_path("/api/assets/avatar/avatar-shville.webp"),
            "/api/assets/avatar/avatar-shville.webp"
        );
        assert_eq!(
            log_path("/api/assets/music/not-a-uuid/cover.webp"),
            "/api/assets/music/not-a-uuid/cover.webp"
        );
    }
}
