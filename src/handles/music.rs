use std::{
    collections::HashMap,
    io::Cursor,
    path::{Path, PathBuf},
    process::Stdio,
    time::Instant,
};

use axum::{
    Extension, Json,
    extract::{
        Multipart, Path as AxumPath, State, WebSocketUpgrade,
        multipart::Field,
        ws::{Message as WsMessage, WebSocket},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgb};
use serde::Deserialize;
use tokio::{io::AsyncWriteExt, process::Command, sync::Semaphore};
use uuid::Uuid;

use crate::{
    app::AppState,
    common::response::ApiResponse,
    middleware::jwt::Claims,
    models::music::{
        Music, MusicFavoriteState, MusicListItem, MusicProcessingBroadcast, UpdateMusicFavorite,
    },
};

const MUSIC_DIRECTORY: &str = "src/assets/music";
const MAX_AUDIO_BYTES: u64 = 1024 * 1024 * 1024;
const AAC_BITRATE: &str = "256k";
const ALLOWED_EXTENSIONS: &[&str] = &["mp3", "m4a", "aac", "flac", "wav", "ogg", "opus"];
static AUDIO_TRANSCODE_SLOTS: Semaphore = Semaphore::const_new(2);

#[derive(Debug, Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    codec_type: Option<String>,
    sample_rate: Option<String>,
    bit_rate: Option<String>,
    duration: Option<String>,
    #[serde(default)]
    tags: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
    bit_rate: Option<String>,
    #[serde(default)]
    tags: HashMap<String, String>,
}

struct AudioMetadata {
    title: String,
    artist: String,
    album: String,
    duration_ms: i64,
    bitrate: i32,
    sample_rate: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoverOutcome {
    Embedded,
    Fallback,
}

#[derive(Clone)]
struct MusicUpload {
    id: Uuid,
    directory: PathBuf,
    title: String,
    original_url: String,
    original_format: String,
    original_size: i64,
}

pub async fn public_list(
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<MusicListItem>>>, StatusCode> {
    let music = sqlx::query_as::<_, MusicListItem>(
        r#"
        SELECT id, title, artist, album, cover_url, is_favorite,
               processing_status, processing_error, created_at
        FROM music
        ORDER BY created_at DESC, id DESC
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, "Failed to list public music");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(ApiResponse::success(music)))
}

pub async fn public_get(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<ApiResponse<Music>>, StatusCode> {
    let music = sqlx::query_as::<_, Music>(
        r#"
        SELECT id, title, artist, album, duration_ms, bitrate, sample_rate,
               cover_url, audio_url, original_url, format, original_format,
               size, original_size, is_favorite, processing_status,
               processing_error, created_at, updated_at
        FROM music
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, music_id = %id, "Failed to get public music");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(ApiResponse::success(music)))
}

pub async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<ApiResponse<Vec<MusicListItem>>>, StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    let music = sqlx::query_as::<_, MusicListItem>(
        r#"
        SELECT id, title, artist, album, cover_url, is_favorite,
               processing_status, processing_error, created_at
        FROM music
        WHERE user_id = $1
        ORDER BY created_at DESC, id DESC
        "#,
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, %user_id, "Failed to list music");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(ApiResponse::success(music)))
}

pub async fn websocket(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let user_id = match authenticated_user_id(&claims) {
        Ok(user_id) => user_id,
        Err(status) => return status.into_response(),
    };

    upgrade
        .on_upgrade(move |socket| music_websocket_session(socket, state, user_id))
        .into_response()
}

async fn music_websocket_session(mut socket: WebSocket, state: AppState, user_id: Uuid) {
    let mut events = state.music_tx.subscribe();

    loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Ok(event) if event.user_id == user_id => {
                        let payload = match serde_json::to_string(&event) {
                            Ok(payload) => payload,
                            Err(_) => continue,
                        };
                        if socket.send(WsMessage::Text(payload.into())).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Ok(WsMessage::Ping(payload))) => {
                        if socket.send(WsMessage::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(WsMessage::Text(_)))
                    | Some(Ok(WsMessage::Pong(_)))
                    | Some(Ok(WsMessage::Binary(_))) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }
}

pub async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<ApiResponse<Music>>, StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    let music = sqlx::query_as::<_, Music>(
        r#"
        SELECT id, title, artist, album, duration_ms, bitrate, sample_rate,
               cover_url, audio_url, original_url, format, original_format,
               size, original_size, is_favorite, processing_status,
               processing_error, created_at, updated_at
        FROM music
        WHERE id = $1 AND user_id = $2
        "#,
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, %user_id, music_id = %id, "Failed to get music");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(ApiResponse::success(music)))
}

pub async fn upload(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<ApiResponse<Vec<Music>>>), StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    let mut uploads = Vec::new();
    let mut found_file = false;

    loop {
        let next_field = match multipart.next_field().await {
            Ok(field) => field,
            Err(_) => {
                cleanup_uploads(&uploads).await;
                return Err(StatusCode::BAD_REQUEST);
            }
        };
        let Some(mut field) = next_field else {
            break;
        };
        if field.name() != Some("files") {
            continue;
        }
        found_file = true;

        match prepare_upload(&mut field).await {
            Ok(upload) => uploads.push(upload),
            Err(status) => {
                cleanup_uploads(&uploads).await;
                return Err(status);
            }
        }
    }

    if !found_file || uploads.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut transaction = match state.db.begin().await {
        Ok(transaction) => transaction,
        Err(error) => {
            tracing::error!(%error, %user_id, "Failed to begin music upload transaction");
            cleanup_uploads(&uploads).await;
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let mut created = Vec::with_capacity(uploads.len());
    for upload in &uploads {
        match insert_processing_music(&mut transaction, user_id, upload).await {
            Ok(music) => created.push(music),
            Err(status) => {
                let _ = transaction.rollback().await;
                cleanup_uploads(&uploads).await;
                return Err(status);
            }
        }
    }
    if let Err(error) = transaction.commit().await {
        tracing::error!(%error, %user_id, "Failed to commit music upload transaction");
        cleanup_uploads(&uploads).await;
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    for upload in uploads {
        spawn_music_processing(state.db.clone(), state.music_tx.clone(), user_id, upload);
    }

    Ok((StatusCode::CREATED, Json(ApiResponse::success(created))))
}

pub async fn favorite(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<Uuid>,
    Json(input): Json<UpdateMusicFavorite>,
) -> Result<Json<ApiResponse<MusicFavoriteState>>, StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    let state = sqlx::query_as::<_, MusicFavoriteState>(
        r#"
        UPDATE music
        SET is_favorite = $1, updated_at = NOW()
        WHERE id = $2 AND user_id = $3
        RETURNING id, is_favorite
        "#,
    )
    .bind(input.favorite)
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, %user_id, music_id = %id, "Failed to update music favorite");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(ApiResponse::success(state)))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<ApiResponse<()>>, StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    let deleted_id = sqlx::query_scalar::<_, Uuid>(
        r#"
        DELETE FROM music
        WHERE id = $1 AND user_id = $2
        RETURNING id
        "#,
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, %user_id, music_id = %id, "Failed to delete music");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    let directory = PathBuf::from(MUSIC_DIRECTORY).join(deleted_id.to_string());
    match tokio::fs::remove_dir_all(&directory).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::error!(
                %error,
                %user_id,
                music_id = %deleted_id,
                path = %directory.display(),
                "Music database row was deleted but local files could not be removed"
            );
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    tracing::info!(%user_id, music_id = %deleted_id, "Music and local files deleted");
    Ok(Json(ApiResponse::success(())))
}

fn authenticated_user_id(claims: &Claims) -> Result<Uuid, StatusCode> {
    claims
        .sub
        .parse::<Uuid>()
        .map_err(|_| StatusCode::UNAUTHORIZED)
}

async fn prepare_upload(field: &mut Field<'_>) -> Result<MusicUpload, StatusCode> {
    let original_name = field
        .file_name()
        .map(str::to_owned)
        .ok_or(StatusCode::BAD_REQUEST)?;
    let original_format = audio_extension(&original_name)
        .ok_or(StatusCode::UNSUPPORTED_MEDIA_TYPE)?
        .to_owned();
    let id = Uuid::new_v4();
    let directory = PathBuf::from(MUSIC_DIRECTORY).join(id.to_string());
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| {
            tracing::error!(%error, path = %directory.display(), "Failed to create music directory");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let result = async {
        let original_path = directory.join(format!("original.{original_format}"));
        let original_size = save_audio_field(field, &original_path).await?;
        let fallback_title = filename_title(&original_name);

        let base_url = format!("/api/assets/music/{id}");
        let original_url = format!("{base_url}/original.{original_format}");
        Ok(MusicUpload {
            id,
            directory: directory.clone(),
            title: fallback_title,
            original_url,
            original_format,
            original_size,
        })
    }
    .await;

    if result.is_err() {
        cleanup_directory(&directory).await;
    }
    result
}

async fn insert_processing_music(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    music: &MusicUpload,
) -> Result<Music, StatusCode> {
    sqlx::query_as::<_, Music>(
        r#"
        INSERT INTO music (
            id, user_id, title, artist, album, duration_ms, bitrate,
            sample_rate, cover_url, audio_url, original_url, format,
            original_format, size, original_size, processing_status
        )
        VALUES (
            $1, $2, $3, 'Unknown Artist', 'Unknown Album', 0, 0, 0,
            '', '', $4, $5, $5, 0, $6, 'processing'
        )
        RETURNING id, title, artist, album, duration_ms, bitrate, sample_rate,
                  cover_url, audio_url, original_url, format, original_format,
                  size, original_size, is_favorite, processing_status,
                  processing_error, created_at, updated_at
        "#,
    )
    .bind(music.id)
    .bind(user_id)
    .bind(&music.title)
    .bind(&music.original_url)
    .bind(&music.original_format)
    .bind(music.original_size)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| {
        tracing::error!(%error, %user_id, music_id = %music.id, "Failed to save processing music");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

async fn save_audio_field(field: &mut Field<'_>, path: &Path) -> Result<i64, StatusCode> {
    let mut file = tokio::fs::File::create(path).await.map_err(|error| {
        tracing::error!(%error, path = %path.display(), "Failed to create original audio");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let mut total = 0_u64;

    while let Some(chunk) = field.chunk().await.map_err(|_| StatusCode::BAD_REQUEST)? {
        total = total
            .checked_add(chunk.len() as u64)
            .ok_or(StatusCode::PAYLOAD_TOO_LARGE)?;
        if total > MAX_AUDIO_BYTES {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        file.write_all(&chunk).await.map_err(|error| {
            tracing::error!(%error, path = %path.display(), "Failed to write original audio");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    }
    file.flush()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if total == 0 {
        return Err(StatusCode::BAD_REQUEST);
    }
    i64::try_from(total).map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)
}

async fn probe_audio(path: &Path) -> Result<ProbeOutput, StatusCode> {
    let ffprobe = std::env::var("FFPROBE_PATH").unwrap_or_else(|_| "ffprobe".to_owned());
    let output = Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration,bit_rate:format_tags=title,artist,album:stream=codec_type,sample_rate,bit_rate,duration:stream_tags=title,artist,album",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .await
        .map_err(|error| {
            tracing::error!(%error, executable = %ffprobe, "Failed to start FFprobe");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    if !output.status.success() {
        tracing::warn!(
            path = %path.display(),
            stderr = %String::from_utf8_lossy(&output.stderr),
            "FFprobe rejected audio"
        );
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    serde_json::from_slice(&output.stdout).map_err(|error| {
        tracing::error!(%error, path = %path.display(), "Failed to parse FFprobe output");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

async fn transcode_to_aac(source: &Path, output: &Path) -> Result<(), StatusCode> {
    let _permit = AUDIO_TRANSCODE_SLOTS
        .acquire()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let ffmpeg = std::env::var("FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".to_owned());
    let result = Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(source)
        .args([
            "-map",
            "0:a:0",
            "-vn",
            "-c:a",
            "aac",
            "-b:a",
            AAC_BITRATE,
            "-map_metadata",
            "0",
            "-movflags",
            "+faststart",
        ])
        .arg(output)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| {
            tracing::error!(%error, executable = %ffmpeg, "Failed to start FFmpeg");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    if !result.status.success() {
        tracing::warn!(
            source = %source.display(),
            stderr = %String::from_utf8_lossy(&result.stderr),
            "AAC transcoding failed"
        );
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    Ok(())
}

async fn has_embedded_cover(source: &Path) -> Result<bool, StatusCode> {
    let ffprobe = std::env::var("FFPROBE_PATH").unwrap_or_else(|_| "ffprobe".to_owned());
    let result = Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=index",
            "-of",
            "csv=p=0",
        ])
        .arg(source)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| {
            tracing::error!(%error, executable = %ffprobe, "Failed to inspect embedded music cover");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if !result.status.success() {
        tracing::warn!(
            source = %source.display(),
            stderr = %String::from_utf8_lossy(&result.stderr),
            "FFprobe failed while inspecting embedded music cover"
        );
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }

    Ok(!result.stdout.iter().all(u8::is_ascii_whitespace))
}

async fn create_cover(source: &Path, output: &Path, id: Uuid) -> Result<CoverOutcome, StatusCode> {
    let has_embedded_cover = has_embedded_cover(source).await?;
    if !has_embedded_cover {
        let id_bytes = *id.as_bytes();
        let encoded = tokio::task::spawn_blocking(move || encode_cover(None, id_bytes))
            .await
            .map_err(|error| {
                tracing::error!(%error, "Fallback cover encoding task failed");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
            .map_err(|error| {
                tracing::error!(%error, "Failed to encode fallback WebP cover");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        tokio::fs::write(output, encoded).await.map_err(|error| {
            tracing::error!(%error, path = %output.display(), "Failed to save fallback WebP cover");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        tracing::info!(
            music_id = %id,
            source = %source.display(),
            "Music has no embedded artwork; generated fallback cover"
        );
        return Ok(CoverOutcome::Fallback);
    }

    let ffmpeg = std::env::var("FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".to_owned());
    let extracted = Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(source)
        .args([
            "-an",
            "-map",
            "0:v:0",
            "-frames:v",
            "1",
            "-f",
            "image2pipe",
            "-vcodec",
            "png",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|error| {
            tracing::error!(%error, executable = %ffmpeg, music_id = %id, "Failed to extract embedded music cover");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if !extracted.status.success() || extracted.stdout.is_empty() {
        tracing::error!(
            music_id = %id,
            source = %source.display(),
            status = ?extracted.status.code(),
            stderr = %String::from_utf8_lossy(&extracted.stderr),
            "Embedded music cover extraction failed"
        );
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }

    let extracted_bytes = extracted.stdout;
    let encoded =
        tokio::task::spawn_blocking(move || encode_cover(Some(&extracted_bytes), [0; 16]))
            .await
            .map_err(|error| {
                tracing::error!(%error, music_id = %id, "Cover encoding task failed");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
            .map_err(|error| {
                tracing::error!(%error, music_id = %id, "Failed to encode embedded WebP cover");
                StatusCode::UNPROCESSABLE_ENTITY
            })?;

    tokio::fs::write(output, encoded).await.map_err(|error| {
        tracing::error!(%error, path = %output.display(), "Failed to save WebP cover");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(CoverOutcome::Embedded)
}

fn spawn_music_processing(
    db: sqlx::PgPool,
    music_tx: tokio::sync::broadcast::Sender<MusicProcessingBroadcast>,
    user_id: Uuid,
    music: MusicUpload,
) {
    tokio::spawn(async move {
        let started_at = Instant::now();
        let original_path = music
            .directory
            .join(format!("original.{}", music.original_format));
        let audio_path = music.directory.join("audio.m4a");
        let cover_path = music.directory.join("cover-processed.webp");
        let (source_probe_result, transcode_result, cover_result) = tokio::join!(
            probe_audio(&original_path),
            transcode_to_aac(&original_path, &audio_path),
            create_cover(&original_path, &cover_path, music.id),
        );

        let source_probe = match source_probe_result {
            Ok(probe) => probe,
            Err(status) => {
                tracing::error!(music_id = %music.id, %status, "Failed to probe uploaded music");
                mark_music_processing_failed(
                    &db,
                    &music_tx,
                    user_id,
                    music.id,
                    "Unsupported or invalid audio file",
                )
                .await;
                return;
            }
        };
        let (cover_ready, preserve_original) = match cover_result {
            Ok(outcome) => {
                tracing::info!(music_id = %music.id, ?outcome, "Music cover processing completed");
                (true, false)
            }
            Err(status) => {
                tracing::warn!(
                    music_id = %music.id,
                    %status,
                    "Music cover processing failed; preserving original audio for retry"
                );
                (false, true)
            }
        };
        if let Err(status) = transcode_result {
            tracing::error!(music_id = %music.id, %status, "Background music transcoding failed");
            mark_music_processing_failed(
                &db,
                &music_tx,
                user_id,
                music.id,
                "Audio transcoding failed",
            )
            .await;
            return;
        }

        let output_probe = match probe_audio(&audio_path).await {
            Ok(probe) => probe,
            Err(status) => {
                tracing::error!(music_id = %music.id, %status, "Failed to probe transcoded music");
                mark_music_processing_failed(
                    &db,
                    &music_tx,
                    user_id,
                    music.id,
                    "Transcoded audio validation failed",
                )
                .await;
                return;
            }
        };
        let source_metadata = metadata_from_probe(&source_probe, &music.title);
        let output_metadata = metadata_from_probe(&output_probe, &source_metadata.title);
        let output_size = match file_size(&audio_path).await {
            Ok(size) => size,
            Err(status) => {
                tracing::error!(music_id = %music.id, %status, "Failed to read transcoded music size");
                mark_music_processing_failed(
                    &db,
                    &music_tx,
                    user_id,
                    music.id,
                    "Failed to read processed audio",
                )
                .await;
                return;
            }
        };

        match sqlx::query_as::<_, Music>(
            r#"
            UPDATE music
            SET title = $2,
                artist = $3,
                album = $4,
                duration_ms = $5,
                bitrate = $6,
                sample_rate = $7,
                audio_url = $8,
                original_url = $8,
                format = 'm4a',
                original_format = 'm4a',
                size = $9,
                original_size = $9,
                cover_url = CASE WHEN $10 THEN $11 ELSE cover_url END,
                processing_status = 'ready',
                processing_error = NULL,
                updated_at = NOW()
            WHERE id = $1 AND user_id = $12
            RETURNING id, title, artist, album, duration_ms, bitrate, sample_rate,
                      cover_url, audio_url, original_url, format, original_format,
                      size, original_size, is_favorite, processing_status,
                      processing_error, created_at, updated_at
            "#,
        )
        .bind(music.id)
        .bind(preferred_text(&source_metadata.title, &music.title))
        .bind(preferred_text(&source_metadata.artist, "Unknown Artist"))
        .bind(preferred_text(&source_metadata.album, "Unknown Album"))
        .bind(output_metadata.duration_ms.max(source_metadata.duration_ms))
        .bind(output_metadata.bitrate)
        .bind(output_metadata.sample_rate)
        .bind(format!("/api/assets/music/{}/audio.m4a", music.id))
        .bind(output_size)
        .bind(cover_ready)
        .bind(format!(
            "/api/assets/music/{}/cover-processed.webp",
            music.id
        ))
        .bind(user_id)
        .fetch_optional(&db)
        .await
        {
            Ok(None) => {
                tracing::warn!(music_id = %music.id, "Transcoded music no longer has a database row");
            }
            Ok(Some(published)) => {
                if !preserve_original {
                    if let Err(error) = tokio::fs::remove_file(&original_path).await
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        tracing::warn!(
                            %error,
                            music_id = %music.id,
                            path = %original_path.display(),
                            "Failed to remove original music after publishing AAC"
                        );
                    }
                }
                tracing::info!(
                    music_id = %music.id,
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "Background music processing completed"
                );
                publish_music_processing_event(&music_tx, user_id, published);
            }
            Err(error) => {
                tracing::error!(%error, music_id = %music.id, "Failed to publish transcoded music");
                mark_music_processing_failed(
                    &db,
                    &music_tx,
                    user_id,
                    music.id,
                    "Failed to publish processed audio",
                )
                .await;
            }
        }
    });
}

async fn mark_music_processing_failed(
    db: &sqlx::PgPool,
    music_tx: &tokio::sync::broadcast::Sender<MusicProcessingBroadcast>,
    user_id: Uuid,
    id: Uuid,
    message: &str,
) {
    match sqlx::query_as::<_, Music>(
        r#"
        UPDATE music
        SET processing_status = 'failed',
            processing_error = $2,
            updated_at = NOW()
        WHERE id = $1 AND user_id = $3
        RETURNING id, title, artist, album, duration_ms, bitrate, sample_rate,
                  cover_url, audio_url, original_url, format, original_format,
                  size, original_size, is_favorite, processing_status,
                  processing_error, created_at, updated_at
        "#,
    )
    .bind(id)
    .bind(message)
    .bind(user_id)
    .fetch_optional(db)
    .await
    {
        Ok(Some(music)) => publish_music_processing_event(music_tx, user_id, music),
        Ok(None) => {}
        Err(error) => {
            tracing::error!(%error, music_id = %id, "Failed to mark music processing as failed");
        }
    }
}

fn publish_music_processing_event(
    music_tx: &tokio::sync::broadcast::Sender<MusicProcessingBroadcast>,
    user_id: Uuid,
    music: Music,
) {
    let event = MusicProcessingBroadcast {
        user_id,
        event: "music.processing",
        id: music.id,
        status: music.processing_status.clone(),
        audio_url: music.audio_url.clone(),
        music,
    };
    let _ = music_tx.send(event);
}

fn encode_cover(bytes: Option<&[u8]>, seed: [u8; 16]) -> image::ImageResult<Vec<u8>> {
    let image = match bytes.and_then(|value| image::load_from_memory(value).ok()) {
        Some(image) => image,
        None => fallback_cover(seed),
    };
    let resized = image.resize_to_fill(900, 900, image::imageops::FilterType::Lanczos3);
    let mut encoded = Cursor::new(Vec::new());
    resized.write_to(&mut encoded, ImageFormat::WebP)?;
    Ok(encoded.into_inner())
}

fn fallback_cover(seed: [u8; 16]) -> DynamicImage {
    let base = [
        seed[0].saturating_add(48),
        seed[5].saturating_add(36),
        seed[10].saturating_add(52),
    ];
    let accent = [
        seed[3].saturating_add(70),
        seed[8].saturating_add(48),
        seed[13].saturating_add(64),
    ];
    let image = ImageBuffer::from_fn(900, 900, |x, y| {
        let diagonal = ((x + y) as f32 / 1800.0).clamp(0.0, 1.0);
        let radial_x = x as f32 - 520.0;
        let radial_y = y as f32 - 360.0;
        let glow =
            (1.0 - ((radial_x * radial_x + radial_y * radial_y).sqrt() / 720.0)).clamp(0.0, 1.0);
        let mix = (diagonal * 0.58 + glow * 0.42).clamp(0.0, 1.0);
        Rgb([
            blend(base[0], accent[0], mix),
            blend(base[1], accent[1], mix),
            blend(base[2], accent[2], mix),
        ])
    });
    DynamicImage::ImageRgb8(image)
}

fn blend(start: u8, end: u8, amount: f32) -> u8 {
    (start as f32 + (end as f32 - start as f32) * amount).round() as u8
}

fn metadata_from_probe(probe: &ProbeOutput, fallback_title: &str) -> AudioMetadata {
    let empty_tags = HashMap::new();
    let audio_stream = probe
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("audio"));
    let format_tags = probe
        .format
        .as_ref()
        .map(|format| &format.tags)
        .unwrap_or(&empty_tags);
    let stream_tags = audio_stream.map(|stream| &stream.tags);

    let tag = |name: &str| {
        tag_value(format_tags, name)
            .or_else(|| stream_tags.and_then(|tags| tag_value(tags, name)))
            .map(str::to_owned)
    };
    let duration = probe
        .format
        .as_ref()
        .and_then(|format| parse_number(format.duration.as_deref()))
        .or_else(|| audio_stream.and_then(|stream| parse_number(stream.duration.as_deref())))
        .unwrap_or(0.0);
    let bitrate = audio_stream
        .and_then(|stream| parse_integer(stream.bit_rate.as_deref()))
        .or_else(|| {
            probe
                .format
                .as_ref()
                .and_then(|format| parse_integer(format.bit_rate.as_deref()))
        })
        .unwrap_or(0);
    let sample_rate = audio_stream
        .and_then(|stream| parse_integer(stream.sample_rate.as_deref()))
        .unwrap_or(0);

    AudioMetadata {
        title: tag("title").unwrap_or_else(|| fallback_title.to_owned()),
        artist: tag("artist").unwrap_or_else(|| "Unknown Artist".to_owned()),
        album: tag("album").unwrap_or_else(|| "Unknown Album".to_owned()),
        duration_ms: (duration * 1000.0).round().max(0.0) as i64,
        bitrate: bitrate.clamp(0, i32::MAX as i64) as i32,
        sample_rate: sample_rate.clamp(0, i32::MAX as i64) as i32,
    }
}

fn tag_value<'a>(tags: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    tags.iter()
        .find(|(key, value)| key.eq_ignore_ascii_case(name) && !value.trim().is_empty())
        .map(|(_, value)| value.trim())
}

fn parse_number(value: Option<&str>) -> Option<f64> {
    value?
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
}

fn parse_integer(value: Option<&str>) -> Option<i64> {
    value?.parse::<i64>().ok()
}

fn audio_extension(filename: &str) -> Option<&str> {
    let extension = Path::new(filename).extension()?.to_str()?;
    ALLOWED_EXTENSIONS
        .iter()
        .copied()
        .find(|allowed| extension.eq_ignore_ascii_case(allowed))
}

fn filename_title(filename: &str) -> String {
    Path::new(filename)
        .file_stem()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.starts_with('.'))
        .unwrap_or("Untitled")
        .to_owned()
}

fn preferred_text(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value.to_owned()
    }
}

async fn file_size(path: &Path) -> Result<i64, StatusCode> {
    let size = tokio::fs::metadata(path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .len();
    i64::try_from(size).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn cleanup_directory(path: &Path) {
    if let Err(error) = tokio::fs::remove_dir_all(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, path = %path.display(), "Failed to clean incomplete music upload");
    }
}

async fn cleanup_uploads(uploads: &[MusicUpload]) {
    for item in uploads {
        cleanup_directory(&item.directory).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CoverOutcome, ProbeFormat, ProbeOutput, ProbeStream, audio_extension, create_cover,
        encode_cover, filename_title, metadata_from_probe, probe_audio, transcode_to_aac,
    };
    use image::{ImageBuffer, ImageFormat, Rgb};
    use std::{collections::HashMap, process::Stdio};
    use tokio::process::Command;
    use uuid::Uuid;

    #[test]
    fn validates_supported_audio_extensions_case_insensitively() {
        assert_eq!(audio_extension("track.M4A"), Some("m4a"));
        assert_eq!(audio_extension("track.opus"), Some("opus"));
        assert_eq!(audio_extension("track.exe"), None);
    }

    #[test]
    fn derives_a_readable_title_from_filename() {
        assert_eq!(filename_title("Morning Light.flac"), "Morning Light");
        assert_eq!(filename_title(".mp3"), "Untitled");
    }

    #[test]
    fn reads_audio_metadata_from_ffprobe_output() {
        let probe = ProbeOutput {
            streams: vec![ProbeStream {
                codec_type: Some("audio".to_owned()),
                sample_rate: Some("48000".to_owned()),
                bit_rate: Some("255921".to_owned()),
                duration: Some("123.456".to_owned()),
                tags: HashMap::new(),
            }],
            format: Some(ProbeFormat {
                duration: Some("123.456".to_owned()),
                bit_rate: Some("260000".to_owned()),
                tags: HashMap::from([
                    ("TITLE".to_owned(), "Night Drive".to_owned()),
                    ("artist".to_owned(), "Nova".to_owned()),
                    ("album".to_owned(), "Signals".to_owned()),
                ]),
            }),
        };
        let metadata = metadata_from_probe(&probe, "fallback");
        assert_eq!(metadata.title, "Night Drive");
        assert_eq!(metadata.artist, "Nova");
        assert_eq!(metadata.album, "Signals");
        assert_eq!(metadata.duration_ms, 123_456);
        assert_eq!(metadata.bitrate, 255_921);
        assert_eq!(metadata.sample_rate, 48_000);
    }

    #[test]
    fn creates_a_webp_fallback_cover() {
        let encoded = encode_cover(None, [42; 16]).unwrap();
        assert_eq!(image::guess_format(&encoded).unwrap(), ImageFormat::WebP);
    }

    #[tokio::test]
    async fn transcodes_audio_and_generates_runtime_assets() {
        let directory = std::env::temp_dir().join(format!("rust-app-music-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let source = directory.join("source.wav");
        let audio = directory.join("audio.m4a");
        let cover = directory.join("cover.webp");
        let ffmpeg = std::env::var("FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".to_owned());
        let generated = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-metadata",
                "title=Runtime Test",
                "-metadata",
                "artist=Codex",
            ])
            .arg(&source)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .unwrap();
        assert!(generated.success());

        transcode_to_aac(&source, &audio).await.unwrap();
        let outcome = create_cover(&source, &cover, Uuid::new_v4()).await.unwrap();
        assert_eq!(outcome, CoverOutcome::Fallback);
        let probe = probe_audio(&audio).await.unwrap();
        let metadata = metadata_from_probe(&probe, "fallback");
        assert!(metadata.duration_ms >= 900);
        assert!(metadata.bitrate > 0);
        assert!(metadata.sample_rate > 0);
        assert_eq!(
            image::guess_format(&tokio::fs::read(&cover).await.unwrap()).unwrap(),
            ImageFormat::WebP
        );

        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn extracts_an_embedded_cover_instead_of_using_fallback() {
        let directory = std::env::temp_dir().join(format!("rust-app-cover-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let artwork = directory.join("artwork.png");
        let source = directory.join("source.mp3");
        let cover = directory.join("cover.webp");
        ImageBuffer::from_pixel(64, 64, Rgb([220_u8, 24, 80]))
            .save(&artwork)
            .unwrap();

        let ffmpeg = std::env::var("FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".to_owned());
        let generated = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-i",
            ])
            .arg(&artwork)
            .args([
                "-map",
                "0:a:0",
                "-map",
                "1:v:0",
                "-c:a",
                "libmp3lame",
                "-c:v",
                "png",
                "-disposition:v:0",
                "attached_pic",
                "-id3v2_version",
                "3",
            ])
            .arg(&source)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .unwrap();
        assert!(generated.success());

        let outcome = create_cover(&source, &cover, Uuid::new_v4()).await.unwrap();
        assert_eq!(outcome, CoverOutcome::Embedded);
        assert_eq!(
            image::guess_format(&tokio::fs::read(&cover).await.unwrap()).unwrap(),
            ImageFormat::WebP
        );

        tokio::fs::remove_dir_all(directory).await.unwrap();
    }
}
