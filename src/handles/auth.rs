use axum::{
    Json,
    extract::{Query, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{HOST, ORIGIN, REFERER, SET_COOKIE},
    },
    response::{IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use rand::{Rng, distr::Alphanumeric};
use redis::AsyncCommands;
use serde::Deserialize;

use crate::app::AppState;
use crate::common::response::ApiResponse;
use crate::middleware::jwt;
use crate::models::user::{AuthResponse, LoginInput, RegisterInput, User};

// Used for unknown users so the response path still performs bcrypt verification.
const DUMMY_PASSWORD_HASH: &str = "$2y$08$mnpm4SdYbuv8jY6GBq5DxOeLZWTbhoyhFStR7UclYBrbt0pCQ6SYC";
const BCRYPT_COST: u32 = 8;

pub async fn register(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(input): Json<RegisterInput>,
) -> Result<Response, StatusCode> {
    let name = input.name.trim();
    let email = input.email.trim();
    if name.is_empty()
        || email.is_empty()
        || input.password.trim().is_empty()
        || !email.contains('@')
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let password = input.password.clone();
    let hash = tokio::task::spawn_blocking(move || bcrypt::hash(password, BCRYPT_COST))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let user = sqlx::query_as::<_, User>(
        r##"INSERT INTO "user" (name, email, password_hash, last_login_at)
         VALUES ($1, $2, $3, NOW())
         RETURNING id, name, email, github_id, avatar, last_login_at, password_hash, created_at, updated_at"##,
    )
    .bind(name)
    .bind(email)
    .bind(&hash)
    .fetch_one(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, "User registration insert failed");
        match &error {
            sqlx::Error::Database(database_error)
                if database_error.constraint() == Some("user_email_key")
                    || database_error.constraint() == Some("user_name_unique") =>
            {
                StatusCode::CONFLICT
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    })?;
    let token = jwt::sign(&user.id.to_string(), &state.jwt_secret, state.jwt_max_age)?;
    Ok(auth_response(
        &headers,
        StatusCode::CREATED,
        AuthResponse {
            token: token.clone(),
            user,
        },
        token,
    ))
}

pub async fn login(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(input): Json<LoginInput>,
) -> Result<Response, StatusCode> {
    // check
    let username = input.username.trim();
    if username.is_empty() || input.password.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::error(
                400,
                "Invalid username or password",
            )),
        )
            .into_response());
    }
    // sql
    let user = sqlx::query_as::<_, User>(
        r##"SELECT id, name, email, github_id, avatar, last_login_at, password_hash, created_at, updated_at FROM "user" WHERE name = $1"##,
    )
    .bind(username)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let password_hash = user
        .as_ref()
        .map(|user| user.password_hash.clone())
        .unwrap_or_else(|| DUMMY_PASSWORD_HASH.to_owned());
    let password = input.password.clone();
    let valid = tokio::task::spawn_blocking(move || bcrypt::verify(password, &password_hash))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let user = match (user, valid) {
        (Some(user), true) => user,
        _ => {
            return Ok((
                StatusCode::UNAUTHORIZED,
                Json(ApiResponse::<()>::error(
                    401,
                    "Invalid username or password",
                )),
            )
                .into_response());
        }
    };
    let pool = state.db.clone();
    tokio::spawn(async move {
        let _ = sqlx::query(r##"UPDATE "user" SET last_login_at = NOW() WHERE id = $1"##)
            .bind(user.id)
            .execute(&pool)
            .await;
    });
    // token
    let token = jwt::sign(&user.id.to_string(), &state.jwt_secret, state.jwt_max_age)?;
    Ok(auth_response(
        &headers,
        StatusCode::OK,
        AuthResponse {
            token: token.clone(),
            user,
        },
        token,
    ))
}

fn is_local_environment() -> bool {
    std::env::var("FRONTEND_URL")
        .map(|url| url.starts_with("http://localhost") || url.starts_with("http://127.0.0.1"))
        .unwrap_or(cfg!(debug_assertions))
}

fn request_uses_local_cookie(headers: &HeaderMap) -> bool {
    headers
        .get(ORIGIN)
        .or_else(|| headers.get(REFERER))
        .or_else(|| headers.get(HOST))
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value.contains("localhost") || value.contains("127.0.0.1") || value.contains("[::1]")
        })
        .unwrap_or_else(is_local_environment)
}

fn build_auth_cookie(value: String, max_age: time::Duration, local: bool) -> Cookie<'static> {
    let cookie = Cookie::build(("access_token", value))
        .http_only(true)
        .path("/")
        .max_age(max_age);
    let cookie = if local {
        cookie.secure(false).same_site(SameSite::Lax).build()
    } else {
        cookie
            .secure(true)
            .domain(".coulsonzero.shop")
            .same_site(SameSite::None)
            .build()
    };
    cookie
}

fn auth_response(
    headers: &HeaderMap,
    status: StatusCode,
    body: AuthResponse,
    token: String,
) -> Response {
    let cookie = build_auth_cookie(
        token,
        time::Duration::days(7),
        request_uses_local_cookie(headers),
    );
    let jar = CookieJar::new().add(cookie);

    (status, jar, Json(ApiResponse::success(body))).into_response()
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
    avatar_url: Option<String>,
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
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(query): Query<GithubCallback>,
) -> Response {
    let frontend_url = state.frontend_url.trim_end_matches('/').to_owned();
    match github_callback_inner(state, query).await {
        Ok((redirect, token)) => {
            let cookie = build_auth_cookie(
                token,
                time::Duration::days(7),
                request_uses_local_cookie(&headers),
            );
            (CookieJar::new().add(cookie), redirect).into_response()
        }
        Err(status) => {
            tracing::error!(%status, "GitHub OAuth callback failed");
            Redirect::temporary(&format!("{frontend_url}/?auth_error=github_login_failed"))
                .into_response()
        }
    }
}

async fn github_callback_inner(
    state: AppState,
    query: GithubCallback,
) -> Result<(Redirect, String), StatusCode> {
    let mut redis = state.redis.clone();
    let key = format!("github_oauth:{}", query.state);
    let valid: Option<String> = redis.get_del(&key).await.map_err(|error| {
        tracing::error!(%error, "GitHub OAuth state lookup failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    if valid.is_none() {
        tracing::warn!("GitHub OAuth state is missing or expired");
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
        .map_err(|error| {
            tracing::error!(%error, "GitHub access token request failed");
            StatusCode::BAD_GATEWAY
        })?
        .json::<serde_json::Value>()
        .await
        .map_err(|error| {
            tracing::error!(%error, "GitHub access token response was invalid");
            StatusCode::BAD_GATEWAY
        })?
        .get("access_token")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            tracing::error!("GitHub did not return an access token");
            StatusCode::UNAUTHORIZED
        })?;
    let github: GithubUser = client
        .get("https://api.github.com/user")
        .bearer_auth(&token)
        .header("User-Agent", "rust-app")
        .send()
        .await
        .map_err(|error| {
            tracing::error!(%error, "GitHub user request failed");
            StatusCode::BAD_GATEWAY
        })?
        .json()
        .await
        .map_err(|error| {
            tracing::error!(%error, "GitHub user response was invalid");
            StatusCode::BAD_GATEWAY
        })?;
    let email = match github.email {
        Some(email) => email,
        None => client
            .get("https://api.github.com/user/emails")
            .bearer_auth(&token)
            .header("User-Agent", "rust-app")
            .send()
            .await
            .map_err(|error| {
                tracing::error!(%error, "GitHub email request failed");
                StatusCode::BAD_GATEWAY
            })?
            .json::<Vec<GithubEmail>>()
            .await
            .map_err(|error| {
                tracing::error!(%error, "GitHub email response was invalid");
                StatusCode::BAD_GATEWAY
            })?
            .into_iter()
            .find(|e| e.primary && e.verified)
            .map(|e| e.email)
            .ok_or_else(|| {
                tracing::warn!("GitHub account has no verified primary email");
                StatusCode::UNAUTHORIZED
            })?,
    };
    let user = sqlx::query_as::<_, User>(r##"INSERT INTO "user"
        (name, email, password_hash, github_id, avatar, last_login_at)
        VALUES ($1, $2, $3, $4, $5, NOW())
        ON CONFLICT (email) DO UPDATE SET
            github_id = EXCLUDED.github_id,
            name = EXCLUDED.name,
            avatar = EXCLUDED.avatar,
            last_login_at = NOW(),
            updated_at = NOW()
        RETURNING id, name, email, github_id, avatar, last_login_at, password_hash, created_at, updated_at"##)
        .bind(&github.login)
        .bind(&email)
        .bind("")
        .bind(github.id.to_string())
        .bind(&github.avatar_url)
        .fetch_one(&state.db)
        .await
        .map_err(|_| StatusCode::CONFLICT)?;
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
    let redirect = Redirect::temporary(&format!(
        "{}/chat",
        state.frontend_url.trim_end_matches('/')
    ));
    Ok((redirect, jwt))
}

pub async fn logout(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Result<Response, StatusCode> {
    if let Some(token) = headers.get("X-Refresh-Token").and_then(|v| v.to_str().ok()) {
        let mut redis = state.redis.clone();
        let _: () = redis
            .del(format!("refresh:{token}"))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    // Clear both cookie scopes. Older deployments may have created a host-only
    // cookie while current production uses the shared parent domain.
    let local_cookie = build_auth_cookie(String::new(), time::Duration::ZERO, true);
    let domain_cookie = build_auth_cookie(String::new(), time::Duration::ZERO, false);
    let mut response = (StatusCode::OK, Json(ApiResponse::success(()))).into_response();
    for cookie in [local_cookie, domain_cookie] {
        let value = HeaderValue::from_str(&cookie.to_string())
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        response.headers_mut().append(SET_COOKIE, value);
    }
    Ok(response)
}

#[derive(Debug, Deserialize)]
pub struct RefreshInput {
    pub refresh_token: String,
}

pub async fn refresh(
    State(state): State<AppState>,
    Json(input): Json<RefreshInput>,
) -> Result<Json<ApiResponse<serde_json::Value>>, StatusCode> {
    let mut redis = state.redis.clone();
    let user_id: Option<String> = redis
        .get(format!("refresh:{}", input.refresh_token))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let user_id = user_id.ok_or(StatusCode::UNAUTHORIZED)?;
    let token = jwt::sign(&user_id, &state.jwt_secret, state.jwt_max_age)?;
    Ok(Json(ApiResponse::success(
        serde_json::json!({ "token": token }),
    )))
}
