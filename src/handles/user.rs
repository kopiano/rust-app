use axum::{
    Extension,
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use uuid::Uuid;

use crate::middleware::jwt::Claims;
use crate::app::AppState;
use crate::common::response::ApiResponse;
use crate::models::user::{CreateUser, UpdateUser, User};

pub async fn me(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<ApiResponse<User>>, StatusCode> {
    let id = claims
        .sub
        .parse::<Uuid>()
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let user = sqlx::query_as::<_, User>(
        r##"SELECT id, name, email, github_id, avatar, last_login_at, status, password_hash, created_at, updated_at
            FROM "user" WHERE id = $1"##,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(ApiResponse::success(user)))
}

pub async fn list(
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<User>>>, StatusCode> {
    sqlx::query_as::<_, User>(
        r##"SELECT id, name, email, github_id, avatar, last_login_at, status, password_hash, created_at, updated_at FROM "user" ORDER BY created_at DESC"##,
    )
    .fetch_all(&state.db)
    .await
        .map(|users| Json(ApiResponse::success(users)))
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub async fn get_by_id(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<User>>, StatusCode> {
    let user = sqlx::query_as::<_, User>(
        r##"SELECT id, name, email, github_id, avatar, last_login_at, status, password_hash, created_at, updated_at FROM "user" WHERE id = $1"##,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    user.ok_or(StatusCode::NOT_FOUND)
        .map(|user| Json(ApiResponse::success(user)))
}

pub async fn create(
    State(state): State<AppState>,
    Json(input): Json<CreateUser>,
) -> Result<(StatusCode, Json<ApiResponse<User>>), StatusCode> {
    sqlx::query_as::<_, User>(
        r##"INSERT INTO "user" (name, email) VALUES ($1, $2)
         RETURNING id, name, email, github_id, avatar, last_login_at, status, password_hash, created_at, updated_at"##,
    )
    .bind(&input.name)
    .bind(&input.email)
    .fetch_one(&state.db)
    .await
    .map(|user| (StatusCode::CREATED, Json(ApiResponse::success(user))))
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateUser>,
) -> Result<Json<ApiResponse<User>>, StatusCode> {
    sqlx::query_as::<_, User>(
        r##"UPDATE "user" SET
               name = COALESCE($2, name),
               email = COALESCE($3, email),
               updated_at = NOW()
            WHERE id = $1
            RETURNING id, name, email, github_id, avatar, last_login_at, status, password_hash, created_at, updated_at"##,
    )
    .bind(id)
    .bind(&input.name)
    .bind(&input.email)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)
    .map(|user| Json(ApiResponse::success(user)))
}

pub async fn delete(State(state): State<AppState>, Path(id): Path<Uuid>) -> StatusCode {
    sqlx::query(r##"DELETE FROM "user" WHERE id = $1"##)
        .bind(id)
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
