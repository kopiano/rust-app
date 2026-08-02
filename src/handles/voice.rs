use axum::{
    body::Body,
    extract::{Json, Path, State},
    http::{
        HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    net::IpAddr,
    time::{Duration, Instant},
};
use uuid::Uuid;

use crate::{app::AppState, common::response::ApiResponse, models::character::Character};

#[derive(Debug, Deserialize, Serialize)]
struct LlmConfig {
    model_name: String,
    api_token: String,
    api_request_url: String,
    effort: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ChatHistoryMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LlmTestRequest {
    llm: LlmConfig,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LlmChatRequest {
    message: String,
    #[serde(default)]
    history: Vec<ChatHistoryMessage>,
    llm: LlmConfig,
    character_id: Option<Uuid>,
    system_prompt: Option<String>,
    #[serde(default)]
    prefer_low_latency: bool,
}

#[derive(Debug, Serialize)]
struct ProviderMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ProviderChatResponse {
    choices: Vec<ProviderChoice>,
}

#[derive(Debug, Deserialize)]
struct ProviderChoice {
    message: ProviderReplyMessage,
}

#[derive(Debug, Deserialize)]
struct ProviderReplyMessage {
    content: Option<String>,
}

struct PreparedLlmChat {
    config: LlmConfig,
    model_name: String,
    messages: Vec<ProviderMessage>,
}

fn downstream_url(state: &AppState, path: &str) -> String {
    format!(
        "{}/{}",
        state.voice_api_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn json_response(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    json_response(
        status,
        serde_json::to_value(ApiResponse::<()>::error(status.as_u16(), message))
            .unwrap_or_else(|_| json!({"code": status.as_u16(), "message": "Voice API error"})),
    )
}

fn invalid_request(message: impl Into<String>) -> Response {
    error_response(StatusCode::BAD_REQUEST, message)
}

fn normalize_required(
    value: String,
    field_name: &str,
    max_chars: usize,
) -> Result<String, Response> {
    let normalized = value.trim().to_string();
    if normalized.is_empty() {
        return Err(invalid_request(format!("{field_name} is required")));
    }
    if normalized.chars().count() > max_chars {
        return Err(invalid_request(format!("{field_name} is too long")));
    }
    Ok(normalized)
}

fn normalize_llm_config(mut config: LlmConfig) -> Result<LlmConfig, Response> {
    config.model_name = normalize_required(config.model_name, "LLM model name", 128)?;
    config.api_token = normalize_required(config.api_token, "LLM API token", 4096)?;
    config.api_request_url = normalize_required(config.api_request_url, "LLM request URL", 2048)?;
    config.effort = normalize_required(config.effort, "Reasoning effort", 32)?.to_lowercase();
    Ok(config)
}

fn is_private_address(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => {
            address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_unspecified()
                || address.is_multicast()
        }
        Ok(IpAddr::V6(address)) => {
            address.is_loopback()
                || address.is_unspecified()
                || address.is_unique_local()
                || address.is_unicast_link_local()
                || address.is_multicast()
        }
        Err(_) => false,
    }
}

fn validate_llm_url(
    value: &str,
    allowed_hosts: &HashSet<String>,
) -> Result<reqwest::Url, Response> {
    let mut url =
        reqwest::Url::parse(value).map_err(|_| invalid_request("Invalid LLM request URL"))?;
    let host = url
        .host_str()
        .map(str::to_lowercase)
        .ok_or_else(|| invalid_request("LLM request URL is not allowed"))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || is_private_address(&host)
        || (!allowed_hosts.contains(&host) && !allowed_hosts.contains("*"))
    {
        return Err(invalid_request("LLM request URL is not allowed"));
    }

    let path = url.path().trim_end_matches('/').to_string();
    if path.is_empty() {
        url.set_path("/chat/completions");
    } else if path.ends_with("/v1") {
        url.set_path(&format!("{path}/chat/completions"));
    }
    url.set_fragment(None);
    Ok(url)
}

fn validate_history(history: Vec<ChatHistoryMessage>) -> Result<Vec<ChatHistoryMessage>, Response> {
    if history.len() > 100 {
        return Err(invalid_request("LLM history contains too many messages"));
    }
    history
        .into_iter()
        .map(|mut item| {
            if item.role != "user" && item.role != "assistant" {
                return Err(invalid_request("Invalid LLM history role"));
            }
            item.content = normalize_required(item.content, "History message", 12_000)?;
            Ok(item)
        })
        .collect()
}

fn low_latency_model_name(configured_model: &str, prefer_low_latency: bool) -> String {
    let _ = prefer_low_latency;
    configured_model.to_string()
}

async fn prepare_llm_chat(
    state: &AppState,
    request: LlmChatRequest,
) -> Result<PreparedLlmChat, Response> {
    let message = normalize_required(request.message, "Message", 12_000)?;
    let history = validate_history(request.history)?;
    let config = normalize_llm_config(request.llm)?;
    let mut system_prompt = match request.system_prompt {
        Some(prompt) => Some(normalize_required(prompt, "System prompt", 12_000)?),
        None => None,
    };
    if let Some(character_id) = request.character_id {
        system_prompt = match sqlx::query_scalar::<_, Option<String>>(
            r#"SELECT system_prompt FROM "character" WHERE id = $1"#,
        )
        .bind(character_id)
        .fetch_optional(&state.db)
        .await
        {
            Ok(Some(system_prompt)) => system_prompt,
            Ok(None) => return Err(error_response(StatusCode::NOT_FOUND, "Character not found")),
            Err(error) => {
                tracing::error!(target: "app::voice", %error, %character_id, "Character prompt lookup failed");
                return Err(error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Unable to load character",
                ));
            }
        };
    }

    let mut messages = vec![ProviderMessage {
        role: "system".to_string(),
        content: system_prompt
            .as_deref()
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty())
            .unwrap_or(&state.character_system_prompt)
            .to_string(),
    }];
    messages.extend(
        history
            .into_iter()
            .rev()
            .take(8)
            .rev()
            .map(|item| ProviderMessage {
                role: item.role,
                content: item.content,
            }),
    );
    messages.push(ProviderMessage {
        role: "user".to_string(),
        content: message,
    });

    let model_name = low_latency_model_name(&config.model_name, request.prefer_low_latency);
    Ok(PreparedLlmChat {
        config,
        model_name,
        messages,
    })
}

fn llm_chat_payload(prepared: &PreparedLlmChat, streaming: bool) -> Value {
    let mut payload = json!({
        "model": prepared.model_name,
        "messages": prepared.messages,
        "temperature": 0.85,
        "max_tokens": 256,
        "stream": streaming,
    });
    if prepared.model_name == "deepseek-v4-flash" {
        payload["reasoning_effort"] = Value::String(prepared.config.effort.clone());
    }
    payload
}

fn provider_error_message(body: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let message = value
        .pointer("/error/message")
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)?
        .trim();
    if message.is_empty() {
        return None;
    }
    Some(message.chars().take(500).collect())
}

async fn send_llm_request(
    state: &AppState,
    config: &LlmConfig,
    payload: Value,
    timeout: Duration,
    failure_message: &'static str,
) -> Result<Value, Response> {
    let url = validate_llm_url(&config.api_request_url, &state.allowed_llm_hosts)?;
    let model_name = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(&config.model_name)
        .to_string();
    let started_at = Instant::now();
    let response = state
        .llm_client
        .post(url)
        .bearer_auth(&config.api_token)
        .json(&payload)
        .timeout(timeout)
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(target: "app::voice", %error, "Unable to reach LLM service");
            error_response(StatusCode::BAD_GATEWAY, "Unable to reach the LLM service")
        })?;
    tracing::info!(
        target: "app::voice",
        stage = "llm_response_headers",
        elapsed_ms = started_at.elapsed().as_millis(),
        model = %model_name,
        "LLM response headers received"
    );
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let detail = provider_error_message(&body);
        tracing::warn!(
            target: "app::voice",
            upstream_status = status.as_u16(),
            upstream_message = detail.as_deref().unwrap_or("unknown"),
            "LLM request rejected"
        );
        let message = detail
            .map(|detail| format!("{failure_message}: {detail}"))
            .unwrap_or_else(|| failure_message.to_string());
        return Err(error_response(status, message));
    }
    let body = response.json::<Value>().await.map_err(|error| {
        tracing::warn!(target: "app::voice", %error, "LLM service returned invalid JSON");
        error_response(StatusCode::BAD_GATEWAY, "LLM returned an invalid response")
    })?;
    tracing::info!(
        target: "app::voice",
        stage = "llm_complete",
        elapsed_ms = started_at.elapsed().as_millis(),
        model = %model_name,
        "LLM response completed"
    );
    Ok(body)
}

async fn character_by_voice_model(
    state: &AppState,
    model_id: &str,
) -> Result<Character, sqlx::Error> {
    sqlx::query_as::<_, Character>(
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
        WHERE voice_model = $1
        "#,
    )
    .bind(model_id)
    .fetch_one(&state.db)
    .await
}

pub(crate) async fn post_json(
    state: &AppState,
    path: &str,
    payload: Value,
) -> Result<(StatusCode, Value), Response> {
    let response = reqwest::Client::new()
        .post(downstream_url(state, path))
        .json(&payload)
        .send()
        .await
        .map_err(|_| error_response(StatusCode::BAD_GATEWAY, "Voice service is unavailable"))?;
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let body = response.json::<Value>().await.map_err(|_| {
        error_response(
            StatusCode::BAD_GATEWAY,
            "Voice service returned invalid JSON",
        )
    })?;
    Ok((status, body))
}

pub async fn test_llm(
    State(state): State<AppState>,
    Json(request): Json<LlmTestRequest>,
) -> Response {
    let config = match normalize_llm_config(request.llm) {
        Ok(config) => config,
        Err(response) => return response,
    };
    let mut payload = json!({
        "model": config.model_name,
        "messages": [{"role": "user", "content": "Reply with OK only."}],
        "temperature": 0,
        "max_tokens": 64,
        "stream": false,
    });
    if config.model_name == "deepseek-v4-flash" {
        payload["reasoning_effort"] = Value::String(config.effort.clone());
    }
    match send_llm_request(
        &state,
        &config,
        payload,
        Duration::from_secs(30),
        "LLM connection failed",
    )
    .await
    {
        Ok(_) => json_response(
            StatusCode::OK,
            json!({"code": 200, "message": "LLM connected", "data": {"connected": true}}),
        ),
        Err(response) => response,
    }
}

pub async fn llm_chat(
    State(state): State<AppState>,
    Json(request): Json<LlmChatRequest>,
) -> Response {
    let prepared = match prepare_llm_chat(&state, request).await {
        Ok(prepared) => prepared,
        Err(response) => return response,
    };
    let payload = llm_chat_payload(&prepared, false);
    match send_llm_request(
        &state,
        &prepared.config,
        payload,
        Duration::from_secs(120),
        "LLM request failed",
    )
    .await
    {
        Ok(body) => {
            let provider_response = match serde_json::from_value::<ProviderChatResponse>(body) {
                Ok(response) => response,
                Err(_) => {
                    return error_response(
                        StatusCode::BAD_GATEWAY,
                        "LLM returned an invalid response",
                    );
                }
            };
            let Some(text) = provider_response
                .choices
                .first()
                .and_then(|choice| choice.message.content.as_deref())
                .map(str::trim)
                .filter(|text| !text.is_empty())
            else {
                return error_response(StatusCode::BAD_GATEWAY, "LLM returned an empty response");
            };
            json_response(
                StatusCode::OK,
                json!({
                    "code": 200,
                    "message": "LLM reply generated",
                    "data": {"text": text},
                }),
            )
        }
        Err(response) => response,
    }
}

pub async fn llm_chat_stream(
    State(state): State<AppState>,
    Json(request): Json<LlmChatRequest>,
) -> Response {
    let prepared = match prepare_llm_chat(&state, request).await {
        Ok(prepared) => prepared,
        Err(response) => return response,
    };
    let url = match validate_llm_url(&prepared.config.api_request_url, &state.allowed_llm_hosts) {
        Ok(url) => url,
        Err(response) => return response,
    };
    let payload = llm_chat_payload(&prepared, true);
    let started_at = Instant::now();
    let response = match state
        .llm_client
        .post(url)
        .bearer_auth(&prepared.config.api_token)
        .json(&payload)
        .timeout(Duration::from_secs(120))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(target: "app::voice", %error, "Unable to reach LLM service");
            return error_response(StatusCode::BAD_GATEWAY, "Unable to reach the LLM service");
        }
    };
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    tracing::info!(
        target: "app::voice",
        stage = "llm_stream_headers",
        elapsed_ms = started_at.elapsed().as_millis(),
        model = %prepared.model_name,
        upstream_status = status.as_u16(),
        "LLM stream response headers received"
    );
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let detail = provider_error_message(&body);
        tracing::warn!(
            target: "app::voice",
            upstream_status = status.as_u16(),
            upstream_message = detail.as_deref().unwrap_or("unknown"),
            "LLM stream request rejected"
        );
        let message = detail
            .map(|detail| format!("LLM request failed: {detail}"))
            .unwrap_or_else(|| "LLM request failed".to_string());
        return error_response(status, message);
    }

    let model_name = prepared.model_name;
    let output = stream::unfold(
        (response.bytes_stream(), false, started_at, model_name),
        |(mut upstream, first_chunk_seen, started_at, model_name)| async move {
            match upstream.next().await {
                Some(Ok(bytes)) => {
                    if !first_chunk_seen {
                        tracing::info!(
                            target: "app::voice",
                            stage = "llm_first_chunk",
                            elapsed_ms = started_at.elapsed().as_millis(),
                            model = %model_name,
                            "LLM first stream chunk received"
                        );
                    }
                    Some((
                        Ok::<_, reqwest::Error>(bytes),
                        (upstream, true, started_at, model_name),
                    ))
                }
                Some(Err(error)) => {
                    tracing::warn!(
                        target: "app::voice",
                        stage = "llm_stream_error",
                        elapsed_ms = started_at.elapsed().as_millis(),
                        model = %model_name,
                        %error,
                        "LLM stream failed"
                    );
                    Some((
                        Err(error),
                        (upstream, first_chunk_seen, started_at, model_name),
                    ))
                }
                None => {
                    tracing::info!(
                        target: "app::voice",
                        stage = "llm_stream_complete",
                        elapsed_ms = started_at.elapsed().as_millis(),
                        model = %model_name,
                        "LLM stream completed"
                    );
                    None
                }
            }
        },
    );

    let mut response = Response::new(Body::from_stream(output));
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

pub async fn voice_chat(State(state): State<AppState>, Json(mut payload): Json<Value>) -> Response {
    let Some(model_id) = payload
        .get("model_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return error_response(StatusCode::BAD_REQUEST, "Voice model ID is required");
    };
    let character = match character_by_voice_model(&state, model_id).await {
        Ok(character) => character,
        Err(sqlx::Error::RowNotFound) => {
            return error_response(StatusCode::NOT_FOUND, "Character voice model not found");
        }
        Err(error) => {
            tracing::error!(target: "app::voice", %error, model_id, "Character lookup failed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to load character voice model",
            );
        }
    };
    if character.train_status != "ready" {
        return error_response(
            StatusCode::CONFLICT,
            "Character voice model training is not complete",
        );
    }
    if let Some(system_prompt) = character
        .system_prompt
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        && let Some(object) = payload.as_object_mut()
    {
        object.insert(
            "system_prompt".to_owned(),
            Value::String(system_prompt.to_owned()),
        );
    }

    let result = match post_json(&state, "/voice/chat", payload).await {
        Ok(result) => result,
        Err(response) => return response,
    };
    let (status, mut body) = result;

    rewrite_audio_url(status, &mut body);

    json_response(status, body)
}

pub(crate) fn rewrite_audio_url(status: StatusCode, body: &mut Value) {
    if !status.is_success() {
        return;
    }
    let Some(audio_url) = body
        .get_mut("data")
        .and_then(Value::as_object_mut)
        .and_then(|data| data.get_mut("audio_url"))
        .and_then(|value| value.as_str())
        .map(str::to_owned)
    else {
        return;
    };
    let Some(filename) = audio_url.rsplit('/').next() else {
        return;
    };
    if filename.is_empty() || filename.contains(['?', '#']) {
        return;
    }
    let encoded = urlencoding::encode(filename);
    if let Some(data) = body.get_mut("data").and_then(Value::as_object_mut) {
        data.insert(
            "audio_url".to_owned(),
            Value::String(format!("/api/voice/media/{encoded}")),
        );
    }
}

pub async fn media(State(state): State<AppState>, Path(filename): Path<String>) -> Response {
    if filename.is_empty()
        || filename.contains('/')
        || filename.contains('\\')
        || filename == "."
        || filename == ".."
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let response = match reqwest::Client::new()
        .get(downstream_url(&state, &format!("/media/{filename}")))
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<HeaderValue>().ok());
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let mut output = Response::new(Body::from(bytes));
    *output.status_mut() = status;
    if let Some(content_type) = content_type {
        output.headers_mut().insert(CONTENT_TYPE, content_type);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        LlmConfig, PreparedLlmChat, ProviderMessage, is_private_address, llm_chat_payload,
        low_latency_model_name, provider_error_message, validate_llm_url,
    };
    use std::collections::HashSet;

    fn allowed_hosts() -> HashSet<String> {
        ["api.deepseek.com", "api.openai.com"]
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn normalizes_llm_base_urls() {
        let hosts = allowed_hosts();
        let deepseek = validate_llm_url("https://api.deepseek.com", &hosts).unwrap();
        let openai = validate_llm_url("https://api.openai.com/v1/", &hosts).unwrap();

        assert_eq!(
            deepseek.as_str(),
            "https://api.deepseek.com/chat/completions"
        );
        assert_eq!(
            openai.as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn preserves_explicit_llm_endpoint() {
        let url = validate_llm_url(
            "https://api.deepseek.com/chat/completions",
            &allowed_hosts(),
        )
        .unwrap();

        assert_eq!(url.as_str(), "https://api.deepseek.com/chat/completions");
    }

    #[test]
    fn rejects_unapproved_or_insecure_llm_urls() {
        let hosts = allowed_hosts();

        assert!(validate_llm_url("http://api.deepseek.com", &hosts).is_err());
        assert!(validate_llm_url("https://example.com/v1", &hosts).is_err());
        assert!(validate_llm_url("https://user@api.deepseek.com/v1", &hosts).is_err());
    }

    #[test]
    fn rejects_private_network_addresses() {
        assert!(is_private_address("localhost"));
        assert!(is_private_address("127.0.0.1"));
        assert!(is_private_address("10.0.0.1"));
        assert!(is_private_address("::1"));
        assert!(!is_private_address("8.8.8.8"));
    }

    #[test]
    fn keeps_configured_model_for_low_latency_auto_replies() {
        assert_eq!(
            low_latency_model_name("deepseek-v4-flash", true),
            "deepseek-v4-flash"
        );
        assert_eq!(
            low_latency_model_name("custom-chat-model", true),
            "custom-chat-model"
        );
        assert_eq!(
            low_latency_model_name("deepseek-v4-flash", false),
            "deepseek-v4-flash"
        );
    }

    #[test]
    fn chat_payload_limits_output_and_enables_streaming() {
        let prepared = PreparedLlmChat {
            config: LlmConfig {
                model_name: "deepseek-v4-flash".to_string(),
                api_token: "token".to_string(),
                api_request_url: "https://api.deepseek.com".to_string(),
                effort: "low".to_string(),
            },
            model_name: "deepseek-v4-flash".to_string(),
            messages: vec![ProviderMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
            }],
        };

        let payload = llm_chat_payload(&prepared, true);

        assert_eq!(payload["model"], "deepseek-v4-flash");
        assert_eq!(payload["max_tokens"], 256);
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["reasoning_effort"], "low");
    }

    #[test]
    fn extracts_provider_error_messages() {
        assert_eq!(
            provider_error_message(r#"{"error":{"message":"Model not found"}}"#).as_deref(),
            Some("Model not found")
        );
        assert_eq!(
            provider_error_message(r#"{"message":"Invalid token"}"#).as_deref(),
            Some("Invalid token")
        );
        assert_eq!(provider_error_message("not json"), None);
    }
}
