use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

use axum::{
    Extension, Json,
    extract::{Multipart, State, multipart::Field},
    http::StatusCode,
};
use image::ImageFormat;
use serde::Deserialize;
use tokio::{io::AsyncWriteExt, process::Command, sync::Semaphore};
use uuid::Uuid;

use crate::{
    app::AppState,
    common::response::ApiResponse,
    middleware::jwt::Claims,
    models::moment::{Moment, MomentMedia},
};

const MAX_CONTENT_CHARS: usize = 5_000;
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const MAX_VIDEO_BYTES: usize = 300 * 1024 * 1024;
const HLS_SEGMENT_SECONDS: &str = "6";
static VIDEO_TRANSCODE_SLOTS: Semaphore = Semaphore::const_new(2);

struct PendingVideo {
    source_path: PathBuf,
    output_directory: PathBuf,
}

struct SavedMomentMedia {
    media: MomentMedia,
    saved_path: PathBuf,
    pending_video: Option<PendingVideo>,
}

#[derive(Deserialize)]
struct VideoProbeOutput {
    streams: Vec<VideoProbeStream>,
}

#[derive(Deserialize)]
struct VideoProbeStream {
    width: u32,
    height: u32,
    #[serde(default)]
    tags: VideoProbeTags,
    #[serde(default)]
    side_data_list: Vec<VideoProbeSideData>,
}

#[derive(Default, Deserialize)]
struct VideoProbeTags {
    rotate: Option<String>,
}

#[derive(Deserialize)]
struct VideoProbeSideData {
    rotation: Option<i32>,
}

pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<ApiResponse<Moment>>), StatusCode> {
    let user_id = claims
        .sub
        .parse::<Uuid>()
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let (username, avatar) = sqlx::query_as::<_, (String, Option<String>)>(
        r#"SELECT name, avatar FROM "user" WHERE id = $1"#,
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, %user_id, "Failed to load moment author");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    let mut content = None;
    let mut requested_media_type = None;
    let mut media = None;
    let mut saved_path = None;
    let mut pending_video = None;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
    {
        let field_name = field.name().map(str::to_owned);
        match field_name.as_deref() {
            Some("content") => {
                let value = field.text().await.map_err(|_| StatusCode::BAD_REQUEST)?;
                let value = value.trim().to_owned();
                if value.chars().count() > MAX_CONTENT_CHARS {
                    cleanup_saved_path(saved_path.as_deref()).await;
                    return Err(StatusCode::PAYLOAD_TOO_LARGE);
                }
                content = Some(value);
            }
            Some("media_type") => {
                requested_media_type =
                    Some(field.text().await.map_err(|_| StatusCode::BAD_REQUEST)?);
            }
            Some("media") => {
                if media.is_some() {
                    cleanup_saved_path(saved_path.as_deref()).await;
                    return Err(StatusCode::BAD_REQUEST);
                }
                let result =
                    save_moment_media(&mut field, requested_media_type.as_deref(), &username).await;
                match result {
                    Ok(saved) => {
                        media = Some(saved.media);
                        saved_path = Some(saved.saved_path);
                        pending_video = saved.pending_video;
                    }
                    Err(status) => {
                        cleanup_saved_path(saved_path.as_deref()).await;
                        return Err(status);
                    }
                }
            }
            _ => {}
        }
    }

    let content = content.filter(|value| !value.is_empty());
    if content.is_none() && media.is_none() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if media.is_none() && requested_media_type.is_some() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let processing_status = if pending_video.is_some() {
        "processing"
    } else {
        "ready"
    };
    let result = sqlx::query_as::<_, Moment>(
        r#"
        WITH inserted AS (
            INSERT INTO moment (user_id, content, media, processing_status)
            VALUES ($1, $2, $3, $4)
            RETURNING id, user_id, content, media, processing_status,
                      processing_error, created_at, updated_at
        )
        SELECT inserted.id, inserted.user_id, $5::text AS username,
               $6::text AS avatar, inserted.content, inserted.media,
               inserted.processing_status, inserted.processing_error,
               inserted.created_at, inserted.updated_at
        FROM inserted
        "#,
    )
    .bind(user_id)
    .bind(content)
    .bind(sqlx::types::Json(media.into_iter().collect::<Vec<_>>()))
    .bind(processing_status)
    .bind(username)
    .bind(avatar)
    .fetch_one(&state.db)
    .await;
    let moment = match result {
        Ok(moment) => moment,
        Err(error) => {
            cleanup_saved_path(saved_path.as_deref()).await;
            tracing::error!(%error, %user_id, "Failed to create moment");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    let response_status = if let Some(pending) = pending_video {
        spawn_video_processing(state.db.clone(), moment.id, pending);
        StatusCode::ACCEPTED
    } else {
        StatusCode::CREATED
    };

    Ok((response_status, Json(ApiResponse::success(moment))))
}

pub async fn list(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> Result<Json<ApiResponse<Vec<Moment>>>, StatusCode> {
    let moments = sqlx::query_as::<_, Moment>(
        r#"
        SELECT moment.id, moment.user_id, "user".name AS username,
               "user".avatar, moment.content, moment.media,
               moment.processing_status, moment.processing_error,
               moment.created_at, moment.updated_at
        FROM moment
        INNER JOIN "user" ON "user".id = moment.user_id
        ORDER BY moment.created_at DESC, moment.id DESC
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, "Failed to load moments");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(ApiResponse::success(moments)))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    axum::extract::Path(moment_id): axum::extract::Path<Uuid>,
) -> Result<Json<ApiResponse<Moment>>, StatusCode> {
    let moment = sqlx::query_as::<_, Moment>(
        r#"
        SELECT moment.id, moment.user_id, "user".name AS username,
               "user".avatar, moment.content, moment.media,
               moment.processing_status, moment.processing_error,
               moment.created_at, moment.updated_at
        FROM moment
        INNER JOIN "user" ON "user".id = moment.user_id
        WHERE moment.id = $1
        "#,
    )
    .bind(moment_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, %moment_id, "Failed to load moment");
        StatusCode::INTERNAL_SERVER_ERROR
    })?
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(ApiResponse::success(moment)))
}

async fn save_moment_media(
    field: &mut Field<'_>,
    requested_media_type: Option<&str>,
    username: &str,
) -> Result<SavedMomentMedia, StatusCode> {
    let content_type = field.content_type().map(str::to_owned);
    let original_name = field.file_name().map(str::to_owned);
    let media_type = requested_media_type
        .or_else(|| infer_media_type(content_type.as_deref()))
        .ok_or(StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
    let directory = Path::new("src/assets/moment");
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let timestamp = chrono::Utc::now().timestamp_millis();
    let unique = Uuid::new_v4().simple().to_string();
    let safe_username = media_name_component(username);
    match media_type {
        "image" => {
            if !content_type
                .as_deref()
                .is_some_and(|value| value.starts_with("image/"))
            {
                return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
            }
            let source = read_field_limited(field, MAX_IMAGE_BYTES).await?;
            let encoded = tokio::task::spawn_blocking(move || encode_moment_image(&source))
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                .map_err(|_| StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
            let filename = format!("moment-{safe_username}-{timestamp}-{unique}.webp");
            tokio::fs::write(directory.join(&filename), encoded)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            let path = directory.join(&filename);
            Ok(SavedMomentMedia {
                media: MomentMedia {
                    media_type: media_type.to_owned(),
                    url: format!("/api/assets/moment/{filename}"),
                    poster_url: None,
                    width: None,
                    height: None,
                },
                saved_path: path,
                pending_video: None,
            })
        }
        "video" => {
            let extension = video_extension(content_type.as_deref(), original_name.as_deref())
                .ok_or(StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
            let video_directory_name = format!("video-{safe_username}-{timestamp}-{unique}");
            let video_directory = directory.join(&video_directory_name);
            tokio::fs::create_dir(&video_directory)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

            let source_path = video_directory.join(format!("source.{extension}"));
            let temporary_path = video_directory.join(format!(".source.{extension}.uploading"));
            if let Err(status) = stream_video_to_file(field, &temporary_path).await {
                cleanup_saved_path(Some(&video_directory)).await;
                return Err(status);
            }
            if let Err(error) = tokio::fs::rename(&temporary_path, &source_path).await {
                cleanup_saved_file(Some(&temporary_path)).await;
                tracing::error!(%error, "Failed to finalize moment video");
                cleanup_saved_path(Some(&video_directory)).await;
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
            let (width, height) = match probe_video_dimensions(&source_path).await {
                Ok(dimensions) => dimensions,
                Err(status) => {
                    cleanup_saved_path(Some(&video_directory)).await;
                    return Err(status);
                }
            };

            Ok(SavedMomentMedia {
                media: MomentMedia {
                    media_type: media_type.to_owned(),
                    url: format!("/api/assets/moment/{video_directory_name}/index.m3u8"),
                    poster_url: Some(format!(
                        "/api/assets/moment/{video_directory_name}/poster.webp"
                    )),
                    width: Some(width),
                    height: Some(height),
                },
                saved_path: video_directory.clone(),
                pending_video: Some(PendingVideo {
                    source_path,
                    output_directory: video_directory,
                }),
            })
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    }
}

async fn probe_video_dimensions(source_path: &Path) -> Result<(u32, u32), StatusCode> {
    let ffprobe = std::env::var("FFPROBE_PATH").unwrap_or_else(|_| "ffprobe".to_owned());
    let output = Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height:stream_tags=rotate:stream_side_data=rotation",
            "-of",
            "json",
        ])
        .arg(source_path)
        .output()
        .await
        .map_err(|error| {
            tracing::error!(%error, executable = %ffprobe, "Failed to start FFprobe");
            StatusCode::SERVICE_UNAVAILABLE
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!(
            status = ?output.status.code(),
            error = %stderr.trim(),
            "FFprobe video inspection failed"
        );
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }

    parse_video_dimensions(&output.stdout).ok_or_else(|| {
        tracing::error!("FFprobe did not return valid video dimensions");
        StatusCode::UNPROCESSABLE_ENTITY
    })
}

fn parse_video_dimensions(output: &[u8]) -> Option<(u32, u32)> {
    let probe = serde_json::from_slice::<VideoProbeOutput>(output).ok()?;
    let stream = probe.streams.first()?;
    if stream.width == 0 || stream.height == 0 {
        return None;
    }

    let rotation = stream
        .tags
        .rotate
        .as_deref()
        .and_then(|value| value.parse::<i32>().ok())
        .or_else(|| stream.side_data_list.iter().find_map(|item| item.rotation))
        .unwrap_or(0)
        .rem_euclid(360);

    if matches!(rotation, 90 | 270) {
        Some((stream.height, stream.width))
    } else {
        Some((stream.width, stream.height))
    }
}

fn spawn_video_processing(db: sqlx::PgPool, moment_id: Uuid, pending: PendingVideo) {
    tokio::spawn(async move {
        let result = transcode_video_to_hls(&pending.source_path, &pending.output_directory).await;
        let (status, processing_error) = match result {
            Ok(()) => ("ready", None),
            Err(error_status) => {
                tracing::error!(
                    %moment_id,
                    status = %error_status,
                    "Moment video processing failed"
                );
                ("failed", Some(error_status.to_string()))
            }
        };

        if let Err(error) = sqlx::query(
            r#"
            UPDATE moment
            SET processing_status = $2, processing_error = $3, updated_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(moment_id)
        .bind(status)
        .bind(processing_error)
        .execute(&db)
        .await
        {
            tracing::error!(%error, %moment_id, "Failed to update moment processing status");
        }
    });
}

async fn transcode_video_to_hls(
    source_path: &Path,
    output_directory: &Path,
) -> Result<(), StatusCode> {
    let _permit = VIDEO_TRANSCODE_SLOTS
        .acquire()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let ffmpeg = std::env::var("FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".to_owned());
    let playlist_path = output_directory.join("index.m3u8");
    let segment_pattern = output_directory.join("segment-%05d.ts");

    let output = Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
        .arg(source_path)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a:0?",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "24",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-force_key_frames",
            "expr:gte(t,n_forced*6)",
            "-f",
            "hls",
            "-hls_time",
            HLS_SEGMENT_SECONDS,
            "-hls_playlist_type",
            "vod",
            "-hls_segment_type",
            "mpegts",
            "-hls_flags",
            "independent_segments",
            "-hls_segment_filename",
        ])
        .arg(segment_pattern)
        .arg(playlist_path)
        .output()
        .await
        .map_err(|error| {
            tracing::error!(%error, executable = %ffmpeg, "Failed to start FFmpeg");
            StatusCode::SERVICE_UNAVAILABLE
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!(
            status = ?output.status.code(),
            error = %stderr.trim(),
            "FFmpeg HLS transcoding failed"
        );
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }

    generate_video_poster(&ffmpeg, source_path, output_directory).await?;

    Ok(())
}

async fn generate_video_poster(
    ffmpeg: &str,
    source_path: &Path,
    output_directory: &Path,
) -> Result<(), StatusCode> {
    let output = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(source_path)
        .args([
            "-map",
            "0:v:0",
            "-vf",
            "thumbnail=150",
            "-frames:v",
            "1",
            "-c:v",
            "mjpeg",
            "-q:v",
            "3",
            "-f",
            "image2pipe",
            "pipe:1",
        ])
        .output()
        .await
        .map_err(|error| {
            tracing::error!(%error, executable = %ffmpeg, "Failed to start FFmpeg poster generation");
            StatusCode::SERVICE_UNAVAILABLE
        })?;

    if !output.status.success() || output.stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!(
            status = ?output.status.code(),
            error = %stderr.trim(),
            "FFmpeg poster generation failed"
        );
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }

    let encoded = tokio::task::spawn_blocking(move || encode_video_poster(&output.stdout))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|error| {
            tracing::error!(%error, "Failed to encode video poster as WebP");
            StatusCode::UNPROCESSABLE_ENTITY
        })?;
    tokio::fs::write(output_directory.join("poster.webp"), encoded)
        .await
        .map_err(|error| {
            tracing::error!(%error, "Failed to save video poster");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

async fn read_field_limited(
    field: &mut Field<'_>,
    max_bytes: usize,
) -> Result<Vec<u8>, StatusCode> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(|_| StatusCode::BAD_REQUEST)? {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn stream_video_to_file(field: &mut Field<'_>, path: &Path) -> Result<(), StatusCode> {
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut written = 0usize;
    loop {
        let chunk = match field.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => {
                drop(file);
                cleanup_saved_file(Some(path)).await;
                return Err(StatusCode::BAD_REQUEST);
            }
        };
        written = written.saturating_add(chunk.len());
        if written > MAX_VIDEO_BYTES {
            drop(file);
            cleanup_saved_file(Some(path)).await;
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        if file.write_all(&chunk).await.is_err() {
            drop(file);
            cleanup_saved_file(Some(path)).await;
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
    if file.flush().await.is_err() {
        drop(file);
        cleanup_saved_file(Some(path)).await;
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    Ok(())
}

async fn cleanup_saved_file(path: Option<&Path>) {
    let Some(path) = path else {
        return;
    };
    if let Err(error) = tokio::fs::remove_file(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, path = %path.display(), "Failed to clean up moment upload");
    }
}

async fn cleanup_saved_path(path: Option<&Path>) {
    let Some(path) = path else {
        return;
    };
    let result = match tokio::fs::metadata(path).await {
        Ok(metadata) if metadata.is_dir() => tokio::fs::remove_dir_all(path).await,
        Ok(_) => tokio::fs::remove_file(path).await,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        tracing::warn!(%error, path = %path.display(), "Failed to clean up moment upload");
    }
}

fn infer_media_type(content_type: Option<&str>) -> Option<&'static str> {
    match content_type {
        Some(value) if value.starts_with("image/") => Some("image"),
        Some(value) if value.starts_with("video/") => Some("video"),
        _ => None,
    }
}

fn media_name_component(username: &str) -> String {
    let value = username
        .chars()
        .filter_map(|character| {
            if character.is_ascii_alphanumeric() {
                Some(character.to_ascii_lowercase())
            } else if matches!(character, '-' | '_') {
                Some(character)
            } else {
                None
            }
        })
        .take(48)
        .collect::<String>();
    if value.is_empty() {
        "user".to_owned()
    } else {
        value
    }
}

fn encode_moment_image(bytes: &[u8]) -> image::ImageResult<Vec<u8>> {
    let image = image::load_from_memory(bytes)?;
    let resized = image.thumbnail(1920, 1920);
    let mut encoded = Cursor::new(Vec::new());
    resized.write_to(&mut encoded, ImageFormat::WebP)?;
    Ok(encoded.into_inner())
}

fn encode_video_poster(bytes: &[u8]) -> image::ImageResult<Vec<u8>> {
    let image = image::load_from_memory(bytes)?;
    let resized = image.thumbnail(1280, 1280);
    let mut encoded = Cursor::new(Vec::new());
    resized.write_to(&mut encoded, ImageFormat::WebP)?;
    Ok(encoded.into_inner())
}

fn video_extension(
    content_type: Option<&str>,
    original_name: Option<&str>,
) -> Option<&'static str> {
    match content_type {
        Some("video/mp4") => Some("mp4"),
        Some("video/webm") => Some("webm"),
        Some("video/quicktime") => Some("mov"),
        Some("video/x-m4v") => Some("m4v"),
        None | Some("application/octet-stream") => {
            match original_name
                .and_then(|name| name.rsplit_once('.'))
                .map(|(_, extension)| extension.to_ascii_lowercase())
                .as_deref()
            {
                Some("mp4") => Some("mp4"),
                Some("webm") => Some("webm"),
                Some("mov") => Some("mov"),
                Some("m4v") => Some("m4v"),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        encode_moment_image, encode_video_poster, media_name_component, parse_video_dimensions,
        video_extension,
    };
    use image::{DynamicImage, GenericImageView, ImageFormat, RgbImage};
    use std::io::Cursor;

    #[test]
    fn encodes_moment_image_as_resized_webp() {
        let source = DynamicImage::ImageRgb8(RgbImage::new(2400, 1200));
        let mut input = Cursor::new(Vec::new());
        source.write_to(&mut input, ImageFormat::Png).unwrap();

        let encoded = encode_moment_image(&input.into_inner()).unwrap();
        assert_eq!(image::guess_format(&encoded).unwrap(), ImageFormat::WebP);
        assert_eq!(
            image::load_from_memory(&encoded).unwrap().dimensions(),
            (1920, 960)
        );
    }

    #[test]
    fn encodes_video_poster_as_resized_webp() {
        let source = DynamicImage::ImageRgb8(RgbImage::new(1920, 1080));
        let mut input = Cursor::new(Vec::new());
        source.write_to(&mut input, ImageFormat::Jpeg).unwrap();

        let encoded = encode_video_poster(&input.into_inner()).unwrap();
        assert_eq!(image::guess_format(&encoded).unwrap(), ImageFormat::WebP);
        assert_eq!(
            image::load_from_memory(&encoded).unwrap().dimensions(),
            (1280, 720)
        );
    }

    #[test]
    fn validates_video_content_type_and_extension() {
        assert_eq!(
            video_extension(Some("video/mp4"), Some("clip.exe")),
            Some("mp4")
        );
        assert_eq!(
            video_extension(Some("application/octet-stream"), Some("clip.webm")),
            Some("webm")
        );
        assert_eq!(video_extension(Some("image/webp"), Some("clip.mp4")), None);
        assert_eq!(video_extension(Some("video/avi"), Some("clip.avi")), None);
    }

    #[test]
    fn sanitizes_username_for_moment_filename() {
        assert_eq!(media_name_component("Admin User!"), "adminuser");
        assert_eq!(media_name_component("动态用户"), "user");
    }

    #[test]
    fn reads_display_dimensions_from_ffprobe_output() {
        let landscape = br#"{"streams":[{"width":1920,"height":1080}]}"#;
        let portrait =
            br#"{"streams":[{"width":1920,"height":1080,"side_data_list":[{"rotation":-90}]}]}"#;

        assert_eq!(parse_video_dimensions(landscape), Some((1920, 1080)));
        assert_eq!(parse_video_dimensions(portrait), Some((1080, 1920)));
    }
}
