use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::app::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: usize,
    pub iat: usize,
}

pub fn sign(user_id: &str, secret: &str, max_age: i64) -> Result<String, StatusCode> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .as_secs() as usize;

    let claims = Claims {
        sub: user_id.to_string(),
        exp: now + max_age as usize * 60,
        iat: now,
    };

    jsonwebtoken::encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_ref()),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn verify(token: &str, secret: &str) -> Result<Claims, StatusCode> {
    jsonwebtoken::decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_ref()),
        &Validation::default(),
    )
    .map(|data| data.claims)
    .map_err(|_| StatusCode::UNAUTHORIZED)
}

fn request_token(headers: &HeaderMap) -> Option<&str> {
    let header_token = headers
        .get("Authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let cookie_token = headers.get("Cookie").and_then(|value| {
        value.to_str().ok()?.split(';').find_map(|part| {
            let (name, token) = part.trim().split_once('=')?;
            (name == "access_token").then_some(token)
        })
    });
    header_token.or(cookie_token)
}

pub async fn require_auth(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let token = request_token(req.headers()).ok_or(StatusCode::UNAUTHORIZED);
    let token = match token {
        Ok(t) => t,
        Err(status) => return status.into_response(),
    };

    let claims = match verify(token, &state.jwt_secret) {
        Ok(c) => c,
        Err(resp) => return resp.into_response(),
    };

    req.extensions_mut().insert(claims.clone());
    let mut response = next.run(req).await;
    response.extensions_mut().insert(claims);
    response
}

pub async fn optional_auth(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    if let Some(claims) =
        request_token(req.headers()).and_then(|token| verify(token, &state.jwt_secret).ok())
    {
        req.extensions_mut().insert(claims);
    }
    next.run(req).await
}
