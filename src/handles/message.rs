use axum::{
    Extension, Json,
    extract::State,
    http::StatusCode,
};
use uuid::Uuid;

use crate::{
    app::AppState,
    middleware::jwt::Claims,
    models::message::MessageUserInfo,
};

pub async fn user_info(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Vec<MessageUserInfo>>, StatusCode> {
    let user_id = claims
        .sub
        .parse::<Uuid>()
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    let contacts = sqlx::query_as::<_, MessageUserInfo>(
        r##"
        WITH contacts AS (
            SELECT CASE
                WHEN send_id = $1 THEN receiver_id
                ELSE send_id
            END AS user_id
            FROM "message"
            WHERE chat_type = 'private'
              AND deleted_at IS NULL
              AND (send_id = $1 OR receiver_id = $1)
        ),
        unique_contacts AS (
            SELECT DISTINCT user_id
            FROM contacts
            WHERE user_id IS NOT NULL
        ),
        latest_messages AS (
            SELECT DISTINCT ON (CASE
                WHEN send_id = $1 THEN receiver_id
                ELSE send_id
            END)
                CASE
                    WHEN send_id = $1 THEN receiver_id
                    ELSE send_id
                END AS user_id,
                content,
                created_at AS last_message_time
            FROM "message"
            WHERE chat_type = 'private'
              AND deleted_at IS NULL
              AND (send_id = $1 OR receiver_id = $1)
            ORDER BY CASE
                WHEN send_id = $1 THEN receiver_id
                ELSE send_id
            END, created_at DESC
        )
        SELECT
            u.id AS user_id,
            u.avatar,
            u.name AS username,
            u.status,
            lm.content,
            lm.last_message_time
        FROM unique_contacts c
        JOIN "user" u ON u.id = c.user_id
        LEFT JOIN latest_messages lm ON lm.user_id = u.id
        ORDER BY lm.last_message_time DESC NULLS LAST
        "##,
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(contacts))
}
