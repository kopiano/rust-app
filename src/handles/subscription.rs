use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Duration, Utc};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::app::AppState;
use crate::common::response::ApiResponse;
use crate::middleware::jwt::Claims;
use crate::models::message::{Message, MessageBroadcast};

const WEBHOOK_SECRET_HEADER: &str = "x-subscription-webhook-secret";
const SYSTEM_USER_EMAIL: &str = "system@internal.local";

#[derive(Debug, Deserialize)]
pub struct CheckoutInput {
    pub return_to: Option<String>,
    pub payment_method: Option<String>,
    pub contact_email: Option<String>,
    pub currency: Option<String>,
    pub billing_cycle: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CheckoutResponse {
    pub order_no: String,
    pub status: String,
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
    let payment_method = normalize_payment_method(input.payment_method.as_deref());
    let currency = normalize_currency(input.currency.as_deref());
    let billing_cycle = normalize_billing_cycle(input.billing_cycle.as_deref());
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

    let mut transaction = match state.db.begin().await {
        Ok(transaction) => transaction,
        Err(error) => {
            tracing::error!(%error, "Failed to start payment order transaction");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::error(
                    500,
                    "Unable to create payment order.",
                )),
            )
                .into_response();
        }
    };
    let system_user_id = match system_user_id(&mut transaction).await {
        Ok(system_user_id) => system_user_id,
        Err(error) => {
            tracing::error!(%error, "System notification account is missing");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ApiResponse::<()>::error(
                    503,
                    "System notifications are not configured.",
                )),
            )
                .into_response();
        }
    };
    let administrators = match admin_users(&mut transaction).await {
        Ok(administrators) if !administrators.is_empty() => administrators,
        Ok(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ApiResponse::<()>::error(
                    503,
                    "No payment administrator is configured.",
                )),
            )
                .into_response();
        }
        Err(error) => {
            tracing::error!(%error, "Failed to load payment administrators");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let purchaser = match sqlx::query_as::<_, (String, String)>(
        r#"SELECT name, email FROM "user" WHERE id = $1"#,
    )
    .bind(user_id)
    .fetch_optional(&mut *transaction)
    .await
    {
        Ok(Some(purchaser)) => purchaser,
        Ok(None) => return StatusCode::UNAUTHORIZED.into_response(),
        Err(error) => {
            tracing::error!(%error, %user_id, "Failed to load payment user");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let order_no = create_order_no();
    let amount = price_for(currency, billing_cycle);
    let order_created = sqlx::query(
        r#"
        INSERT INTO subscription_order (
            order_no, user_id, plan, billing_cycle, payment_method, currency, amount, contact_email
        )
        VALUES ($1, $2, 'pro', $3, $4, $5, $6::numeric, $7)
        "#,
    )
    .bind(&order_no)
    .bind(user_id)
    .bind(billing_cycle)
    .bind(payment_method)
    .bind(currency)
    .bind(amount)
    .bind(contact_email.as_deref())
    .execute(&mut *transaction)
    .await;
    if let Err(error) = order_created {
        tracing::error!(%error, %user_id, "Failed to create payment order");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let user_content = format!(
        "你的 Pro 支付申请已提交，订单号：{order_no}，金额：{currency} {amount}。请完成支付，管理员确认到账后将为你开通 Pro 权限。"
    );
    let mut broadcasts = Vec::with_capacity(administrators.len() + 1);
    match insert_system_private_message(
        &mut transaction,
        system_user_id,
        user_id,
        &order_no,
        "purchaser",
        user_content,
    )
    .await
    {
        Ok(message) => broadcasts.push((message, vec![system_user_id, user_id])),
        Err(error) => {
            tracing::error!(%error, %order_no, "Failed to notify payment user");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    for (admin_id, admin_name) in administrators {
        let admin_content = format!(
            "待确认 Pro 支付\n用户：{} ({})\n订单号：{order_no}\n套餐：Pro {}\n支付方式：{}\n金额：{} {}\n请在确认到账后发放权限。",
            purchaser.0,
            purchaser.1,
            billing_cycle_label(billing_cycle),
            payment_method_label(payment_method),
            currency,
            amount,
        );
        match insert_system_private_message(
            &mut transaction,
            system_user_id,
            admin_id,
            &order_no,
            &format!("admin:{admin_name}"),
            admin_content,
        )
        .await
        {
            Ok(message) => broadcasts.push((message, vec![system_user_id, admin_id])),
            Err(error) => {
                tracing::error!(%error, %order_no, "Failed to notify payment administrator");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    }
    if let Err(error) = transaction.commit().await {
        tracing::error!(%error, %order_no, "Failed to commit payment order");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    broadcast_messages(&state, broadcasts);

    Json(ApiResponse::success(CheckoutResponse {
        order_no,
        status: "pending_confirmation".to_string(),
    }))
    .into_response()
}

pub async fn confirm_order(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(order_no): Path<String>,
) -> Response {
    let administrator_id = match claims.sub.parse::<Uuid>() {
        Ok(user_id) => user_id,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    if !valid_order_no(&order_no) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let is_admin =
        match sqlx::query_scalar::<_, bool>(r#"SELECT is_admin FROM "user" WHERE id = $1"#)
            .bind(administrator_id)
            .fetch_optional(&state.db)
            .await
        {
            Ok(Some(is_admin)) => is_admin,
            Ok(None) => return StatusCode::UNAUTHORIZED.into_response(),
            Err(error) => {
                tracing::error!(%error, %administrator_id, "Failed to check payment administrator");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };
    if !is_admin {
        return StatusCode::FORBIDDEN.into_response();
    }

    let mut transaction = match state.db.begin().await {
        Ok(transaction) => transaction,
        Err(error) => {
            tracing::error!(%error, "Failed to start payment confirmation transaction");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let order = match sqlx::query_as::<_, (Uuid, String)>(
        r#"
        SELECT user_id, billing_cycle
        FROM subscription_order
        WHERE order_no = $1 AND status = 'pending_confirmation'
        FOR UPDATE
        "#,
    )
    .bind(&order_no)
    .fetch_optional(&mut *transaction)
    .await
    {
        Ok(Some(order)) => order,
        Ok(None) => return StatusCode::CONFLICT.into_response(),
        Err(error) => {
            tracing::error!(%error, %order_no, "Failed to load payment order");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let subscription_end_at = Utc::now()
        + if order.1 == "yearly" {
            Duration::days(365)
        } else {
            Duration::days(30)
        };
    if let Err(error) = sqlx::query(
        r#"
        UPDATE "user"
        SET plan = 'pro',
            subscription_status = 'active',
            subscription_start_at = NOW(),
            subscription_end_at = $2,
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(order.0)
    .bind(subscription_end_at)
    .execute(&mut *transaction)
    .await
    {
        tracing::error!(%error, %order_no, "Failed to activate Pro subscription");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if let Err(error) = sqlx::query(
        r#"
        UPDATE subscription_order
        SET status = 'succeeded',
            confirmed_by = $2,
            confirmed_at = NOW(),
            updated_at = NOW()
        WHERE order_no = $1
        "#,
    )
    .bind(&order_no)
    .bind(administrator_id)
    .execute(&mut *transaction)
    .await
    {
        tracing::error!(%error, %order_no, "Failed to confirm payment order");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let system_user_id = match system_user_id(&mut transaction).await {
        Ok(system_user_id) => system_user_id,
        Err(error) => {
            tracing::error!(%error, "System notification account is missing");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let message = match insert_system_private_message(
        &mut transaction,
        system_user_id,
        order.0,
        &order_no,
        "confirmed",
        format!("你的 Pro 支付已确认，权限现已开通。订单号：{order_no}。"),
    )
    .await
    {
        Ok(message) => message,
        Err(error) => {
            tracing::error!(%error, %order_no, "Failed to send payment confirmation message");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if let Err(error) = transaction.commit().await {
        tracing::error!(%error, %order_no, "Failed to commit payment confirmation");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    broadcast_messages(&state, vec![(message, vec![system_user_id, order.0])]);
    Json(ApiResponse::success(serde_json::json!({
        "order_no": order_no,
        "status": "succeeded",
        "subscription_end_at": subscription_end_at,
    })))
    .into_response()
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

async fn system_user_id(transaction: &mut Transaction<'_, Postgres>) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(r#"SELECT id FROM "user" WHERE email = $1"#)
        .bind(SYSTEM_USER_EMAIL)
        .fetch_one(&mut **transaction)
        .await
}

async fn admin_users(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<Vec<(Uuid, String)>, sqlx::Error> {
    sqlx::query_as(r#"SELECT id, name FROM "user" WHERE is_admin = TRUE ORDER BY name"#)
        .fetch_all(&mut **transaction)
        .await
}

async fn insert_system_private_message(
    transaction: &mut Transaction<'_, Postgres>,
    system_user_id: Uuid,
    receiver_id: Uuid,
    order_no: &str,
    notification_kind: &str,
    content: String,
) -> Result<Message, sqlx::Error> {
    let (first, second) = if system_user_id.as_bytes() <= receiver_id.as_bytes() {
        (system_user_id, receiver_id)
    } else {
        (receiver_id, system_user_id)
    };
    let conversation_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("private:{first}:{second}").as_bytes(),
    );
    let client_message_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("subscription:{order_no}:{notification_kind}:{receiver_id}").as_bytes(),
    );

    sqlx::query_as::<_, Message>(
        r#"
        INSERT INTO "message" (
            conversation_id, chat_type, send_id, client_message_id,
            receiver_id, content, message_type, status
        )
        VALUES ($1, 'private', $2, $3, $4, $5, 3, 'sent')
        ON CONFLICT (send_id, client_message_id) DO UPDATE
            SET id = "message".id
        RETURNING id, conversation_id, chat_type, send_id, receiver_id, group_id,
                  client_message_id, content, message_type, status, created_at, update_at, deleted_at,
                  file_name, file_url
        "#,
    )
    .bind(conversation_id)
    .bind(system_user_id)
    .bind(client_message_id)
    .bind(receiver_id)
    .bind(content)
    .fetch_one(&mut **transaction)
    .await
}

fn broadcast_messages(state: &AppState, messages: Vec<(Message, Vec<Uuid>)>) {
    for (message, recipients) in messages {
        let _ = state.message_tx.send(MessageBroadcast {
            event: "message",
            message,
            recipients,
        });
    }
}

fn create_order_no() -> String {
    format!(
        "PRO{}{}",
        Utc::now().format("%Y%m%d%H%M%S"),
        Uuid::new_v4().simple()
    )
}

fn price_for(currency: &str, billing_cycle: &str) -> &'static str {
    match (currency, billing_cycle) {
        ("USD", "yearly") => "0.1470",
        ("USD", _) => "0.0147",
        ("CNY", "yearly") => "1.0000",
        _ => "0.1000",
    }
}

fn billing_cycle_label(cycle: &str) -> &'static str {
    if cycle == "yearly" {
        "年付"
    } else {
        "月付"
    }
}

fn payment_method_label(payment_method: &str) -> &'static str {
    match payment_method {
        "alipay" => "支付宝",
        "union_pay" => "云闪付",
        _ => "微信支付",
    }
}

fn valid_order_no(order_no: &str) -> bool {
    !order_no.is_empty()
        && order_no.len() <= 64
        && order_no
            .bytes()
            .all(|character| character.is_ascii_alphanumeric() || character == b'-')
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

fn normalize_billing_cycle(value: Option<&str>) -> &'static str {
    match value
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "yearly" => "yearly",
        _ => "monthly",
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
        normalize_billing_cycle, normalize_contact_email, normalize_currency,
        normalize_payment_method, normalize_return_to, valid_order_no, valid_plan,
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
    fn billing_cycle_is_limited_to_checkout_options() {
        assert_eq!(normalize_billing_cycle(Some("yearly")), "yearly");
        assert_eq!(normalize_billing_cycle(Some("monthly")), "monthly");
        assert_eq!(normalize_billing_cycle(Some("unknown")), "monthly");
    }

    #[test]
    fn order_number_has_a_conservative_character_set() {
        assert!(valid_order_no("PRO20260717123000a1b2"));
        assert!(!valid_order_no(""));
        assert!(!valid_order_no("../../order"));
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
