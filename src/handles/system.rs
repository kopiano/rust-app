use axum::{Json, extract::State};
use serde_json::{Value, json};

use crate::app::AppState;

pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

pub async fn metrics(State(state): State<AppState>) -> Json<Value> {
    let metrics = state.metrics.snapshot();
    Json(json!({
        "uptime_seconds": metrics.uptime_seconds,
        "http": {
            "total": metrics.http_total,
            "active": metrics.http_active,
            "rejected": metrics.http_rejected,
            "limit": state.limits.http_max,
        },
        "upload": {
            "active": metrics.upload_active,
            "rejected": metrics.upload_rejected,
            "limit": state.limits.upload_max,
        },
        "bcrypt": {
            "active": metrics.bcrypt_active,
            "rejected": metrics.bcrypt_rejected,
            "limit": state.limits.bcrypt_max,
        },
        "transcode": {
            "active": state.limits.transcode_max
                .saturating_sub(state.limits.transcode.available_permits()),
            "limit": state.limits.transcode_max,
        },
        "websocket": {
            "connected": state.message_hub.connected(),
            "dropped_connections": state.message_hub.dropped_connections(),
            "queue_capacity": state.message_hub.queue_capacity(),
        },
        "database": {
            "pool_size": state.db.size(),
            "idle_connections": state.db.num_idle(),
        }
    }))
}
