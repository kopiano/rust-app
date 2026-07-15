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
    models::moment::{CreateMoment, Moment},
};

pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(input): Json<CreateMoment>,
) -> Result<(StatusCode, Json<ApiResponse<Moment>>), StatusCode> {
    let user_id = claims
        .sub
        .parse::<Uuid>()
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let content = input.content.map(|value| value.trim().to_owned());

    if content.as_deref().is_none_or(str::is_empty) && input.media.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if input.media.iter().any(|media| {
        !matches!(media.media_type.as_str(), "image" | "video")
            || media.url.trim().is_empty()
    }) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let moment = sqlx::query_as::<_, Moment>(
        r#"
        INSERT INTO moment (user_id, content, media)
        VALUES ($1, $2, $3)
        RETURNING id, user_id, content, media, created_at, updated_at
        "#,
    )
    .bind(user_id)
    .bind(content.filter(|value| !value.is_empty()))
    .bind(sqlx::types::Json(input.media))
    .fetch_one(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, %user_id, "Failed to create moment");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok((StatusCode::CREATED, Json(ApiResponse::success(moment))))
}

pub async fn list(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> Result<Json<ApiResponse<Vec<Moment>>>, StatusCode> {
    let moments = sqlx::query_as::<_, Moment>(
        r#"
        SELECT id, user_id, content, media, created_at, updated_at
        FROM moment
        ORDER BY created_at DESC, id DESC
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, "Failed to load moments");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(ApiResponse::success(moments)))
}
