use axum::{
    Json,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::app::AppState;
use crate::common::response::ApiResponse;
use crate::middleware::jwt::Claims;

pub fn has_library_access(
    plan: &str,
    subscription_status: &str,
    subscription_end_at: Option<DateTime<Utc>>,
) -> bool {
    let paid_plan = plan.trim().eq_ignore_ascii_case("pro");
    let active = subscription_status.trim().eq_ignore_ascii_case("active");
    let not_expired = subscription_end_at.is_none_or(|end_at| end_at > Utc::now());
    paid_plan && active && not_expired
}

fn requested_library_owner(query: Option<&str>) -> Option<Uuid> {
    query?.split('&').find_map(|parameter| {
        let (key, value) = parameter.split_once('=')?;
        (key == "user_id")
            .then(|| Uuid::parse_str(value).ok())
            .flatten()
    })
}

pub async fn require_library_access(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let user_id = match req
        .extensions()
        .get::<Claims>()
        .and_then(|claims| claims.sub.parse::<Uuid>().ok())
    {
        Some(user_id) => user_id,
        None => return StatusCode::UNAUTHORIZED.into_response(),
    };

    let requested_owner = requested_library_owner(req.uri().query());
    if requested_owner.is_none() || requested_owner == Some(user_id) {
        return next.run(req).await;
    }

    let subscription = match sqlx::query_as::<_, (String, String, Option<DateTime<Utc>>)>(
        r#"SELECT plan, subscription_status, subscription_end_at FROM "user" WHERE id = $1"#,
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(Some(subscription)) => subscription,
        Ok(None) => return StatusCode::UNAUTHORIZED.into_response(),
        Err(error) => {
            tracing::error!(%error, %user_id, "Failed to check library plan");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    if !has_library_access(&subscription.0, &subscription.1, subscription.2) {
        return (
            StatusCode::FORBIDDEN,
            Json(ApiResponse::<()>::error(
                StatusCode::FORBIDDEN.as_u16(),
                "A Pro subscription is required to access the music library.",
            )),
        )
            .into_response();
    }

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::{has_library_access, requested_library_owner};
    use chrono::{Duration, Utc};
    use uuid::Uuid;

    #[test]
    fn library_access_requires_an_active_pro_plan() {
        assert!(has_library_access("pro", "active", None));
        assert!(has_library_access(
            "PRO",
            "active",
            Some(Utc::now() + Duration::days(1))
        ));
        assert!(!has_library_access("plus", "active", None));
        assert!(!has_library_access("free", "active", None));
        assert!(!has_library_access("pro", "", None));
        assert!(!has_library_access("pro", "expired", None));
        assert!(!has_library_access(
            "pro",
            "active",
            Some(Utc::now() - Duration::days(1))
        ));
    }

    #[test]
    fn library_owner_is_read_from_query() {
        let user_id = Uuid::new_v4();
        let query = format!("page=1&user_id={user_id}&collection=uploads");

        assert_eq!(requested_library_owner(Some(&query)), Some(user_id));
        assert_eq!(requested_library_owner(Some("page=1")), None);
        assert_eq!(requested_library_owner(Some("user_id=invalid")), None);
    }
}
