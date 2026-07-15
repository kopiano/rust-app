use axum::{
    Extension, Json,
    extract::{Query, State},
    http::StatusCode,
    extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
    response::IntoResponse,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    app::AppState,
    common::response::ApiResponse,
    middleware::jwt::Claims,
    models::message::{Message, MessageBroadcast, MessageUserInfo, SendMessageRequest},
};

pub async fn send(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(input): Json<SendMessageRequest>,
) -> Result<Json<ApiResponse<Message>>, StatusCode> {
    let sender_id = claims
        .sub
        .parse::<Uuid>()
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let content = input.content.trim().to_string();
    if content.is_empty() || content.chars().count() > 10_000 {
        return Err(StatusCode::BAD_REQUEST);
    }

    let message_type = input.message_type.unwrap_or(1);
    if !(1..=3).contains(&message_type) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let (receiver_id, group_id, recipients, conversation_id) = match input.chat_type.as_str() {
        "private" => {
            let receiver_id = input.receiver_id.ok_or(StatusCode::BAD_REQUEST)?;
            if receiver_id == sender_id || input.group_id.is_some() {
                return Err(StatusCode::BAD_REQUEST);
            }

            let exists = sqlx::query_scalar::<_, bool>(
                r#"SELECT EXISTS(SELECT 1 FROM "user" WHERE id = $1)"#,
            )
            .bind(receiver_id)
            .fetch_one(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            if !exists {
                return Err(StatusCode::NOT_FOUND);
            }

            let (first, second) = if sender_id.as_bytes() <= receiver_id.as_bytes() {
                (sender_id, receiver_id)
            } else {
                (receiver_id, sender_id)
            };
            let conversation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("private:{first}:{second}").as_bytes(),
            );
            (Some(receiver_id), None, vec![sender_id, receiver_id], conversation_id)
        }
        "public" => {
            let group_id = input.group_id.ok_or(StatusCode::BAD_REQUEST)?;
            if input.receiver_id.is_some() {
                return Err(StatusCode::BAD_REQUEST);
            }

            let recipients = sqlx::query_scalar::<_, Uuid>(
                "SELECT user_id FROM group_member WHERE group_id = $1",
            )
            .bind(group_id)
            .fetch_all(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            if !recipients.contains(&sender_id) {
                return Err(StatusCode::FORBIDDEN);
            }
            (None, Some(group_id), recipients, group_id)
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let message = sqlx::query_as::<_, Message>(
        r#"
        INSERT INTO "message" (
            conversation_id, chat_type, send_id, client_message_id,
            receiver_id, group_id, content, message_type, status
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'sent')
        ON CONFLICT (send_id, client_message_id) DO UPDATE
            SET id = "message".id
        RETURNING id, conversation_id, chat_type, send_id, receiver_id, group_id,
                  client_message_id, content, message_type, status, created_at, update_at, deleted_at,
                  file_name, file_url
        "#,
    )
    .bind(conversation_id)
    .bind(&input.chat_type)
    .bind(sender_id)
    .bind(input.client_message_id)
    .bind(receiver_id)
    .bind(group_id)
    .bind(content)
    .bind(message_type)
    .fetch_one(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, %sender_id, "Failed to send message");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let _ = state.message_tx.send(MessageBroadcast {
        event: "message",
        message: message.clone(),
        recipients,
    });

    Ok(Json(ApiResponse::success(message)))
}

#[derive(Debug, Deserialize)]
pub struct MessageHistoryQuery {
    pub chat_type: String,
    pub contact_id: Uuid,
    pub limit: Option<i64>,
}

pub async fn history(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<MessageHistoryQuery>,
) -> Result<Json<ApiResponse<Vec<Message>>>, StatusCode> {
    let user_id = claims
        .sub
        .parse::<Uuid>()
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let limit = query.limit.unwrap_or(100).clamp(1, 200);

    let conversation_id = match query.chat_type.as_str() {
        "private" => {
            if query.contact_id == user_id {
                return Err(StatusCode::BAD_REQUEST);
            }
            let (first, second) = if user_id.as_bytes() <= query.contact_id.as_bytes() {
                (user_id, query.contact_id)
            } else {
                (query.contact_id, user_id)
            };
            Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("private:{first}:{second}").as_bytes(),
            )
        }
        "public" => {
            let member = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM group_member WHERE group_id = $1 AND user_id = $2)",
            )
            .bind(query.contact_id)
            .bind(user_id)
            .fetch_one(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            if !member {
                return Err(StatusCode::FORBIDDEN);
            }
            query.contact_id
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let mut messages = sqlx::query_as::<_, Message>(
        r#"
        SELECT id, conversation_id, chat_type, send_id, client_message_id,
               receiver_id, group_id, content, message_type, status,
               created_at, update_at, deleted_at, file_name, file_url
        FROM "message"
        WHERE conversation_id = $1
          AND deleted_at IS NULL
        ORDER BY created_at DESC, id DESC
        LIMIT $2
        "#,
    )
    .bind(conversation_id)
    .bind(limit)
    .fetch_all(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, %user_id, "Failed to load message history");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    messages.reverse();

    Ok(Json(ApiResponse::success(messages)))
}

pub async fn websocket(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    let user_id = match claims.sub.parse::<Uuid>() {
        Ok(user_id) => user_id,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };

    upgrade
        .on_upgrade(move |socket| websocket_session(socket, state, user_id))
        .into_response()
}

async fn websocket_session(mut socket: WebSocket, state: AppState, user_id: Uuid) {
    let mut messages = state.message_tx.subscribe();

    loop {
        tokio::select! {
            event = messages.recv() => {
                match event {
                    Ok(event) if event.recipients.contains(&user_id) => {
                        let payload = match serde_json::to_string(&event) {
                            Ok(payload) => payload,
                            Err(_) => continue,
                        };
                        if socket.send(WsMessage::Text(payload.into())).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }
}

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
