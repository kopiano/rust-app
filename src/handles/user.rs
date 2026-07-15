use axum::{
    Extension,
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::ImageFormat;
use std::io::Cursor;
use uuid::Uuid;

use crate::middleware::jwt::Claims;
use crate::app::AppState;
use crate::common::response::ApiResponse;
use crate::models::user::{CreateUser, UpdateProfileInput, UpdateUser, User};

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

pub async fn profile(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(input): Json<UpdateProfileInput>,
) -> Result<Json<ApiResponse<User>>, StatusCode> {
    const MAX_AVATAR_BYTES: usize = 5 * 1024 * 1024;

    let user_id = claims
        .sub
        .parse::<Uuid>()
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let username = input.username.trim();
    if username.is_empty() || input.avatar.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let current = sqlx::query_as::<_, User>(
        r##"SELECT id, name, email, github_id, avatar, last_login_at, status, password_hash, created_at, updated_at
            FROM "user" WHERE id = $1"##,
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    if current.github_id.is_some() {
        return Err(StatusCode::FORBIDDEN);
    }

    let avatar = if input.avatar == current.avatar.clone().unwrap_or_default() {
        input.avatar
    } else {
        let image_bytes = decode_avatar(&input.avatar)?;
        if image_bytes.len() > MAX_AVATAR_BYTES {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        let image = image::load_from_memory(&image_bytes).map_err(|_| StatusCode::BAD_REQUEST)?;
        let resized = image.thumbnail(512, 512);
        let filename = format!(
            "avatar-{}-{}.webp",
            avatar_name_component(username),
            chrono::Utc::now().timestamp_millis()
        );
        let directory = std::path::Path::new("src/assets/avatar");
        tokio::fs::create_dir_all(directory)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let path = directory.join(&filename);
        let mut encoded = Cursor::new(Vec::new());
        resized
            .write_to(&mut encoded, ImageFormat::WebP)
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        tokio::fs::write(path, encoded.into_inner())
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        format!("/api/assets/avatar/{filename}")
    };

    let password_hash = if input.password.trim().is_empty() {
        current.password_hash
    } else {
        let password = input.password;
        tokio::task::spawn_blocking(move || bcrypt::hash(password, 8))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };

    sqlx::query_as::<_, User>(
        r##"UPDATE "user" SET
               name = $2,
               avatar = $3,
               password_hash = $4,
               updated_at = NOW()
            WHERE id = $1
            RETURNING id, name, email, github_id, avatar, last_login_at, status, password_hash, created_at, updated_at"##,
    )
    .bind(user_id)
    .bind(username)
    .bind(avatar)
    .bind(password_hash)
    .fetch_one(&state.db)
    .await
    .map(|user| Json(ApiResponse::success(user)))
    .map_err(|error| {
        if let sqlx::Error::Database(database_error) = &error {
            if database_error.constraint() == Some("user_name_unique") {
                return StatusCode::CONFLICT;
            }
        }
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

fn decode_avatar(value: &str) -> Result<Vec<u8>, StatusCode> {
    let encoded = value
        .strip_prefix("data:image/")
        .and_then(|value| value.split_once(";base64,").map(|(_, encoded)| encoded))
        .ok_or(StatusCode::BAD_REQUEST)?;
    STANDARD.decode(encoded).map_err(|_| StatusCode::BAD_REQUEST)
}

fn avatar_name_component(username: &str) -> String {
    let value = username
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let value = value.trim_matches('-');
    if value.is_empty() {
        "user".to_string()
    } else {
        value.chars().take(64).collect()
    }
}

pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<()>>, StatusCode> {
    sqlx::query(r##"DELETE FROM "user" WHERE id = $1"##)
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
        .and_then(|result| {
            if result.rows_affected() > 0 {
                Ok(Json(ApiResponse::success(())))
            } else {
                Err(StatusCode::NOT_FOUND)
            }
        })
}
