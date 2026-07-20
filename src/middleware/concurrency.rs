use std::sync::atomic::Ordering;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::app::AppState;

pub async fn limit_http(State(state): State<AppState>, request: Request, next: Next) -> Response {
    state.metrics.http_total.fetch_add(1, Ordering::Relaxed);
    let permit = tokio::time::timeout(
        state.limits.http_queue_timeout,
        state.limits.http.clone().acquire_owned(),
    )
    .await;
    let _permit = match permit {
        Ok(Ok(permit)) => permit,
        Ok(Err(_)) | Err(_) => {
            state.metrics.http_rejected.fetch_add(1, Ordering::Relaxed);
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };

    state.metrics.http_active.fetch_add(1, Ordering::Relaxed);
    let response = next.run(request).await;
    state.metrics.http_active.fetch_sub(1, Ordering::Relaxed);
    response
}

pub async fn limit_upload(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let permit = tokio::time::timeout(
        state.limits.upload_queue_timeout,
        state.limits.upload.clone().acquire_owned(),
    )
    .await;
    let _permit = match permit {
        Ok(Ok(permit)) => permit,
        Ok(Err(_)) | Err(_) => {
            state
                .metrics
                .upload_rejected
                .fetch_add(1, Ordering::Relaxed);
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };

    state.metrics.upload_active.fetch_add(1, Ordering::Relaxed);
    let response = next.run(request).await;
    state.metrics.upload_active.fetch_sub(1, Ordering::Relaxed);
    response
}
