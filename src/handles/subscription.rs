use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app::AppState;
use crate::common::response::ApiResponse;
use crate::middleware::jwt::Claims;

const WEBHOOK_SECRET_HEADER: &str = "x-subscription-webhook-secret";

#[derive(Debug, Deserialize)]
pub struct CheckoutInput {
    pub return_to: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CheckoutResponse {
    pub checkout_url: String,
}

#[derive(Debug, Deserialize)]
pub struct SubscriptionWebhookInput {
    pub user_id: Uuid,
    pub plan: String,
    pub status: String,
}

pub async fn create_pro_checkout(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(input): Json<CheckoutInput>,
) -> Response {
    let user_id = match claims.sub.parse::<Uuid>() {
        Ok(user_id) => user_id,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    let return_to = normalize_return_to(input.return_to.as_deref());
    let checkout_url = match build_checkout_url(&state, user_id, &return_to) {
        Ok(url) => url,
        Err((status, message)) => {
            return (
                status,
                Json(ApiResponse::<()>::error(status.as_u16(), message)),
            )
                .into_response();
        }
    };

    Json(ApiResponse::success(CheckoutResponse { checkout_url })).into_response()
}

pub async fn webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<SubscriptionWebhookInput>,
) -> Response {
    let configured_secret = match state.subscription_webhook_secret.as_deref() {
        Some(secret) => secret,
        None => {
            tracing::error!("SUBSCRIPTION_WEBHOOK_SECRET is not configured");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let supplied_secret = headers
        .get(WEBHOOK_SECRET_HEADER)
        .and_then(|value| value.to_str().ok());
    if supplied_secret != Some(configured_secret) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let requested_plan = input.plan.trim().to_ascii_lowercase();
    if !valid_plan(&requested_plan) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::error(400, "Invalid subscription plan.")),
        )
            .into_response();
    }
    let status = input.status.trim().to_ascii_lowercase();
    let plan = match status.as_str() {
        "active" | "paid" | "succeeded" => requested_plan,
        "canceled" | "cancelled" | "expired" => "free".to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::error(
                    400,
                    "Unsupported subscription status.",
                )),
            )
                .into_response();
        }
    };

    match sqlx::query(
        r#"UPDATE "user" SET plan = $2, updated_at = NOW() WHERE id = $1 RETURNING id"#,
    )
    .bind(input.user_id)
    .bind(&plan)
    .fetch_optional(&state.db)
    .await
    {
        Ok(Some(_)) => Json(ApiResponse::success(serde_json::json!({
            "user_id": input.user_id,
            "plan": plan,
        })))
        .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!(%error, user_id = %input.user_id, "Failed to update subscription");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn build_checkout_url(
    state: &AppState,
    user_id: Uuid,
    return_to: &str,
) -> Result<String, (StatusCode, &'static str)> {
    let configured_url = state.pro_checkout_url.as_deref().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "Pro checkout is not configured.",
    ))?;
    let mut checkout_url = Url::parse(configured_url)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Invalid checkout URL."))?;
    if !matches!(checkout_url.scheme(), "http" | "https") {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "Invalid checkout URL."));
    }

    let mut success_url = frontend_url(&state.frontend_url, "/subscription/success")?;
    success_url
        .query_pairs_mut()
        .append_pair("return_to", return_to);
    let cancel_url = frontend_url(&state.frontend_url, return_to)?;
    checkout_url
        .query_pairs_mut()
        .append_pair("user_id", &user_id.to_string())
        .append_pair("plan", "pro")
        .append_pair("success_url", success_url.as_str())
        .append_pair("cancel_url", cancel_url.as_str());
    Ok(checkout_url.to_string())
}

fn frontend_url(configured_frontend: &str, path: &str) -> Result<Url, (StatusCode, &'static str)> {
    let mut url = Url::parse(configured_frontend)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Invalid frontend URL."))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "Invalid frontend URL."));
    }
    let parsed = Url::parse(&format!("http://local{path}"))
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid return path."))?;
    url.set_path(parsed.path());
    url.set_query(parsed.query());
    url.set_fragment(parsed.fragment());
    Ok(url)
}

fn normalize_return_to(value: Option<&str>) -> String {
    let value = value.unwrap_or("/music").trim();
    if value.starts_with('/') && !value.starts_with("//") && !value.contains('\\') {
        value.to_string()
    } else {
        "/music".to_string()
    }
}

fn valid_plan(plan: &str) -> bool {
    !plan.is_empty()
        && plan.len() <= 20
        && plan.bytes().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == b'-'
        })
}

#[cfg(test)]
mod tests {
    use super::{normalize_return_to, valid_plan};

    #[test]
    fn return_path_rejects_external_urls() {
        assert_eq!(
            normalize_return_to(Some("/music?view=library")),
            "/music?view=library"
        );
        assert_eq!(normalize_return_to(Some("https://example.com")), "/music");
        assert_eq!(normalize_return_to(Some("//example.com")), "/music");
    }

    #[test]
    fn plans_are_short_lowercase_identifiers() {
        assert!(valid_plan("free"));
        assert!(valid_plan("team-plus"));
        assert!(!valid_plan("Pro"));
        assert!(!valid_plan(""));
        assert!(!valid_plan("plan_with_underscore"));
    }
}
