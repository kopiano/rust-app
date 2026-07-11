use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use uuid::Uuid;

use crate::app::AppState;
use crate::middleware::jwt::Claims;
use crate::models::task::{CreateTask, Task, UpdateTask};

pub async fn list(State(state): State<AppState>, Extension(claims): Extension<Claims>) -> Result<Json<Vec<Task>>, StatusCode> {
    let user_id: Uuid = claims.sub.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
    let tasks = sqlx::query_as::<_, Task>(
        r##"SELECT id, user_id, title, completed, created_at, updated_at FROM "task" WHERE user_id = $1 ORDER BY created_at DESC"##,
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(tasks))
}

pub async fn get_by_id(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<Json<Task>, StatusCode> {
    let user_id: Uuid = claims.sub.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
    let task = sqlx::query_as::<_, Task>(
        r##"SELECT id, user_id, title, completed, created_at, updated_at FROM "task" WHERE id = $1 AND user_id = $2"##,
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(task))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(input): Json<CreateTask>,
) -> Result<(StatusCode, Json<Task>), StatusCode> {
    let user_id: Uuid = claims.sub.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
    let task = sqlx::query_as::<_, Task>(
        r##"INSERT INTO "task" (user_id, title) VALUES ($1, $2)
         RETURNING id, user_id, title, completed, created_at, updated_at"##,
    )
    .bind(user_id)
    .bind(&input.title)
    .fetch_one(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((StatusCode::CREATED, Json(task)))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateTask>,
) -> Result<Json<Task>, StatusCode> {
    let user_id: Uuid = claims.sub.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
    let task = sqlx::query_as::<_, Task>(
        r##"UPDATE "task" SET
               title = COALESCE($3, title),
               completed = COALESCE($4, completed),
               updated_at = NOW()
            WHERE id = $1 AND user_id = $2
            RETURNING id, user_id, title, completed, created_at, updated_at"##,
    )
    .bind(id)
    .bind(user_id)
    .bind(&input.title)
    .bind(input.completed)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(task))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> StatusCode {
    let user_id: Uuid = match claims.sub.parse() {
        Ok(id) => id,
        Err(_) => return StatusCode::UNAUTHORIZED,
    };
    sqlx::query(r##"DELETE FROM "task" WHERE id = $1 AND user_id = $2"##)
        .bind(id)
        .bind(user_id)
        .execute(&state.db)
        .await
        .map(|result| {
            if result.rows_affected() > 0 {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::NOT_FOUND
            }
        })
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}
