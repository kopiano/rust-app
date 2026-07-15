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
use tokio::io::AsyncWriteExt;
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
                    cleanup_saved_file(saved_path.as_deref()).await;
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
                    cleanup_saved_file(saved_path.as_deref()).await;
                    return Err(StatusCode::BAD_REQUEST);
                }
                let result =
                    save_moment_media(&mut field, requested_media_type.as_deref(), &username).await;
                match result {
                    Ok((item, path)) => {
                        media = Some(item);
                        saved_path = Some(path);
                    }
                    Err(status) => {
                        cleanup_saved_file(saved_path.as_deref()).await;
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

    let result = sqlx::query_as::<_, Moment>(
        r#"
        WITH inserted AS (
            INSERT INTO moment (user_id, content, media)
            VALUES ($1, $2, $3)
            RETURNING id, user_id, content, media, created_at, updated_at
        )
        SELECT inserted.id, inserted.user_id, $4::text AS username,
               $5::text AS avatar, inserted.content, inserted.media,
               inserted.created_at, inserted.updated_at
        FROM inserted
        "#,
    )
    .bind(user_id)
    .bind(content)
    .bind(sqlx::types::Json(media.into_iter().collect::<Vec<_>>()))
    .bind(username)
    .bind(avatar)
    .fetch_one(&state.db)
    .await;
    let moment = match result {
        Ok(moment) => moment,
        Err(error) => {
            cleanup_saved_file(saved_path.as_deref()).await;
            tracing::error!(%error, %user_id, "Failed to create moment");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    Ok((StatusCode::CREATED, Json(ApiResponse::success(moment))))
}

pub async fn list(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> Result<Json<ApiResponse<Vec<Moment>>>, StatusCode> {
    let moments = sqlx::query_as::<_, Moment>(
        r#"
        SELECT moment.id, moment.user_id, "user".name AS username,
               "user".avatar, moment.content, moment.media,
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

async fn save_moment_media(
    field: &mut Field<'_>,
    requested_media_type: Option<&str>,
    username: &str,
) -> Result<(MomentMedia, PathBuf), StatusCode> {
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
    let filename = match media_type {
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
            filename
        }
        "video" => {
            let extension = video_extension(content_type.as_deref(), original_name.as_deref())
                .ok_or(StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
            let filename = format!("moment-{safe_username}-{timestamp}-{unique}.{extension}");
            let final_path = directory.join(&filename);
            let temporary_path = directory.join(format!(".{filename}.uploading"));
            stream_video_to_file(field, &temporary_path).await?;
            if let Err(error) = tokio::fs::rename(&temporary_path, &final_path).await {
                cleanup_saved_file(Some(&temporary_path)).await;
                tracing::error!(%error, "Failed to finalize moment video");
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
            filename
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let path = directory.join(&filename);
    Ok((
        MomentMedia {
            media_type: media_type.to_owned(),
            url: format!("/api/assets/moment/{filename}"),
        },
        path,
    ))
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
    use super::{encode_moment_image, media_name_component, video_extension};
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
}
