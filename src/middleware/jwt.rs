use axum::{
    extract::{Request, State},
    http::StatusCode,
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

pub async fn require_auth(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let header_token = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let cookie_token = req.headers().get("Cookie").and_then(|value| {
        value.to_str().ok()?.split(';').find_map(|part| {
            let (name, token) = part.trim().split_once('=')?;
            (name == "auth_token").then_some(token)
        })
    });
    let token = header_token.or(cookie_token).ok_or(StatusCode::UNAUTHORIZED);
    let token = match token {
        Ok(t) => t,
        Err(status) => return status.into_response(),
    };

    let claims = match verify(token, &state.jwt_secret) {
        Ok(c) => c,
        Err(resp) => return resp.into_response(),
    };

    req.extensions_mut().insert(claims);
    next.run(req).await
}
