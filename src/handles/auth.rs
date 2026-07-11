use axum::{Extension, Json, extract::State, http::StatusCode};

use crate::app::AppState;
use crate::middleware::jwt::{self, Claims};
use crate::models::user::{AuthResponse, LoginInput, RegisterInput, User};

pub async fn register(State(state): State<AppState>, Json(input): Json<RegisterInput>) -> Result<(StatusCode, Json<AuthResponse>), StatusCode> {
    let hash = bcrypt::hash(&input.password, bcrypt::DEFAULT_COST).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let user = sqlx::query_as::<_, User>(
        r##"INSERT INTO "user" (name, email, password_hash) VALUES ($1, $2, $3)
         RETURNING id, name, email, password_hash, created_at, updated_at"##,
    )
    .bind(&input.name)
    .bind(&input.email)
    .bind(&hash)
    .fetch_one(&state.db)
    .await
    .map_err(|_| StatusCode::CONFLICT)?;
    let token = jwt::sign(&user.id.to_string(), &state.jwt_secret, state.jwt_max_age)?;
    Ok((StatusCode::CREATED, Json(AuthResponse { token, user })))
}

pub async fn login(State(state): State<AppState>, Json(input): Json<LoginInput>) -> Result<Json<AuthResponse>, StatusCode> {
    let user = sqlx::query_as::<_, User>(
        r##"SELECT id, name, email, password_hash, created_at, updated_at FROM "user" WHERE email = $1"##,
    )
    .bind(&input.email)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::UNAUTHORIZED)?;
    let valid = bcrypt::verify(&input.password, &user.password_hash).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !valid { return Err(StatusCode::UNAUTHORIZED); }
    let token = jwt::sign(&user.id.to_string(), &state.jwt_secret, state.jwt_max_age)?;
    Ok(Json(AuthResponse { token, user }))
}

pub async fn me(State(state): State<AppState>, Extension(claims): Extension<Claims>) -> Result<Json<User>, StatusCode> {
    let user = sqlx::query_as::<_, User>(
        r##"SELECT id, name, email, password_hash, created_at, updated_at FROM "user" WHERE id = $1"##,
    )
    .bind(&claims.sub.parse::<uuid::Uuid>().map_err(|_| StatusCode::UNAUTHORIZED)?)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(user))
}
