use axum::{
    Extension, Json,
    extract::State,
    http::StatusCode,
};
use uuid::Uuid;

use crate::{
    app::AppState,
    common::response::ApiResponse,
    middleware::jwt::Claims,
    models::message::MessageUserInfo,
};

pub async fn user_info(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<ApiResponse<Vec<MessageUserInfo>>>, StatusCode> {
    let user_id = claims
        .sub
        .parse::<Uuid>()
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    let contacts = sqlx::query_as::<_, MessageUserInfo>(
        r##"
        WITH private_latest AS (
            SELECT DISTINCT ON (
                CASE
                    WHEN send_id = $1 THEN receiver_id
                    ELSE send_id
                END
            )
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
            ORDER BY
                CASE
                    WHEN send_id = $1 THEN receiver_id
                    ELSE send_id
                END,
                created_at DESC,
                id DESC
        ),
        group_latest AS (
            SELECT DISTINCT ON (m.group_id)
                m.group_id AS contact_id,
                m.content,
                m.created_at AS last_message_time
            FROM "message" m
            JOIN group_member cm ON cm.group_id = m.group_id
            WHERE m.group_id IS NOT NULL
              AND cm.user_id = $1
              AND m.deleted_at IS NULL
            ORDER BY m.group_id, m.created_at DESC, m.id DESC
        )
        SELECT
            u.id AS user_id,
            NULL::uuid AS group_id,
            'private' AS chat_type,
            u.avatar,
            u.name AS username,
            u.status,
            pl.content,
            pl.last_message_time,
            '[]'::jsonb AS members
        FROM "user" u
        LEFT JOIN private_latest pl ON pl.user_id = u.id
        WHERE u.id <> $1

        UNION ALL

        SELECT
            NULL::uuid AS user_id,
            c.id AS group_id,
            'public' AS chat_type,
            c.avatar,
            c.name AS username,
            NULL::boolean AS status,
            gl.content,
            gl.last_message_time,
            COALESCE((
                SELECT jsonb_agg(jsonb_build_object(
                    'user_id', u.id,
                    'avatar', u.avatar,
                    'username', u.name,
                    'status', u.status
                ) ORDER BY u.name)
                FROM group_member cm
                JOIN "user" u ON u.id = cm.user_id
                WHERE cm.group_id = c.id
            ), '[]'::jsonb) AS members
        FROM group_member gm
        JOIN "group" c ON c.id = gm.group_id
        LEFT JOIN group_latest gl ON gl.contact_id = c.id
        WHERE gm.user_id = $1
        ORDER BY last_message_time DESC NULLS LAST
        "##,
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, %user_id, "Failed to load message contacts");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(ApiResponse::success(contacts)))
}
