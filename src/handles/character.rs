use axum::{
    Json,
    body::Body,
    extract::{
        Extension, Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{
        HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use std::time::{Duration, Instant};
use tokio::time::timeout;
use uuid::Uuid;

use crate::{
    app::AppState,
    common::response::ApiResponse,
    handles::voice::{post_json, rewrite_audio_url},
    middleware::jwt::Claims,
    models::character::Character,
};

#[derive(Debug, Serialize)]
struct CharacterVoiceModel {
    id: String,
    version: i32,
    name: String,
}

#[derive(Debug, Serialize)]
struct CharacterVoiceModelOption {
    id: String,
    version: i32,
    name: String,
    status: String,
    active: bool,
}

#[derive(Debug, Deserialize)]
pub struct SwitchCharacterModelInput {
    model_id: String,
}

#[derive(Debug, Serialize)]
struct CharacterDetail {
    id: Uuid,
    name: String,
    voice_model: Option<CharacterVoiceModel>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct CharacterListItem {
    pub id: Uuid,
    pub name: String,
    pub avatar_url: Option<String>,
    pub description: Option<String>,
    pub voice_model: Option<String>,
    pub train_status: String,
}

#[derive(Debug, Serialize, FromRow)]
struct CharacterSession {
    id: Uuid,
    character_id: Uuid,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSessionInput {
    character_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct CharacterBindingInput {
    character_id: Option<Uuid>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct CharacterBinding {
    contact_user_id: Uuid,
    character_id: Uuid,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LlmConfigInput {
    model_name: String,
    api_token: String,
    api_request_url: String,
    effort: String,
}

#[derive(Debug, Deserialize)]
pub struct CharacterMessageInput {
    session_id: Uuid,
    message: String,
    llm: LlmConfigInput,
    language: Option<String>,
    speed_factor: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct TtsInput {
    character_id: Uuid,
    model_id: Option<String>,
    text: String,
    language: Option<String>,
    speed_factor: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCharacterInput {
    name: String,
    avatar_url: Option<String>,
    description: Option<String>,
    system_prompt: Option<String>,
    voice_model: Option<String>,
    ckpt_path: Option<String>,
    pth_path: Option<String>,
    train_status: Option<String>,
}

#[derive(Debug, FromRow)]
struct SessionCharacter {
    session_id: Uuid,
    character_id: Uuid,
    system_prompt: Option<String>,
    voice_model: Option<String>,
    train_status: String,
}

#[derive(Debug, FromRow)]
struct ChatHistoryRow {
    role: String,
    content: String,
}

fn json_response<T: Serialize>(status: StatusCode, response: ApiResponse<T>) -> Response {
    (status, Json(response)).into_response()
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    json_response(status, ApiResponse::<()>::error(status.as_u16(), message))
}

fn user_id(claims: &Claims) -> Result<Uuid, Response> {
    claims
        .sub
        .parse()
        .map_err(|_| error_response(StatusCode::UNAUTHORIZED, "Invalid authenticated user"))
}

fn normalized_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn normalized_language(language: Option<&str>) -> Result<&str, Response> {
    match language.unwrap_or("zh") {
        "zh" => Ok("zh"),
        "en" => Ok("en"),
        _ => Err(error_response(
            StatusCode::BAD_REQUEST,
            "Language must be zh or en",
        )),
    }
}

fn normalized_speed(speed: Option<f64>) -> Result<f64, Response> {
    let speed = speed.unwrap_or(1.0);
    if (0.5..=2.0).contains(&speed) {
        Ok(speed)
    } else {
        Err(error_response(
            StatusCode::BAD_REQUEST,
            "Speed factor must be between 0.5 and 2.0",
        ))
    }
}

async fn require_admin(state: &AppState, claims: &Claims) -> Result<Uuid, Response> {
    let user_id = user_id(claims)?;
    let role = sqlx::query_scalar::<_, String>(r#"SELECT role FROM "user" WHERE id = $1"#)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|error| {
            tracing::error!(target: "app::character", %error, %user_id, "Admin role lookup failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to verify administrator",
            )
        })?;
    match role.as_deref() {
        Some("admin" | "super_admin") => Ok(user_id),
        Some(_) => Err(error_response(
            StatusCode::FORBIDDEN,
            "Administrator permission is required",
        )),
        None => Err(error_response(StatusCode::UNAUTHORIZED, "User not found")),
    }
}

pub async fn list(State(state): State<AppState>) -> Response {
    match sqlx::query_as::<_, CharacterListItem>(
        r#"
        SELECT id, name, avatar_url, description, voice_model, train_status
        FROM "character"
        ORDER BY created_at ASC, id ASC
        "#,
    )
    .fetch_all(&state.db)
    .await
    {
        Ok(characters) => json_response(StatusCode::OK, ApiResponse::success(characters)),
        Err(error) => {
            tracing::error!(target: "app::character", %error, "Character list failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to load characters",
            )
        }
    }
}

pub async fn get(State(state): State<AppState>, Path(character_id): Path<Uuid>) -> Response {
    let character = match sqlx::query_as::<_, CharacterDetailRow>(
        r#"
        SELECT
            character.id,
            character.name,
            model.version AS voice_model_version,
            model.name AS voice_model_name
        FROM "character" AS character
        LEFT JOIN LATERAL (
            SELECT name, version
            FROM character_voice_model
            WHERE id = character.active_voice_model_id
               OR (
                   character.active_voice_model_id IS NULL
                   AND name = character.voice_model
               )
            ORDER BY (id = character.active_voice_model_id) DESC, version DESC
            LIMIT 1
        ) AS model ON TRUE
        WHERE character.id = $1
        "#,
    )
    .bind(character_id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "Character not found"),
        Err(error) => {
            tracing::error!(target: "app::character", %error, %character_id, "Character detail lookup failed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to load character",
            );
        }
    };

    let voice_model = character.voice_model_name.map(|name| {
        let version = character.voice_model_version.unwrap_or(1);
        CharacterVoiceModel {
            id: version_model_id(version),
            version,
            name,
        }
    });
    json_response(
        StatusCode::OK,
        ApiResponse::success(CharacterDetail {
            id: character.id,
            name: character.name,
            voice_model,
        }),
    )
}

#[derive(Debug, FromRow)]
struct CharacterDetailRow {
    id: Uuid,
    name: String,
    voice_model_version: Option<i32>,
    voice_model_name: Option<String>,
}

#[derive(Debug, FromRow)]
struct CharacterModelRow {
    id: Uuid,
    version: i32,
    name: String,
    status: String,
    active_voice_model_id: Option<Uuid>,
}

fn version_model_id(version: i32) -> String {
    format!("v{version}")
}

fn parse_model_version(model_id: &str) -> Option<i32> {
    model_id
        .strip_prefix('v')
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|version| *version > 0)
}

pub async fn list_models(
    State(state): State<AppState>,
    Path(character_id): Path<Uuid>,
) -> Response {
    match sqlx::query_as::<_, CharacterModelRow>(
        r#"
        SELECT
            model.id,
            model.version,
            model.name,
            model.status,
            character.active_voice_model_id
        FROM character_voice_model AS model
        JOIN "character" AS character ON character.id = model.character_id
        WHERE character.id = $1
        ORDER BY model.version ASC
        "#,
    )
    .bind(character_id)
    .fetch_all(&state.db)
    .await
    {
        Ok(models) => json_response(
            StatusCode::OK,
            ApiResponse::success(
                models
                    .into_iter()
                    .map(|model| CharacterVoiceModelOption {
                        id: version_model_id(model.version),
                        version: model.version,
                        name: model.name,
                        status: model.status,
                        active: model.active_voice_model_id == Some(model.id),
                    })
                    .collect::<Vec<_>>(),
            ),
        ),
        Err(error) => {
            tracing::error!(target: "app::character", %error, %character_id, "Character model list failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to load character models",
            )
        }
    }
}

pub async fn switch_model(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(character_id): Path<Uuid>,
    Json(input): Json<SwitchCharacterModelInput>,
) -> Response {
    if let Err(response) = require_admin(&state, &claims).await {
        return response;
    }

    let requested_id = input.model_id.trim();
    if requested_id.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "Model ID is required");
    }

    let model = if let Some(version) = parse_model_version(requested_id) {
        sqlx::query_as::<
            _,
            (
                Uuid,
                Option<String>,
                i32,
                String,
                Option<String>,
                Option<String>,
                String,
            ),
        >(
            r#"
            SELECT id, model_id, version, name, ckpt_path, pth_path, status
            FROM character_voice_model
            WHERE character_id = $1 AND version = $2
            "#,
        )
        .bind(character_id)
        .bind(version)
        .fetch_optional(&state.db)
        .await
    } else {
        sqlx::query_as::<
            _,
            (
                Uuid,
                Option<String>,
                i32,
                String,
                Option<String>,
                Option<String>,
                String,
            ),
        >(
            r#"
            SELECT id, model_id, version, name, ckpt_path, pth_path, status
            FROM character_voice_model
            WHERE character_id = $1 AND name = $2
            "#,
        )
        .bind(character_id)
        .bind(requested_id)
        .fetch_optional(&state.db)
        .await
    };

    let (model_id, service_model_id, version, model_name, ckpt_path, pth_path, status) = match model
    {
        Ok(Some(model)) => model,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "Voice model not found"),
        Err(error) => {
            tracing::error!(target: "app::character", %error, %character_id, "Character model lookup failed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to load voice model",
            );
        }
    };
    if status != "ready" {
        return error_response(
            StatusCode::CONFLICT,
            "Only a ready voice model can be activated",
        );
    }
    let inference_model_id = service_model_id
        .as_deref()
        .unwrap_or(&model_name)
        .to_owned();

    match sqlx::query(
        r#"
        UPDATE "character"
        SET
            active_voice_model_id = $1,
            voice_model = $2,
            ckpt_path = $3,
            pth_path = $4,
            train_status = $5,
            updated_at = NOW()
        WHERE id = $6
        "#,
    )
    .bind(model_id)
    .bind(service_model_id.as_deref().unwrap_or(&model_name))
    .bind(ckpt_path)
    .bind(pth_path)
    .bind(&status)
    .bind(character_id)
    .execute(&state.db)
    .await
    {
        Ok(result) if result.rows_affected() == 1 => {}
        Ok(_) => return error_response(StatusCode::NOT_FOUND, "Character not found"),
        Err(error) => {
            tracing::error!(target: "app::character", %error, %character_id, "Character model switch failed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to switch character voice model",
            );
        }
    }

    let preload_state = state.clone();
    tokio::spawn(async move {
        let started_at = Instant::now();
        match preload_state
            .voice_training_client
            .post(format!(
                "{}/voice/models/preload",
                preload_state.voice_api_url.trim_end_matches('/')
            ))
            .json(&json!({"model_id": inference_model_id}))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                tracing::info!(
                    target: "app::character",
                    stage = "tts_model_preload",
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "Character voice model preloaded"
                );
            }
            Ok(response) => {
                tracing::warn!(
                    target: "app::character",
                    stage = "tts_model_preload",
                    elapsed_ms = started_at.elapsed().as_millis(),
                    status = response.status().as_u16(),
                    "Character voice model preload failed"
                );
            }
            Err(error) => {
                tracing::warn!(
                    target: "app::character",
                    stage = "tts_model_preload",
                    elapsed_ms = started_at.elapsed().as_millis(),
                    %error,
                    "Character voice model preload service is unavailable"
                );
            }
        }
    });

    json_response(
        StatusCode::OK,
        ApiResponse::success(CharacterVoiceModel {
            id: version_model_id(version),
            version,
            name: model_name,
        }),
    )
}

pub async fn get_binding(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(contact_user_id): Path<Uuid>,
) -> Response {
    let user_id = match user_id(&claims) {
        Ok(user_id) => user_id,
        Err(response) => return response,
    };

    match sqlx::query_as::<_, CharacterBinding>(
        r#"
        SELECT contact_user_id, character_id
        FROM user_contact_character
        WHERE user_id = $1 AND contact_user_id = $2
        "#,
    )
    .bind(user_id)
    .bind(contact_user_id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(binding) => json_response(StatusCode::OK, ApiResponse::success(binding)),
        Err(error) => {
            tracing::error!(target: "app::character", %error, %user_id, %contact_user_id, "Character binding lookup failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to load character binding",
            )
        }
    }
}

pub async fn save_binding(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(contact_user_id): Path<Uuid>,
    Json(input): Json<CharacterBindingInput>,
) -> Response {
    let user_id = match user_id(&claims) {
        Ok(user_id) => user_id,
        Err(response) => return response,
    };
    if user_id == contact_user_id {
        return error_response(
            StatusCode::BAD_REQUEST,
            "A user cannot bind a character to themselves",
        );
    }

    if let Some(character_id) = input.character_id {
        let character_ready = match sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM "character"
                WHERE id = $1 AND voice_model IS NOT NULL AND train_status = 'ready'
            )
            "#,
        )
        .bind(character_id)
        .fetch_one(&state.db)
        .await
        {
            Ok(ready) => ready,
            Err(error) => {
                tracing::error!(target: "app::character", %error, %user_id, %character_id, "Character binding validation failed");
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Unable to validate character",
                );
            }
        };
        if !character_ready {
            return error_response(StatusCode::CONFLICT, "Character voice model is not ready");
        }

        let contact_exists = match sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(SELECT 1 FROM "user" WHERE id = $1)"#,
        )
        .bind(contact_user_id)
        .fetch_one(&state.db)
        .await
        {
            Ok(exists) => exists,
            Err(error) => {
                tracing::error!(target: "app::character", %error, %contact_user_id, "Character binding contact validation failed");
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Unable to validate contact",
                );
            }
        };
        if !contact_exists {
            return error_response(StatusCode::NOT_FOUND, "Contact user not found");
        }

        return match sqlx::query_as::<_, CharacterBinding>(
            r#"
            INSERT INTO user_contact_character (user_id, contact_user_id, character_id)
            VALUES ($1, $2, $3)
            ON CONFLICT (user_id, contact_user_id) DO UPDATE
                SET character_id = EXCLUDED.character_id
            RETURNING contact_user_id, character_id
            "#,
        )
        .bind(user_id)
        .bind(contact_user_id)
        .bind(character_id)
        .fetch_one(&state.db)
        .await
        {
            Ok(binding) => json_response(StatusCode::OK, ApiResponse::success(Some(binding))),
            Err(error) => {
                tracing::error!(target: "app::character", %error, %user_id, %contact_user_id, "Character binding save failed");
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Unable to save character binding",
                )
            }
        };
    }

    match sqlx::query(
        r#"
        DELETE FROM user_contact_character
        WHERE user_id = $1 AND contact_user_id = $2
        "#,
    )
    .bind(user_id)
    .bind(contact_user_id)
    .execute(&state.db)
    .await
    {
        Ok(_) => json_response(
            StatusCode::OK,
            ApiResponse::success(None::<CharacterBinding>),
        ),
        Err(error) => {
            tracing::error!(target: "app::character", %error, %user_id, %contact_user_id, "Character binding removal failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to remove character binding",
            )
        }
    }
}

pub async fn create_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(input): Json<CreateSessionInput>,
) -> Response {
    let user_id = match user_id(&claims) {
        Ok(user_id) => user_id,
        Err(response) => return response,
    };
    let character_exists = match sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM "character"
            WHERE id = $1 AND voice_model IS NOT NULL AND train_status = 'ready'
        )
        "#,
    )
    .bind(input.character_id)
    .fetch_one(&state.db)
    .await
    {
        Ok(exists) => exists,
        Err(error) => {
            tracing::error!(target: "app::character", %error, "Character session validation failed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to validate character",
            );
        }
    };
    if !character_exists {
        return error_response(StatusCode::CONFLICT, "Character voice model is not ready");
    }

    match sqlx::query_as::<_, CharacterSession>(
        r#"
        INSERT INTO character_chat_session (user_id, character_id)
        VALUES ($1, $2)
        RETURNING id, character_id, created_at, updated_at
        "#,
    )
    .bind(user_id)
    .bind(input.character_id)
    .fetch_one(&state.db)
    .await
    {
        Ok(session) => json_response(StatusCode::OK, ApiResponse::success(session)),
        Err(error) => {
            tracing::error!(target: "app::character", %error, %user_id, "Character session creation failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to create character session",
            )
        }
    }
}

pub async fn send_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(input): Json<CharacterMessageInput>,
) -> Response {
    let user_id = match user_id(&claims) {
        Ok(user_id) => user_id,
        Err(response) => return response,
    };
    let message = input.message.trim();
    if message.is_empty() || message.chars().count() > 12_000 {
        return error_response(
            StatusCode::BAD_REQUEST,
            "Message must contain between 1 and 12000 characters",
        );
    }
    let language = match normalized_language(input.language.as_deref()) {
        Ok(language) => language,
        Err(response) => return response,
    };
    let speed_factor = match normalized_speed(input.speed_factor) {
        Ok(speed) => speed,
        Err(response) => return response,
    };

    let session = match sqlx::query_as::<_, SessionCharacter>(
        r#"
        SELECT
            session.id AS session_id,
            character.id AS character_id,
            character.system_prompt,
            character.voice_model,
            character.train_status
        FROM character_chat_session AS session
        JOIN "character" AS character ON character.id = session.character_id
        WHERE session.id = $1 AND session.user_id = $2
        "#,
    )
    .bind(input.session_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(Some(session)) => session,
        Ok(None) => {
            return error_response(StatusCode::NOT_FOUND, "Character session not found");
        }
        Err(error) => {
            tracing::error!(target: "app::character", %error, %user_id, "Character session lookup failed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to load character session",
            );
        }
    };
    if session.train_status != "ready" {
        return error_response(
            StatusCode::CONFLICT,
            "Character voice model training is not complete",
        );
    }
    let Some(voice_model) = session.voice_model.as_deref() else {
        return error_response(
            StatusCode::CONFLICT,
            "Character voice model is not configured",
        );
    };

    let history = match sqlx::query_as::<_, ChatHistoryRow>(
        r#"
        SELECT role, content
        FROM (
            SELECT role, content, created_at
            FROM character_chat_message
            WHERE session_id = $1
            ORDER BY created_at DESC
            LIMIT 20
        ) AS recent
        ORDER BY created_at ASC
        "#,
    )
    .bind(session.session_id)
    .fetch_all(&state.db)
    .await
    {
        Ok(history) => history,
        Err(error) => {
            tracing::error!(target: "app::character", %error, session_id = %session.session_id, "Character history lookup failed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to load character history",
            );
        }
    };

    let payload = json!({
        "message": message,
        "history": history.into_iter().map(|item| json!({
            "role": item.role,
            "content": item.content,
        })).collect::<Vec<_>>(),
        "model_id": voice_model,
        "llm": input.llm,
        "language": language,
        "speed_factor": speed_factor,
        "system_prompt": session.system_prompt,
    });
    let (status, mut body) = match post_json(&state, "/voice/chat", payload).await {
        Ok(result) => result,
        Err(response) => return response,
    };
    rewrite_audio_url(status, &mut body);
    if !status.is_success() {
        return (status, Json(body)).into_response();
    }

    let Some(data) = body.get("data").and_then(Value::as_object) else {
        return error_response(
            StatusCode::BAD_GATEWAY,
            "Voice service returned an invalid chat response",
        );
    };
    let Some(reply_text) = data
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return error_response(
            StatusCode::BAD_GATEWAY,
            "Voice service returned an empty reply",
        );
    };
    let audio_url = data.get("audio_url").and_then(Value::as_str);

    let mut transaction = match state.db.begin().await {
        Ok(transaction) => transaction,
        Err(error) => {
            tracing::error!(target: "app::character", %error, "Character message transaction failed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to save character reply",
            );
        }
    };
    for (role, content, audio) in [
        ("user", message, None),
        ("assistant", reply_text, audio_url),
    ] {
        if let Err(error) = sqlx::query(
            r#"
            INSERT INTO character_chat_message (session_id, role, content, audio_url)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(session.session_id)
        .bind(role)
        .bind(content)
        .bind(audio)
        .execute(&mut *transaction)
        .await
        {
            tracing::error!(target: "app::character", %error, session_id = %session.session_id, "Character message persistence failed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to save character reply",
            );
        }
    }
    if let Err(error) =
        sqlx::query("UPDATE character_chat_session SET updated_at = NOW() WHERE id = $1")
            .bind(session.session_id)
            .execute(&mut *transaction)
            .await
    {
        tracing::error!(target: "app::character", %error, session_id = %session.session_id, "Character session update failed");
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to save character reply",
        );
    }
    if let Err(error) = transaction.commit().await {
        tracing::error!(target: "app::character", %error, session_id = %session.session_id, "Character message commit failed");
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to save character reply",
        );
    }

    if let Some(data) = body.get_mut("data").and_then(Value::as_object_mut) {
        data.insert(
            "session_id".to_owned(),
            Value::String(session.session_id.to_string()),
        );
        data.insert(
            "character_id".to_owned(),
            Value::String(session.character_id.to_string()),
        );
    }
    (status, Json(body)).into_response()
}

pub async fn tts(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(input): Json<TtsInput>,
) -> Response {
    if let Err(response) = user_id(&claims) {
        return response;
    }
    let TtsInput {
        character_id,
        model_id,
        text,
        language,
        speed_factor,
    } = input;
    let text = text.trim();
    if text.is_empty() || text.chars().count() > 12_000 {
        return error_response(
            StatusCode::BAD_REQUEST,
            "TTS text must contain between 1 and 12000 characters",
        );
    }
    let language = match normalized_language(language.as_deref()) {
        Ok(language) => language,
        Err(response) => return response,
    };
    let speed_factor = match normalized_speed(speed_factor) {
        Ok(speed) => speed,
        Err(response) => return response,
    };
    let character = match sqlx::query_as::<_, Character>(
        r#"
        SELECT
            id,
            name,
            avatar_url,
            description,
            system_prompt,
            voice_model,
            ckpt_path,
            pth_path,
            train_status,
            created_at,
            updated_at
        FROM "character"
        WHERE id = $1
        "#,
    )
    .bind(character_id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(Some(character)) => character,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "Character not found"),
        Err(error) => {
            tracing::error!(target: "app::character", %error, %character_id, "Character TTS lookup failed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to load character",
            );
        }
    };
    if character.train_status != "ready" {
        return error_response(
            StatusCode::CONFLICT,
            "Character voice model training is not complete",
        );
    }
    let requested_model_id = normalized_optional(model_id);
    let voice_model = if let Some(requested_model_id) = requested_model_id.as_deref() {
        let model = match sqlx::query_as::<_, (String, String)>(
            r#"
            SELECT COALESCE(model_id, name), status
            FROM character_voice_model
            WHERE character_id = $1
              AND (
                  model_id = $2
                  OR name = $2
                  OR CONCAT('v', version) = $2
              )
            ORDER BY version DESC
            LIMIT 1
            "#,
        )
        .bind(character_id)
        .bind(requested_model_id)
        .fetch_optional(&state.db)
        .await
        {
            Ok(Some(model)) => model,
            Ok(None) => {
                return error_response(StatusCode::NOT_FOUND, "Voice model not found");
            }
            Err(error) => {
                tracing::error!(
                    target: "app::character",
                    %error,
                    %character_id,
                    requested_model_id,
                    "Character TTS model lookup failed"
                );
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Unable to load voice model",
                );
            }
        };
        if model.1 != "ready" {
            return error_response(StatusCode::CONFLICT, "Character voice model is not ready");
        }
        model.0
    } else {
        let Some(voice_model) = character.voice_model else {
            return error_response(
                StatusCode::CONFLICT,
                "Character voice model is not configured",
            );
        };
        voice_model
    };
    let payload = json!({
        "text": text,
        "model_id": &voice_model,
        "language": language,
        "speed_factor": speed_factor,
    });
    let tts_started_at = Instant::now();
    let (status, mut body) = match post_json(&state, "/voice/tts", payload).await {
        Ok(result) => result,
        Err(response) => {
            tracing::warn!(
                target: "app::character",
                stage = "tts_generation",
                elapsed_ms = tts_started_at.elapsed().as_millis(),
                %character_id,
                model_id = %voice_model,
                "Character TTS request failed"
            );
            return response;
        }
    };
    tracing::info!(
        target: "app::character",
        stage = "tts_generation",
        elapsed_ms = tts_started_at.elapsed().as_millis(),
        %character_id,
        model_id = %voice_model,
        status = status.as_u16(),
        "Character TTS request completed"
    );
    rewrite_audio_url(status, &mut body);
    (status, Json(body)).into_response()
}

pub async fn tts_stream(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(input): Query<TtsInput>,
) -> Response {
    if let Err(response) = user_id(&claims) {
        return response;
    }
    let TtsInput {
        character_id,
        model_id,
        text,
        language,
        speed_factor,
    } = input;
    let text = text.trim();
    if text.is_empty() || text.chars().count() > 12_000 {
        return error_response(
            StatusCode::BAD_REQUEST,
            "TTS text must contain between 1 and 12000 characters",
        );
    }
    let language = match normalized_language(language.as_deref()) {
        Ok(language) => language,
        Err(response) => return response,
    };
    let speed_factor = match normalized_speed(speed_factor) {
        Ok(speed) => speed,
        Err(response) => return response,
    };
    let character = match sqlx::query_as::<_, Character>(
        r#"
        SELECT
            id,
            name,
            avatar_url,
            description,
            system_prompt,
            voice_model,
            ckpt_path,
            pth_path,
            train_status,
            created_at,
            updated_at
        FROM "character"
        WHERE id = $1
        "#,
    )
    .bind(character_id)
    .fetch_optional(&state.db)
    .await
    {
        Ok(Some(character)) => character,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "Character not found"),
        Err(error) => {
            tracing::error!(target: "app::character", %error, %character_id, "Character TTS stream lookup failed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to load character",
            );
        }
    };
    if character.train_status != "ready" {
        return error_response(
            StatusCode::CONFLICT,
            "Character voice model training is not complete",
        );
    }
    let requested_model_id = normalized_optional(model_id);
    let voice_model = if let Some(requested_model_id) = requested_model_id.as_deref() {
        let model = match sqlx::query_as::<_, (String, String)>(
            r#"
            SELECT COALESCE(model_id, name), status
            FROM character_voice_model
            WHERE character_id = $1
              AND (
                  model_id = $2
                  OR name = $2
                  OR CONCAT('v', version) = $2
              )
            ORDER BY version DESC
            LIMIT 1
            "#,
        )
        .bind(character_id)
        .bind(requested_model_id)
        .fetch_optional(&state.db)
        .await
        {
            Ok(Some(model)) => model,
            Ok(None) => return error_response(StatusCode::NOT_FOUND, "Voice model not found"),
            Err(error) => {
                tracing::error!(
                    target: "app::character",
                    %error,
                    %character_id,
                    requested_model_id,
                    "Character TTS stream model lookup failed"
                );
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Unable to load voice model",
                );
            }
        };
        if model.1 != "ready" {
            return error_response(StatusCode::CONFLICT, "Character voice model is not ready");
        }
        model.0
    } else {
        let Some(voice_model) = character.voice_model else {
            return error_response(
                StatusCode::CONFLICT,
                "Character voice model is not configured",
            );
        };
        voice_model
    };
    let payload = json!({
        "text": text,
        "model_id": &voice_model,
        "language": language,
        "speed_factor": speed_factor,
    });
    let started_at = Instant::now();
    let upstream = match state
        .voice_training_client
        .post(format!(
            "{}/voice/tts/stream",
            state.voice_api_url.trim_end_matches('/')
        ))
        .json(&payload)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(
                target: "app::character",
                %error,
                %character_id,
                model_id = %voice_model,
                "Unable to reach character TTS stream service"
            );
            return error_response(
                StatusCode::BAD_GATEWAY,
                "Unable to reach the character voice service",
            );
        }
    };
    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    if !status.is_success() {
        tracing::warn!(
            target: "app::character",
            elapsed_ms = started_at.elapsed().as_millis(),
            %character_id,
            model_id = %voice_model,
            upstream_status = status.as_u16(),
            "Character TTS stream request failed"
        );
        return error_response(StatusCode::BAD_GATEWAY, "Character voice streaming failed");
    }
    let content_type = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<HeaderValue>().ok())
        .unwrap_or_else(|| HeaderValue::from_static("audio/aac"));

    tracing::info!(
        target: "app::character",
        elapsed_ms = started_at.elapsed().as_millis(),
        %character_id,
        model_id = %voice_model,
        "Character TTS stream headers received"
    );
    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    response.headers_mut().insert(CONTENT_TYPE, content_type);
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-transform"),
    );
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

const TTS_WEBSOCKET_MAX_CHARS: usize = 48;

fn split_tts_websocket_text(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for character in text.trim().chars() {
        current.push(character);
        let is_boundary = matches!(
            character,
            '。' | '！' | '？' | '!' | '?' | '；' | ';' | '，' | ',' | '：' | ':' | '\n'
        );
        let current_chars = current.trim().chars().count();
        if (is_boundary && current_chars >= 8) || current_chars >= TTS_WEBSOCKET_MAX_CHARS {
            chunks.push(current.trim().to_owned());
            current.clear();
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim().to_owned());
    }
    chunks
}

async fn websocket_voice_model(
    state: &AppState,
    character_id: Uuid,
    requested_model_id: Option<String>,
) -> Result<String, String> {
    let character = sqlx::query_as::<_, (Option<String>, String)>(
        r#"
        SELECT voice_model, train_status
        FROM "character"
        WHERE id = $1
        "#,
    )
    .bind(character_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(target: "app::character", %error, %character_id, "Character TTS WebSocket lookup failed");
        "Unable to load character".to_owned()
    })?
    .ok_or_else(|| "Character not found".to_owned())?;
    if character.1 != "ready" {
        return Err("Character voice model training is not complete".to_owned());
    }

    if let Some(requested_model_id) = normalized_optional(requested_model_id) {
        let model = sqlx::query_as::<_, (String, String)>(
            r#"
            SELECT COALESCE(model_id, name), status
            FROM character_voice_model
            WHERE character_id = $1
              AND (
                  model_id = $2
                  OR name = $2
                  OR CONCAT('v', version) = $2
              )
            ORDER BY version DESC
            LIMIT 1
            "#,
        )
        .bind(character_id)
        .bind(&requested_model_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|error| {
            tracing::error!(
                target: "app::character",
                %error,
                %character_id,
                requested_model_id,
                "Character TTS WebSocket model lookup failed"
            );
            "Unable to load voice model".to_owned()
        })?
        .ok_or_else(|| "Voice model not found".to_owned())?;
        if model.1 != "ready" {
            return Err("Character voice model is not ready".to_owned());
        }
        return Ok(model.0);
    }

    character
        .0
        .ok_or_else(|| "Character voice model is not configured".to_owned())
}

async fn send_tts_websocket_event(socket: &mut WebSocket, value: Value) -> bool {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .is_ok()
}

async fn run_tts_websocket(mut socket: WebSocket, state: AppState) {
    let input_message = match timeout(Duration::from_secs(15), socket.next()).await {
        Ok(Some(Ok(Message::Text(message)))) => message,
        Ok(Some(Ok(_))) => {
            let _ = send_tts_websocket_event(
                &mut socket,
                json!({"type": "error", "message": "Expected a JSON TTS request"}),
            )
            .await;
            return;
        }
        _ => return,
    };
    let input = match serde_json::from_str::<TtsInput>(&input_message) {
        Ok(input) => input,
        Err(_) => {
            let _ = send_tts_websocket_event(
                &mut socket,
                json!({"type": "error", "message": "Invalid TTS request"}),
            )
            .await;
            return;
        }
    };
    let text = input.text.trim().to_owned();
    if text.is_empty() || text.chars().count() > 12_000 {
        let _ = send_tts_websocket_event(
            &mut socket,
            json!({
                "type": "error",
                "message": "TTS text must contain between 1 and 12000 characters"
            }),
        )
        .await;
        return;
    }
    let language = match normalized_language(input.language.as_deref()) {
        Ok(language) => language.to_owned(),
        Err(_) => {
            let _ = send_tts_websocket_event(
                &mut socket,
                json!({"type": "error", "message": "Language must be zh or en"}),
            )
            .await;
            return;
        }
    };
    let speed_factor = match normalized_speed(input.speed_factor) {
        Ok(speed) => speed,
        Err(_) => {
            let _ = send_tts_websocket_event(
                &mut socket,
                json!({
                    "type": "error",
                    "message": "Speed factor must be between 0.5 and 2.0"
                }),
            )
            .await;
            return;
        }
    };
    let voice_model = match websocket_voice_model(&state, input.character_id, input.model_id).await
    {
        Ok(model) => model,
        Err(message) => {
            let _ =
                send_tts_websocket_event(&mut socket, json!({"type": "error", "message": message}))
                    .await;
            return;
        }
    };
    let chunks = split_tts_websocket_text(&text);
    let stream_started_at = Instant::now();
    if !send_tts_websocket_event(
        &mut socket,
        json!({
            "type": "start",
            "segments": chunks.len(),
            "mime_type": "audio/wav",
            "model_id": voice_model,
        }),
    )
    .await
    {
        return;
    }

    for (index, text_chunk) in chunks.iter().enumerate() {
        let segment_started_at = Instant::now();
        let response = match state
            .voice_training_client
            .post(format!(
                "{}/voice/tts/audio",
                state.voice_api_url.trim_end_matches('/')
            ))
            .json(&json!({
                "text": text_chunk,
                "model_id": &voice_model,
                "language": &language,
                "speed_factor": speed_factor,
            }))
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(
                    target: "app::character",
                    %error,
                    model_id = %voice_model,
                    segment = index,
                    "Unable to reach character TTS WebSocket service"
                );
                let _ = send_tts_websocket_event(
                    &mut socket,
                    json!({"type": "error", "message": "Character voice service is unavailable"}),
                )
                .await;
                return;
            }
        };
        if !response.status().is_success() {
            let _ = send_tts_websocket_event(
                &mut socket,
                json!({"type": "error", "message": "Character voice generation failed"}),
            )
            .await;
            return;
        }
        let mime_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("audio/wav")
            .to_owned();
        let cache_status = response
            .headers()
            .get("x-voice-cache")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("unknown")
            .to_owned();
        let audio = match response.bytes().await {
            Ok(audio) if !audio.is_empty() => audio,
            _ => {
                let _ = send_tts_websocket_event(
                    &mut socket,
                    json!({"type": "error", "message": "Character voice service returned no audio"}),
                )
                .await;
                return;
            }
        };
        if !send_tts_websocket_event(
            &mut socket,
            json!({
                "type": "segment",
                "index": index,
                "text": text_chunk,
                "mime_type": mime_type,
                "bytes": audio.len(),
                "cache": cache_status,
                "elapsed_ms": segment_started_at.elapsed().as_millis(),
            }),
        )
        .await
        {
            return;
        }
        if socket.send(Message::Binary(audio)).await.is_err() {
            return;
        }
        tracing::info!(
            target: "app::character",
            stage = "tts_websocket_segment",
            elapsed_ms = segment_started_at.elapsed().as_millis(),
            model_id = %voice_model,
            segment = index,
            "Character TTS WebSocket segment sent"
        );
    }

    let _ = send_tts_websocket_event(
        &mut socket,
        json!({
            "type": "done",
            "segments": chunks.len(),
            "elapsed_ms": stream_started_at.elapsed().as_millis(),
        }),
    )
    .await;
}

pub async fn tts_websocket(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Response {
    if let Err(response) = user_id(&claims) {
        return response;
    }
    ws.on_upgrade(move |socket| run_tts_websocket(socket, state))
}

pub async fn admin_create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(input): Json<CreateCharacterInput>,
) -> Response {
    if let Err(response) = require_admin(&state, &claims).await {
        return response;
    }
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 50 {
        return error_response(
            StatusCode::BAD_REQUEST,
            "Character name must contain between 1 and 50 characters",
        );
    }
    let train_status = input.train_status.as_deref().unwrap_or("pending");
    if !matches!(
        train_status,
        "pending" | "queued" | "training" | "ready" | "awaiting_training_command" | "failed"
    ) {
        return error_response(StatusCode::BAD_REQUEST, "Invalid training status");
    }

    match sqlx::query_as::<_, Character>(
        r#"
        INSERT INTO "character" (
            name,
            avatar_url,
            description,
            system_prompt,
            voice_model,
            ckpt_path,
            pth_path,
            train_status
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING
            id,
            name,
            avatar_url,
            description,
            system_prompt,
            voice_model,
            ckpt_path,
            pth_path,
            train_status,
            created_at,
            updated_at
        "#,
    )
    .bind(name)
    .bind(normalized_optional(input.avatar_url))
    .bind(normalized_optional(input.description))
    .bind(normalized_optional(input.system_prompt))
    .bind(normalized_optional(input.voice_model))
    .bind(normalized_optional(input.ckpt_path))
    .bind(normalized_optional(input.pth_path))
    .bind(train_status)
    .fetch_one(&state.db)
    .await
    {
        Ok(character) => json_response(StatusCode::OK, ApiResponse::success(character)),
        Err(error)
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation()) =>
        {
            error_response(
                StatusCode::CONFLICT,
                "A character already uses this voice model",
            )
        }
        Err(error) => {
            tracing::error!(target: "app::character", %error, "Character creation failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to create character",
            )
        }
    }
}

pub async fn admin_delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(character_id): Path<Uuid>,
) -> Response {
    if let Err(response) = require_admin(&state, &claims).await {
        return response;
    }
    match sqlx::query_scalar::<_, Uuid>(r#"DELETE FROM "character" WHERE id = $1 RETURNING id"#)
        .bind(character_id)
        .fetch_optional(&state.db)
        .await
    {
        Ok(Some(id)) => json_response(StatusCode::OK, ApiResponse::success(json!({ "id": id }))),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "Character not found"),
        Err(error) => {
            tracing::error!(target: "app::character", %error, %character_id, "Character deletion failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to delete character",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TTS_WEBSOCKET_MAX_CHARS, split_tts_websocket_text};

    #[test]
    fn splits_tts_text_at_sentence_boundaries() {
        assert_eq!(
            split_tts_websocket_text("你好，欢迎回来。今天也一起去冒险吧！"),
            vec!["你好，欢迎回来。", "今天也一起去冒险吧！"]
        );
    }

    #[test]
    fn limits_tts_chunks_without_punctuation() {
        let text = "a".repeat(TTS_WEBSOCKET_MAX_CHARS + 7);
        let chunks = split_tts_websocket_text(&text);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), TTS_WEBSOCKET_MAX_CHARS);
        assert_eq!(chunks[1].chars().count(), 7);
    }
}
