use axum::{
    Extension, Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::Redirect,
};
use rand::{Rng, distr::Alphanumeric};
use redis::AsyncCommands;
use serde::Deserialize;

use crate::app::AppState;
use crate::middleware::jwt::{self, Claims};
use crate::models::user::{AuthResponse, LoginInput, RegisterInput, User};

pub async fn register(
    State(state): State<AppState>,
    Json(input): Json<RegisterInput>,
) -> Result<(StatusCode, Json<AuthResponse>), StatusCode> {
    let hash = bcrypt::hash(&input.password, bcrypt::DEFAULT_COST)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let user = sqlx::query_as::<_, User>(
        r##"INSERT INTO "user" (name, email, password_hash) VALUES ($1, $2, $3)
         RETURNING id, name, email, github_id, password_hash, created_at, updated_at"##,
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

pub async fn login(
    State(state): State<AppState>,
    Json(input): Json<LoginInput>,
) -> Result<Json<AuthResponse>, StatusCode> {
    let user = sqlx::query_as::<_, User>(
        r##"SELECT id, name, email, github_id, password_hash, created_at, updated_at FROM "user" WHERE email = $1"##,
    )
    .bind(&input.email)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::UNAUTHORIZED)?;
    let valid = bcrypt::verify(&input.password, &user.password_hash)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !valid {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let token = jwt::sign(&user.id.to_string(), &state.jwt_secret, state.jwt_max_age)?;
    Ok(Json(AuthResponse { token, user }))
}

pub async fn me(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<User>, StatusCode> {
    let user = sqlx::query_as::<_, User>(
        r##"SELECT id, name, email, github_id, password_hash, created_at, updated_at FROM "user" WHERE id = $1"##,
    )
    .bind(&claims.sub.parse::<uuid::Uuid>().map_err(|_| StatusCode::UNAUTHORIZED)?)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(user))
}

#[derive(Debug, Deserialize)]
pub struct GithubCallback {
    code: String,
    state: String,
}

#[derive(Debug, Deserialize)]
struct GithubUser {
    id: u64,
    login: String,
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubEmail {
    email: String,
    primary: bool,
    verified: bool,
}

pub async fn github_login(State(state): State<AppState>) -> Result<Redirect, StatusCode> {
    let state_value: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();
    let mut redis = state.redis.clone();
    redis
        .set_ex::<_, _, ()>(format!("github_oauth:{state_value}"), "1", 600)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let url = format!(
        "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&scope=read%3Auser%20user%3Aemail&state={}",
        urlencoding::encode(&state.github_client_id),
        urlencoding::encode(&state.github_redirect_uri),
        urlencoding::encode(&state_value)
    );
    Ok(Redirect::temporary(&url))
}

pub async fn github_callback(
    State(state): State<AppState>,
    Query(query): Query<GithubCallback>,
) -> Result<Redirect, StatusCode> {
    let mut redis = state.redis.clone();
    let key = format!("github_oauth:{}", query.state);
    let valid: Option<String> = redis
        .get_del(&key)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if valid.is_none() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let client = reqwest::Client::new();
    let token = client
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .form(&[
            ("client_id", state.github_client_id.as_str()),
            ("client_secret", state.github_client_secret.as_str()),
            ("code", query.code.as_str()),
            ("redirect_uri", state.github_redirect_uri.as_str()),
        ])
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?
        .json::<serde_json::Value>()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?
        .get("access_token")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let github: GithubUser = client
        .get("https://api.github.com/user")
        .bearer_auth(&token)
        .header("User-Agent", "rust-app")
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?
        .json()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let email = match github.email {
        Some(email) => email,
        None => client
            .get("https://api.github.com/user/emails")
            .bearer_auth(&token)
            .header("User-Agent", "rust-app")
            .send()
            .await
            .map_err(|_| StatusCode::BAD_GATEWAY)?
            .json::<Vec<GithubEmail>>()
            .await
            .map_err(|_| StatusCode::BAD_GATEWAY)?
            .into_iter()
            .find(|e| e.primary && e.verified)
            .map(|e| e.email)
            .ok_or(StatusCode::UNAUTHORIZED)?,
    };
    let user = sqlx::query_as::<_, User>(r##"INSERT INTO "user" (name, email, github_id) VALUES ($1, $2, $3)
        ON CONFLICT (email) DO UPDATE SET github_id = EXCLUDED.github_id, name = EXCLUDED.name, updated_at = NOW()
        RETURNING id, name, email, github_id, password_hash, created_at, updated_at"##)
        .bind(&github.login).bind(&email).bind(github.id.to_string()).fetch_one(&state.db).await.map_err(|_| StatusCode::CONFLICT)?;
    let jwt = jwt::sign(&user.id.to_string(), &state.jwt_secret, state.jwt_max_age)?;
    let refresh: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect();
    redis
        .set_ex::<_, _, ()>(
            format!("refresh:{refresh}"),
            user.id.to_string(),
            60 * 60 * 24 * 30,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Redirect::temporary(&format!(
        "{}/?auth_token={jwt}&refresh_token={refresh}",
        state.frontend_url.trim_end_matches('/')
    )))
}

pub async fn logout(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<StatusCode, StatusCode> {
    if let Some(token) = headers.get("X-Refresh-Token").and_then(|v| v.to_str().ok()) {
        let mut redis = state.redis.clone();
        let _: () = redis
            .del(format!("refresh:{token}"))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct RefreshInput {
    pub refresh_token: String,
}

pub async fn refresh(
    State(state): State<AppState>,
    Json(input): Json<RefreshInput>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut redis = state.redis.clone();
    let user_id: Option<String> = redis
        .get(format!("refresh:{}", input.refresh_token))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let user_id = user_id.ok_or(StatusCode::UNAUTHORIZED)?;
    let token = jwt::sign(&user_id, &state.jwt_secret, state.jwt_max_age)?;
    Ok(Json(serde_json::json!({ "token": token })))
}
