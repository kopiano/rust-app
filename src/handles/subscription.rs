use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Duration, Utc};
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
    pub payment_method: Option<String>,
    pub contact_email: Option<String>,
    pub currency: Option<String>,
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
    pub subscription_start_at: Option<DateTime<Utc>>,
    pub subscription_end_at: Option<DateTime<Utc>>,
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
    let payment_method = normalize_payment_method(input.payment_method.as_deref());
    let currency = normalize_currency(input.currency.as_deref());
    let contact_email = match normalize_contact_email(input.contact_email.as_deref()) {
        Ok(contact_email) => contact_email,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::error(400, message)),
            )
                .into_response();
        }
    };
    let checkout_url = match build_checkout_url(
        &state,
        user_id,
        &return_to,
        payment_method,
        contact_email.as_deref(),
        currency,
    ) {
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
    let event_status = input.status.trim().to_ascii_lowercase();
    let now = Utc::now();
    let (plan, subscription_status, subscription_start_at, subscription_end_at) =
        match event_status.as_str() {
            "active" | "paid" | "succeeded" => {
                let start_at = input.subscription_start_at.unwrap_or(now);
                let end_at = input
                    .subscription_end_at
                    .unwrap_or(start_at + Duration::days(30));
                (requested_plan, "active", Some(start_at), Some(end_at))
            }
            "canceled" | "cancelled" | "expired" => (
                "free".to_string(),
                "expired",
                input.subscription_start_at,
                Some(input.subscription_end_at.unwrap_or(now)),
            ),
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
        r#"UPDATE "user"
           SET plan = $2,
               subscription_status = $3,
               subscription_start_at = COALESCE($4, subscription_start_at),
               subscription_end_at = $5,
               updated_at = NOW()
           WHERE id = $1
           RETURNING id"#,
    )
    .bind(input.user_id)
    .bind(&plan)
    .bind(subscription_status)
    .bind(subscription_start_at)
    .bind(subscription_end_at)
    .fetch_optional(&state.db)
    .await
    {
        Ok(Some(_)) => Json(ApiResponse::success(serde_json::json!({
            "user_id": input.user_id,
            "plan": plan,
            "subscription_status": subscription_status,
            "subscription_start_at": subscription_start_at,
            "subscription_end_at": subscription_end_at,
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
    payment_method: &str,
    contact_email: Option<&str>,
    currency: &str,
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
        .append_pair("payment_method", payment_method)
        .append_pair("currency", currency)
        .append_pair("success_url", success_url.as_str())
        .append_pair("cancel_url", cancel_url.as_str());
    if let Some(contact_email) = contact_email {
        checkout_url
            .query_pairs_mut()
            .append_pair("contact_email", contact_email);
    }
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

fn normalize_payment_method(value: Option<&str>) -> &'static str {
    match value
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "alipay" => "alipay",
        "union_pay" => "union_pay",
        _ => "wechat_pay",
    }
}

fn normalize_currency(value: Option<&str>) -> &'static str {
    match value
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase()
        .as_str()
    {
        "USD" => "USD",
        _ => "CNY",
    }
}

fn normalize_contact_email(value: Option<&str>) -> Result<Option<String>, &'static str> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.len() > 254 || !value.contains('@') || value.contains(char::is_whitespace) {
        return Err("Invalid contact email.");
    }
    Ok(Some(value.to_string()))
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
    use super::{
        normalize_contact_email, normalize_currency, normalize_payment_method, normalize_return_to,
        valid_plan,
    };

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

    #[test]
    fn payment_method_is_limited_to_supported_channels() {
        assert_eq!(normalize_payment_method(Some("alipay")), "alipay");
        assert_eq!(normalize_payment_method(Some("union_pay")), "union_pay");
        assert_eq!(normalize_payment_method(Some("unknown")), "wechat_pay");
    }

    #[test]
    fn currency_is_limited_to_checkout_options() {
        assert_eq!(normalize_currency(Some("usd")), "USD");
        assert_eq!(normalize_currency(Some("unknown")), "CNY");
        assert_eq!(normalize_currency(None), "CNY");
    }

    #[test]
    fn contact_email_is_optional_and_validated() {
        assert_eq!(
            normalize_contact_email(Some(" listener@example.com ")).unwrap(),
            Some("listener@example.com".to_string())
        );
        assert_eq!(normalize_contact_email(None).unwrap(), None);
        assert!(normalize_contact_email(Some("not-an-email")).is_err());
    }
}
