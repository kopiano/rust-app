use axum::{
    Json,
    body::Body,
    extract::{Extension, Multipart, Path, State},
    http::{
        HeaderValue, StatusCode,
        header::{CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Write},
    path::{Path as FsPath, PathBuf},
};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::{app::AppState, common::response::ApiResponse, middleware::jwt::Claims};

const MAX_AUDIO_FILES: usize = 500;
const MAX_TEXT_CHARS: usize = 50;
const ALLOWED_AUDIO_SUFFIXES: &[&str] = &["wav", "mp3", "flac", "ogg", "m4a"];

#[derive(Debug, Clone, FromRow)]
struct TrainingJob {
    id: Uuid,
    character_id: Uuid,
    voice_model_id: Uuid,
    remote_job_id: Option<String>,
    model_id: String,
    nickname: String,
    version_name: String,
    status: String,
    progress: i32,
    stage: String,
    error: Option<String>,
    dataset_path: String,
    artifact_archive_path: Option<String>,
    manifest: Option<Value>,
    remote_acknowledged: bool,
}

#[derive(Debug, Serialize)]
struct TrainingJobResponse {
    id: Uuid,
    model_id: String,
    status: String,
    progress: i32,
    stage: String,
    error: Option<String>,
    artifacts_ready: bool,
    acknowledged: bool,
}

#[derive(Debug, Deserialize)]
struct RemoteJob {
    job_id: String,
    status: String,
    #[serde(default)]
    progress: i32,
    #[serde(default = "queued_stage")]
    stage: String,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ArtifactManifest {
    artifacts: Vec<ArtifactEntry>,
}

#[derive(Debug, Deserialize)]
struct ArtifactEntry {
    name: String,
    sha256: String,
    size: u64,
}

fn queued_stage() -> String {
    "queued".to_string()
}

fn json_response(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

fn success<T: Serialize>(data: T) -> Response {
    Json(ApiResponse::success(data)).into_response()
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    json_response(
        status,
        serde_json::to_value(ApiResponse::<()>::error(status.as_u16(), message))
            .unwrap_or_else(|_| json!({"code": status.as_u16(), "message": "Training API error"})),
    )
}

fn user_id(claims: &Claims) -> Result<Uuid, Response> {
    claims
        .sub
        .parse()
        .map_err(|_| error_response(StatusCode::UNAUTHORIZED, "Invalid user identity"))
}

fn normalize_text(value: String, field: &str) -> Result<String, Response> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            format!("{field} is required"),
        ));
    }
    if value.chars().count() > MAX_TEXT_CHARS {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            format!("{field} is too long"),
        ));
    }
    Ok(value)
}

fn safe_name(value: &str) -> String {
    let normalized = value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let normalized = normalized.trim_matches(['-', '_']).to_lowercase();
    if normalized.is_empty() {
        format!("character-{}", &Uuid::new_v4().simple().to_string()[..8])
    } else {
        normalized.chars().take(64).collect()
    }
}

fn safe_filename(value: &str) -> Option<String> {
    let filename = FsPath::new(value).file_name()?.to_str()?.trim();
    if filename.is_empty() || filename == "." || filename == ".." {
        return None;
    }
    Some(filename.to_string())
}

fn remote_url(state: &AppState, path: &str) -> String {
    format!(
        "{}/{}",
        state.voice_training_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn authorize(state: &AppState, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    match state.voice_training_token.as_deref() {
        Some(token) => request.bearer_auth(token),
        None => request,
    }
}

fn response_data(value: Value) -> Value {
    value.get("data").cloned().unwrap_or(value)
}

fn public_job(job: &TrainingJob) -> TrainingJobResponse {
    TrainingJobResponse {
        id: job.id,
        model_id: job.model_id.clone(),
        status: job.status.clone(),
        progress: job.progress,
        stage: job.stage.clone(),
        error: job.error.clone(),
        artifacts_ready: job.artifact_archive_path.is_some(),
        acknowledged: job.remote_acknowledged,
    }
}

async fn load_job(state: &AppState, job_id: Uuid, user_id: Uuid) -> Result<TrainingJob, Response> {
    sqlx::query_as::<_, TrainingJob>(
        r#"
        SELECT id, character_id, voice_model_id, remote_job_id,
               model_id, nickname, version_name, status, progress, stage,
               error, dataset_path, artifact_archive_path, manifest,
               remote_acknowledged
        FROM voice_training_job
        WHERE id = $1 AND user_id = $2
        "#,
    )
    .bind(job_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(target: "app::voice_training", %error, %job_id, "Training job lookup failed");
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to load training job",
        )
    })?
    .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Training job not found"))
}

async fn save_field(
    mut field: axum::extract::multipart::Field<'_>,
    destination: &FsPath,
) -> Result<(), Response> {
    let mut output = tokio::fs::File::create(destination).await.map_err(|error| {
        tracing::error!(target: "app::voice_training", %error, path = %destination.display(), "Unable to create upload");
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to save training upload",
        )
    })?;
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Unable to read training upload"))?
    {
        output.write_all(&chunk).await.map_err(|_| {
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to save training upload",
            )
        })?;
    }
    output.flush().await.map_err(|_| {
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to save training upload",
        )
    })
}

async fn stream_part(path: &FsPath, filename: String) -> Result<Part, String> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|error| format!("Unable to open {}: {error}", path.display()))?;
    let length = file
        .metadata()
        .await
        .map_err(|error| format!("Unable to inspect {}: {error}", path.display()))?
        .len();
    Ok(
        Part::stream_with_length(reqwest::Body::wrap_stream(ReaderStream::new(file)), length)
            .file_name(filename),
    )
}

async fn mark_failed(state: &AppState, job_id: Uuid, message: String) {
    let _ = sqlx::query(
        r#"
        UPDATE voice_training_job
        SET status = 'failed', stage = 'failed', error = $2, updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .bind(&message)
    .execute(&state.db)
    .await;
    let _ = sqlx::query(
        r#"
        UPDATE character_voice_model AS model
        SET status = 'failed', updated_at = NOW()
        FROM voice_training_job AS job
        WHERE job.id = $1 AND model.id = job.voice_model_id
        "#,
    )
    .bind(job_id)
    .execute(&state.db)
    .await;
}

async fn dispatch_job(state: AppState, job_id: Uuid) -> Result<(), String> {
    let job = sqlx::query_as::<_, TrainingJob>(
        r#"
        SELECT id, character_id, voice_model_id, remote_job_id,
               model_id, nickname, version_name, status, progress, stage,
               error, dataset_path, artifact_archive_path, manifest,
               remote_acknowledged
        FROM voice_training_job WHERE id = $1
        "#,
    )
    .bind(job_id)
    .fetch_one(&state.db)
    .await
    .map_err(|error| error.to_string())?;
    let dataset_dir = PathBuf::from(&job.dataset_path);
    let list_path = dataset_dir.join("dataset.list");
    let audio_dir = dataset_dir.join("audio");
    let mut audio_paths = Vec::new();
    let mut entries = tokio::fs::read_dir(&audio_dir)
        .await
        .map_err(|error| error.to_string())?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| error.to_string())?
    {
        if entry
            .file_type()
            .await
            .map_err(|error| error.to_string())?
            .is_file()
        {
            audio_paths.push(entry.path());
        }
    }
    audio_paths.sort();

    sqlx::query(
        "UPDATE voice_training_job SET status = 'uploading', stage = 'uploading', updated_at = NOW() WHERE id = $1",
    )
    .bind(job_id)
    .execute(&state.db)
    .await
    .map_err(|error| error.to_string())?;

    let manifest = json!({
        "local_job_id": job.id,
        "model_id": job.model_id,
        "nickname": job.nickname,
        "version_name": job.version_name,
        "audio_count": audio_paths.len(),
    });
    let list_part = stream_part(&list_path, "dataset.list".to_string()).await?;
    let mut form = Form::new()
        .text("model_id", job.model_id.clone())
        .text("nickname", job.nickname.clone())
        .text("version_name", job.version_name.clone())
        .text("manifest", manifest.to_string())
        .part("list_file", list_part);
    for audio_path in audio_paths {
        let filename = audio_path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| "Invalid audio filename".to_string())?
            .to_string();
        form = form.part("audio_files", stream_part(&audio_path, filename).await?);
    }

    let response = authorize(
        &state,
        state
            .voice_training_client
            .post(remote_url(&state, "/jobs"))
            .multipart(form),
    )
    .send()
    .await
    .map_err(|error| format!("Remote training service is unavailable: {error}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        return Err(format!(
            "Remote training submission failed ({status}): {detail}"
        ));
    }
    let value = response
        .json::<Value>()
        .await
        .map_err(|error| format!("Remote training response is invalid: {error}"))?;
    let remote: RemoteJob = serde_json::from_value(response_data(value))
        .map_err(|error| format!("Remote training response is invalid: {error}"))?;
    sqlx::query(
        r#"
        UPDATE voice_training_job
        SET remote_job_id = $2, status = $3, progress = $4, stage = $5,
            error = $6, updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .bind(remote.job_id)
    .bind(if remote.status == "ready" {
        "training"
    } else {
        remote.status.as_str()
    })
    .bind(remote.progress.clamp(0, 99))
    .bind(remote.stage)
    .bind(remote.error)
    .execute(&state.db)
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

pub async fn resume_pending_jobs(state: &AppState) {
    let job_ids = match sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT id
        FROM voice_training_job
        WHERE remote_job_id IS NULL
          AND status IN ('queued', 'uploading')
        ORDER BY created_at
        "#,
    )
    .fetch_all(&state.db)
    .await
    {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(target: "app::voice_training", %error, "Unable to load pending training jobs");
            return;
        }
    };
    for job_id in job_ids {
        let state_for_task = state.clone();
        tokio::spawn(async move {
            if let Err(message) = dispatch_job(state_for_task.clone(), job_id).await {
                tracing::error!(target: "app::voice_training", %job_id, %message, "Pending training dispatch failed");
                mark_failed(&state_for_task, job_id, message).await;
            }
        });
    }
}

pub async fn create_job(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    mut multipart: Multipart,
) -> Response {
    let user_id = match user_id(&claims) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let job_id = Uuid::new_v4();
    let incoming_dir = state
        .voice_train_dir
        .join(".incoming")
        .join(job_id.to_string());
    let incoming_audio_dir = incoming_dir.join("audio");
    if let Err(error) = tokio::fs::create_dir_all(&incoming_audio_dir).await {
        tracing::error!(target: "app::voice_training", %error, "Unable to create training upload directory");
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to prepare training upload",
        );
    }

    let mut nickname = None;
    let mut version_name = None;
    let mut list_received = false;
    let mut audio_names = HashSet::new();
    let mut audio_count = 0usize;
    while let Ok(Some(field)) = multipart.next_field().await {
        let field_name = field.name().unwrap_or_default().to_string();
        match field_name.as_str() {
            "nickname" => match field.text().await {
                Ok(value) => nickname = Some(value),
                Err(_) => {
                    return error_response(StatusCode::BAD_REQUEST, "Invalid nickname field");
                }
            },
            "version_name" => match field.text().await {
                Ok(value) => version_name = Some(value),
                Err(_) => {
                    return error_response(StatusCode::BAD_REQUEST, "Invalid version name field");
                }
            },
            "list_file" => {
                if list_received {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        "Only one List file is allowed",
                    );
                }
                if let Err(response) = save_field(field, &incoming_dir.join("dataset.list")).await {
                    return response;
                }
                list_received = true;
            }
            "audio_files" => {
                if audio_count >= MAX_AUDIO_FILES {
                    return error_response(StatusCode::BAD_REQUEST, "Too many audio files");
                }
                let Some(filename) = field.file_name().and_then(safe_filename) else {
                    return error_response(StatusCode::BAD_REQUEST, "Invalid audio filename");
                };
                let suffix = FsPath::new(&filename)
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default()
                    .to_lowercase();
                if !ALLOWED_AUDIO_SUFFIXES.contains(&suffix.as_str()) {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        format!("Unsupported audio file: {filename}"),
                    );
                }
                if !audio_names.insert(filename.to_lowercase()) {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        format!("Duplicate audio filename: {filename}"),
                    );
                }
                if let Err(response) = save_field(field, &incoming_audio_dir.join(&filename)).await
                {
                    return response;
                }
                audio_count += 1;
            }
            _ => {}
        }
    }
    let nickname = match normalize_text(nickname.unwrap_or_default(), "Nickname") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let version_name = match normalize_text(version_name.unwrap_or_default(), "Version name") {
        Ok(value) => value,
        Err(response) => return response,
    };
    if !list_received || audio_count == 0 {
        return error_response(
            StatusCode::BAD_REQUEST,
            "List file and audio files are required",
        );
    }

    let mut transaction = match state.db.begin().await {
        Ok(value) => value,
        Err(_) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to create training job",
            );
        }
    };
    if sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(&nickname)
        .execute(&mut *transaction)
        .await
        .is_err()
    {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to lock character training version",
        );
    }
    let character_id = match sqlx::query_scalar::<_, Uuid>(
        r#"SELECT id FROM "character" WHERE name = $1 FOR UPDATE"#,
    )
    .bind(&nickname)
    .fetch_optional(&mut *transaction)
    .await
    {
        Ok(Some(id)) => id,
        Ok(None) => match sqlx::query_scalar::<_, Uuid>(
            r#"
            INSERT INTO "character" (name, train_status)
            VALUES ($1, 'queued')
            RETURNING id
            "#,
        )
        .bind(&nickname)
        .fetch_one(&mut *transaction)
        .await
        {
            Ok(id) => id,
            Err(error) => {
                tracing::error!(target: "app::voice_training", %error, "Character lookup failed");
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Unable to create training job",
                );
            }
        },
        Err(error) => {
            tracing::error!(target: "app::voice_training", %error, "Character creation failed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to create training job",
            );
        }
    };
    let version = match sqlx::query_scalar::<_, i32>(
        "SELECT COALESCE(MAX(version), 0) + 1 FROM character_voice_model WHERE character_id = $1",
    )
    .bind(character_id)
    .fetch_one(&mut *transaction)
    .await
    {
        Ok(value) => value,
        Err(_) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to allocate model version",
            );
        }
    };
    let model_id = format!("{}-v{version}", safe_name(&nickname));
    let dataset_dir = state
        .voice_train_dir
        .join(safe_name(&nickname))
        .join(format!("v{version}"));
    if dataset_dir.exists() {
        return error_response(
            StatusCode::CONFLICT,
            "Training version directory already exists",
        );
    }
    if let Some(parent) = dataset_dir.parent()
        && let Err(error) = tokio::fs::create_dir_all(parent).await
    {
        tracing::error!(target: "app::voice_training", %error, "Unable to create model directory");
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to prepare model directory",
        );
    }
    if let Err(error) = tokio::fs::rename(&incoming_dir, &dataset_dir).await {
        tracing::error!(target: "app::voice_training", %error, "Unable to publish training upload");
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to persist training upload",
        );
    }
    let dataset_path = dataset_dir.to_string_lossy().to_string();
    let voice_model_id = match sqlx::query_scalar::<_, Uuid>(
        r#"
        INSERT INTO character_voice_model (
            character_id, model_id, version, name, dataset_path, status
        )
        VALUES ($1, $2, $3, $4, $5, 'queued')
        RETURNING id
        "#,
    )
    .bind(character_id)
    .bind(&model_id)
    .bind(version)
    .bind(&version_name)
    .bind(&dataset_path)
    .fetch_one(&mut *transaction)
    .await
    {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(target: "app::voice_training", %error, "Voice model creation failed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to create voice model",
            );
        }
    };
    if let Err(error) = sqlx::query(
        r#"
        INSERT INTO voice_training_job (
            id, user_id, character_id, voice_model_id, model_id, nickname,
            version_name, dataset_path
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(job_id)
    .bind(user_id)
    .bind(character_id)
    .bind(voice_model_id)
    .bind(&model_id)
    .bind(&nickname)
    .bind(&version_name)
    .bind(&dataset_path)
    .execute(&mut *transaction)
    .await
    {
        tracing::error!(target: "app::voice_training", %error, "Training job creation failed");
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to create training job",
        );
    }
    if transaction.commit().await.is_err() {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unable to commit training job",
        );
    }

    let state_for_task = state.clone();
    tokio::spawn(async move {
        if let Err(message) = dispatch_job(state_for_task.clone(), job_id).await {
            tracing::error!(target: "app::voice_training", %job_id, %message, "Training dispatch failed");
            mark_failed(&state_for_task, job_id, message).await;
        }
    });
    success(TrainingJobResponse {
        id: job_id,
        model_id,
        status: "queued".to_string(),
        progress: 0,
        stage: "queued".to_string(),
        error: None,
        artifacts_ready: false,
        acknowledged: false,
    })
}

async fn sync_remote_status(state: &AppState, job: &TrainingJob) -> Result<(), String> {
    let Some(remote_job_id) = job.remote_job_id.as_deref() else {
        return Ok(());
    };
    let response = authorize(
        state,
        state
            .voice_training_client
            .get(remote_url(state, &format!("/jobs/{remote_job_id}"))),
    )
    .send()
    .await
    .map_err(|error| format!("Unable to query remote training job: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Remote training status failed with {}",
            response.status()
        ));
    }
    let value = response
        .json::<Value>()
        .await
        .map_err(|error| format!("Remote training status is invalid: {error}"))?;
    let remote: RemoteJob = serde_json::from_value(response_data(value))
        .map_err(|error| format!("Remote training status is invalid: {error}"))?;
    let status = match remote.status.as_str() {
        "completed" | "ready" => "downloading",
        "failed" => "failed",
        "queued" | "uploading" | "training" => remote.status.as_str(),
        _ => "training",
    };
    sqlx::query(
        r#"
        UPDATE voice_training_job
        SET status = $2, progress = $3, stage = $4, error = $5, updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job.id)
    .bind(status)
    .bind(if status == "downloading" {
        99
    } else {
        remote.progress.clamp(0, 99)
    })
    .bind(if status == "downloading" {
        "downloading"
    } else {
        remote.stage.as_str()
    })
    .bind(remote.error)
    .execute(&state.db)
    .await
    .map_err(|error| error.to_string())?;
    if status == "failed" {
        sqlx::query(
            "UPDATE character_voice_model SET status = 'failed', updated_at = NOW() WHERE id = $1",
        )
        .bind(job.voice_model_id)
        .execute(&state.db)
        .await
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn extract_artifacts(
    archive_path: &FsPath,
    dataset_dir: &FsPath,
) -> Result<(Value, PathBuf, PathBuf), String> {
    let file = File::open(archive_path).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    let mut manifest_bytes = Vec::new();
    archive
        .by_name("manifest.json")
        .map_err(|_| "Artifact archive does not contain manifest.json".to_string())?
        .read_to_end(&mut manifest_bytes)
        .map_err(|error| error.to_string())?;
    let manifest_value: Value =
        serde_json::from_slice(&manifest_bytes).map_err(|error| error.to_string())?;
    let manifest: ArtifactManifest =
        serde_json::from_value(manifest_value.clone()).map_err(|error| error.to_string())?;
    let models_dir = dataset_dir.join("models");
    std::fs::create_dir_all(&models_dir).map_err(|error| error.to_string())?;
    let mut pth_path = None;
    let mut ckpt_path = None;
    for expected in manifest.artifacts {
        let filename = safe_filename(&expected.name)
            .ok_or_else(|| format!("Invalid artifact name: {}", expected.name))?;
        let allowed = filename.ends_with(".pth")
            || filename.ends_with(".ckpt")
            || filename == "training.log"
            || filename == "manifest.json";
        if !allowed {
            return Err(format!("Unexpected artifact: {filename}"));
        }
        let mut entry = archive
            .by_name(&expected.name)
            .map_err(|_| format!("Artifact is missing: {}", expected.name))?;
        if entry.size() != expected.size {
            return Err(format!("Artifact size mismatch: {}", expected.name));
        }
        let destination = if filename.ends_with(".pth") || filename.ends_with(".ckpt") {
            models_dir.join(&filename)
        } else {
            dataset_dir.join(&filename)
        };
        let temporary = destination.with_extension(format!(
            "{}.part",
            destination
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
        ));
        let mut output = File::create(&temporary).map_err(|error| error.to_string())?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 1024 * 1024];
        loop {
            let count = entry.read(&mut buffer).map_err(|error| error.to_string())?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            output
                .write_all(&buffer[..count])
                .map_err(|error| error.to_string())?;
        }
        output.flush().map_err(|error| error.to_string())?;
        let digest = format!("{:x}", hasher.finalize());
        if !digest.eq_ignore_ascii_case(&expected.sha256) {
            return Err(format!("Artifact checksum mismatch: {}", expected.name));
        }
        std::fs::rename(&temporary, &destination).map_err(|error| error.to_string())?;
        if filename.ends_with(".pth") {
            pth_path = Some(destination);
        } else if filename.ends_with(".ckpt") {
            ckpt_path = Some(destination);
        }
    }
    let pth_path = pth_path.ok_or_else(|| "PTH artifact is missing".to_string())?;
    let ckpt_path = ckpt_path.ok_or_else(|| "CKPT artifact is missing".to_string())?;
    std::fs::write(
        dataset_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest_value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok((manifest_value, pth_path, ckpt_path))
}

async fn download_artifacts(state: &AppState, job: &TrainingJob) -> Result<(), String> {
    let remote_job_id = job
        .remote_job_id
        .as_deref()
        .ok_or_else(|| "Remote job ID is missing".to_string())?;
    let response = authorize(
        state,
        state.voice_training_client.get(remote_url(
            state,
            &format!("/jobs/{remote_job_id}/artifacts"),
        )),
    )
    .send()
    .await
    .map_err(|error| format!("Unable to download training artifacts: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Remote artifact download failed with {}",
            response.status()
        ));
    }
    let dataset_dir = PathBuf::from(&job.dataset_path);
    let archive_path = dataset_dir.join(format!("{}-artifacts.zip", job.model_id));
    let temporary_path = dataset_dir.join(format!(".{}-artifacts.zip.part", job.model_id));
    let mut output = tokio::fs::File::create(&temporary_path)
        .await
        .map_err(|error| error.to_string())?;
    let mut stream = response.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        output
            .write_all(&chunk.map_err(|error| error.to_string())?)
            .await
            .map_err(|error| error.to_string())?;
    }
    output.flush().await.map_err(|error| error.to_string())?;
    tokio::fs::rename(&temporary_path, &archive_path)
        .await
        .map_err(|error| error.to_string())?;
    let archive_for_extract = archive_path.clone();
    let dataset_for_extract = dataset_dir.clone();
    let (manifest, pth_path, ckpt_path) = tokio::task::spawn_blocking(move || {
        extract_artifacts(&archive_for_extract, &dataset_for_extract)
    })
    .await
    .map_err(|error| error.to_string())??;
    let mut transaction = state.db.begin().await.map_err(|error| error.to_string())?;
    sqlx::query(
        r#"
        UPDATE voice_training_job
        SET status = 'ready', progress = 100, stage = 'ready', error = NULL,
            artifact_archive_path = $2, manifest = $3, updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job.id)
    .bind(archive_path.to_string_lossy().to_string())
    .bind(manifest)
    .execute(&mut *transaction)
    .await
    .map_err(|error| error.to_string())?;
    sqlx::query(
        r#"
        UPDATE character_voice_model
        SET pth_path = $2, ckpt_path = $3, status = 'ready', updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job.voice_model_id)
    .bind(pth_path.to_string_lossy().to_string())
    .bind(ckpt_path.to_string_lossy().to_string())
    .execute(&mut *transaction)
    .await
    .map_err(|error| error.to_string())?;
    sqlx::query(
        r#"
        UPDATE "character"
        SET active_voice_model_id = COALESCE(active_voice_model_id, $2),
            voice_model = CASE WHEN active_voice_model_id IS NULL THEN $3 ELSE voice_model END,
            pth_path = CASE WHEN active_voice_model_id IS NULL THEN $4 ELSE pth_path END,
            ckpt_path = CASE WHEN active_voice_model_id IS NULL THEN $5 ELSE ckpt_path END,
            train_status = 'ready',
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job.character_id)
    .bind(job.voice_model_id)
    .bind(&job.model_id)
    .bind(pth_path.to_string_lossy().to_string())
    .bind(ckpt_path.to_string_lossy().to_string())
    .execute(&mut *transaction)
    .await
    .map_err(|error| error.to_string())?;
    transaction
        .commit()
        .await
        .map_err(|error| error.to_string())
}

pub async fn get_job(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(job_id): Path<Uuid>,
) -> Response {
    let user_id = match user_id(&claims) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mut job = match load_job(&state, job_id, user_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    if !matches!(job.status.as_str(), "ready" | "failed") {
        if let Err(message) = sync_remote_status(&state, &job).await {
            tracing::warn!(target: "app::voice_training", %job_id, %message, "Remote status refresh failed");
        }
        job = match load_job(&state, job_id, user_id).await {
            Ok(value) => value,
            Err(response) => return response,
        };
        if job.status == "downloading" {
            match download_artifacts(&state, &job).await {
                Ok(()) => {
                    job = match load_job(&state, job_id, user_id).await {
                        Ok(value) => value,
                        Err(response) => return response,
                    };
                }
                Err(message) => {
                    tracing::warn!(target: "app::voice_training", %job_id, %message, "Artifact persistence will be retried");
                    let _ = sqlx::query(
                        r#"
                        UPDATE voice_training_job
                        SET status = 'downloading', stage = 'downloading',
                            error = $2, updated_at = NOW()
                        WHERE id = $1
                        "#,
                    )
                    .bind(job_id)
                    .bind(message)
                    .execute(&state.db)
                    .await;
                    job = match load_job(&state, job_id, user_id).await {
                        Ok(value) => value,
                        Err(response) => return response,
                    };
                }
            }
        }
    }
    success(public_job(&job))
}

pub async fn get_artifacts(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(job_id): Path<Uuid>,
) -> Response {
    let user_id = match user_id(&claims) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let job = match load_job(&state, job_id, user_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    if job.status != "ready" {
        return error_response(StatusCode::CONFLICT, "Training artifacts are not ready");
    }
    let Some(path) = job.artifact_archive_path.as_deref().map(PathBuf::from) else {
        return error_response(StatusCode::NOT_FOUND, "Training artifacts not found");
    };
    let file = match tokio::fs::File::open(&path).await {
        Ok(value) => value,
        Err(_) => return error_response(StatusCode::NOT_FOUND, "Training artifacts not found"),
    };
    let length = file.metadata().await.map(|value| value.len()).unwrap_or(0);
    let mut response = Response::new(Body::from_stream(ReaderStream::new(file)));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/zip"));
    if let Ok(value) = HeaderValue::from_str(&length.to_string()) {
        response.headers_mut().insert(CONTENT_LENGTH, value);
    }
    if let Ok(value) = HeaderValue::from_str(&format!(
        "attachment; filename=\"{}-artifacts.zip\"",
        job.model_id
    )) {
        response.headers_mut().insert(CONTENT_DISPOSITION, value);
    }
    response
}

pub async fn ack_job(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(job_id): Path<Uuid>,
) -> Response {
    let user_id = match user_id(&claims) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let job = match load_job(&state, job_id, user_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    if job.status != "ready" || job.artifact_archive_path.is_none() || job.manifest.is_none() {
        return error_response(
            StatusCode::CONFLICT,
            "Artifacts must be saved locally before acknowledgement",
        );
    }
    if !job.remote_acknowledged {
        let Some(remote_job_id) = job.remote_job_id.as_deref() else {
            return error_response(StatusCode::CONFLICT, "Remote job ID is missing");
        };
        let response = authorize(
            &state,
            state
                .voice_training_client
                .post(remote_url(&state, &format!("/jobs/{remote_job_id}/ack"))),
        )
        .send()
        .await;
        match response {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => {
                return error_response(
                    StatusCode::BAD_GATEWAY,
                    format!("Remote acknowledgement failed with {}", response.status()),
                );
            }
            Err(_) => {
                return error_response(
                    StatusCode::BAD_GATEWAY,
                    "Remote training service is unavailable",
                );
            }
        }
        if let Err(error) = sqlx::query(
            r#"
            UPDATE voice_training_job
            SET remote_acknowledged = TRUE, acknowledged_at = NOW(), updated_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(job_id)
        .execute(&state.db)
        .await
        {
            tracing::error!(target: "app::voice_training", %error, %job_id, "Training acknowledgement persistence failed");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to save acknowledgement",
            );
        }
    }
    let updated = match load_job(&state, job_id, user_id).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    success(public_job(&updated))
}
