use std::{
    io::{Cursor, SeekFrom},
    path::{Path as FsPath, PathBuf},
    process::Stdio,
    sync::Arc,
};

use axum::{
    Extension, Json,
    body::Bytes,
    extract::{Multipart, Path as AxumPath, Query, State, multipart::Field},
    http::{
        HeaderMap, StatusCode,
        header::{HOST, ORIGIN, REFERER},
    },
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use image::ImageFormat;
use serde::Deserialize;
use sqlx::{PgPool, Postgres, Transaction};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader, BufWriter},
    process::Command,
    sync::Semaphore,
};
use uuid::Uuid;

use crate::{
    app::AppState,
    common::assets::asset_directory_name,
    common::response::ApiResponse,
    middleware::{jwt::Claims, plan::has_library_access},
    models::video::{
        CreateVideoCollection, CreateVideoComment, UpdateVideoCollection, Video, VideoCategory,
        VideoCollection, VideoComment, VideoCommentLikeState, VideoListPage, VideoReactionState,
        VideoUploadSession, VideoViewState,
    },
};

const VIDEO_ASSET_ROOT: &str = "src/assets/video";
const VIDEO_ASSET_URL: &str = "/api/assets/video";
const MAX_FREE_VIDEO_BYTES: usize = 2 * 1024 * 1024 * 1024;
const MAX_VIDEO_BYTES: usize = 6 * 1024 * 1024 * 1024;
const VIDEO_UPLOAD_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const MAX_COVER_BYTES: usize = 10 * 1024 * 1024;
const MAX_TITLE_CHARS: usize = 255;
const MAX_DESCRIPTION_CHARS: usize = 10_000;
const MAX_COMMENT_CHARS: usize = 1_000;
const DEFAULT_PAGE_SIZE: i64 = 20;
const MAX_PAGE_SIZE: i64 = 50;
const HLS_SEGMENT_SECONDS: &str = "6";
const VIDEO_COVER_WIDTH: u32 = 1920;
const VIDEO_COVER_HEIGHT: u32 = 1080;
const VIDEO_POSTER_CANDIDATE_COUNT: u32 = 12;
const VIDEO_VIEW_UTC_OFFSET_HOURS: i64 = 8;
const VIDEO_VISITOR_COOKIE: &str = "video_visitor_id";

#[derive(Default, Deserialize)]
pub struct VideoListQuery {
    before_created_at: Option<DateTime<Utc>>,
    before_id: Option<Uuid>,
    limit: Option<i64>,
    query: Option<String>,
    category: Option<String>,
    scope: Option<String>,
    collection_id: Option<Uuid>,
}

#[derive(Default, Deserialize)]
pub struct VideoCollectionQuery {
    mine: Option<bool>,
}

#[derive(Default, Deserialize)]
pub struct VideoCategoryQuery {
    scope: Option<String>,
}

#[derive(Deserialize)]
struct VideoProbeOutput {
    streams: Vec<VideoProbeStream>,
    format: Option<VideoProbeFormat>,
}

#[derive(Deserialize)]
struct VideoProbeStream {
    width: u32,
    height: u32,
    avg_frame_rate: Option<String>,
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

#[derive(Deserialize)]
struct VideoProbeFormat {
    duration: Option<String>,
}

struct ProbedVideo {
    width: u32,
    height: u32,
    duration_us: u64,
    fps: Option<f64>,
}

struct PendingVideo {
    source_path: PathBuf,
    output_directory: PathBuf,
    duration_us: u64,
}

#[derive(Deserialize)]
pub struct CreateVideoUpload {
    file_name: String,
    content_type: Option<String>,
    total_bytes: u64,
}

#[derive(sqlx::FromRow)]
struct UploadStateRow {
    video_id: Uuid,
    asset_directory: Option<String>,
    file_extension: String,
    total_bytes: i64,
    uploaded_bytes: i64,
    status: String,
}

pub async fn list(
    State(state): State<AppState>,
    claims: Option<Extension<Claims>>,
    Query(query): Query<VideoListQuery>,
) -> Result<Json<ApiResponse<VideoListPage>>, StatusCode> {
    let user_id = optional_user_id(claims)?;
    if query.before_created_at.is_some() != query.before_id.is_some() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let scope = query.scope.as_deref().unwrap_or("public");
    if !matches!(
        scope,
        "public" | "accessible" | "mine" | "favorites" | "collection"
    ) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if matches!(scope, "accessible" | "mine" | "favorites")
        || (scope == "collection" && query.collection_id.is_none())
    {
        if user_id.is_none() {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    let limit = query
        .limit
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);
    let search = query
        .query
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("%{value}%"));
    let category = query
        .category
        .as_deref()
        .map(normalize_slug)
        .filter(|value| !value.is_empty() && value != "all");

    let mut videos = sqlx::query_as::<_, Video>(
        r#"
        SELECT video.id, video.user_id, "user".name AS username, "user".avatar,
               video.title, video.description, video.cover_url, video.duration,
               video.width, video.height, video.fps::double precision AS fps,
               video.size, video.origin_file_url, video.hls_master_url,
               video.status, video.visibility, video.processing_progress,
               video.processing_error, video.view_count, video.like_count,
               video.comment_count, video.favorite_count,
               EXISTS (
                   SELECT 1 FROM video_like
                   WHERE video_like.video_id = video.id
                     AND video_like.user_id = $3
               ) AS liked,
               EXISTS (
                   SELECT 1 FROM video_favorite
                   WHERE video_favorite.video_id = video.id
                     AND video_favorite.user_id = $3
               ) AS favorited,
               COALESCE(video.user_id = $3, FALSE) AS owned,
               COALESCE((
                   SELECT jsonb_agg(
                       jsonb_build_object(
                           'id', video_category.id,
                           'slug', video_category.slug,
                           'name_zh', video_category.name_zh,
                           'name_en', video_category.name_en
                       )
                       ORDER BY video_category.name_en ASC
                   )
                   FROM video_category_map
                   INNER JOIN video_category
                       ON video_category.id = video_category_map.category_id
                   WHERE video_category_map.video_id = video.id
               ), '[]'::jsonb) AS categories,
               video.created_at, video.updated_at
        FROM video
        INNER JOIN "user" ON "user".id = video.user_id
        WHERE (
            CASE $7::text
                WHEN 'mine' THEN video.user_id = $3
                WHEN 'favorites' THEN EXISTS (
                    SELECT 1 FROM video_favorite
                    WHERE video_favorite.video_id = video.id
                      AND video_favorite.user_id = $3
                ) AND (
                    video.user_id = $3
                    OR (
                        video.visibility = 'public'
                        AND video.status = 'ready'
                        AND video.published_at IS NOT NULL
                    )
                )
                WHEN 'collection' THEN
                    EXISTS (
                        SELECT 1
                        FROM video_collection
                        WHERE video_collection.id = $8
                          AND (
                              video.user_id = video_collection.user_id
                              OR (
                                  video_collection.include_favorites
                                  AND EXISTS (
                                      SELECT 1 FROM video_favorite
                                      WHERE video_favorite.video_id = video.id
                                        AND video_favorite.user_id = video_collection.user_id
                                  )
                                  AND video.visibility = 'public'
                                  AND video.status = 'ready'
                                  AND video.published_at IS NOT NULL
                              )
                          )
                          AND (
                              video_collection.category_slug IS NULL
                              OR EXISTS (
                                  SELECT 1
                                  FROM regexp_split_to_table(
                                      regexp_replace(
                                          COALESCE(video.description, ''),
                                          '#',
                                          '',
                                          'g'
                                      ),
                                      '\s+'
                                  ) AS video_category
                                  WHERE lower(video_category)
                                      = lower(video_collection.category_slug)
                              )
                          )
                          AND (
                              video_collection.visibility = 'public'
                              OR video_collection.user_id = $3
                          )
                          AND (
                              video.user_id = $3
                              OR (
                                  video.visibility = 'public'
                                  AND video.status = 'ready'
                                  AND video.published_at IS NOT NULL
                              )
                          )
                    )
                WHEN 'accessible' THEN
                    (
                        video.user_id = $3
                        AND (
                            video.status = 'ready'
                            OR video.published_at IS NOT NULL
                        )
                    )
                    OR (
                        video.visibility = 'public'
                        AND video.status = 'ready'
                        AND video.published_at IS NOT NULL
                    )
                ELSE
                    video.visibility = 'public'
                    AND video.status = 'ready'
                    AND video.published_at IS NOT NULL
            END
        )
          AND (
              $1::timestamptz IS NULL
              OR (video.created_at, video.id) < ($1, $2::uuid)
          )
          AND (
              $4::text IS NULL
              OR video.title ILIKE $4
              OR video.description ILIKE $4
              OR "user".name ILIKE $4
          )
          AND (
              $5::text IS NULL
              OR EXISTS (
                  SELECT 1
                  FROM video_category_map
                  INNER JOIN video_category
                      ON video_category.id = video_category_map.category_id
                  WHERE video_category_map.video_id = video.id
                    AND video_category.slug = $5
              )
          )
        ORDER BY video.created_at DESC, video.id DESC
        LIMIT $6
        "#,
    )
    .bind(query.before_created_at)
    .bind(query.before_id)
    .bind(user_id)
    .bind(search)
    .bind(category)
    .bind(limit + 1)
    .bind(scope)
    .bind(query.collection_id)
    .fetch_all(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, %scope, "Failed to list videos");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let has_more = videos.len() as i64 > limit;
    if has_more {
        videos.truncate(limit as usize);
    }
    let next = has_more
        .then(|| videos.last().map(|video| (video.created_at, video.id)))
        .flatten();

    Ok(Json(ApiResponse::success(VideoListPage {
        items: videos,
        has_more,
        next_before_created_at: next.map(|value| value.0),
        next_before_id: next.map(|value| value.1),
    })))
}

pub async fn get(
    State(state): State<AppState>,
    claims: Option<Extension<Claims>>,
    AxumPath(video_id): AxumPath<Uuid>,
) -> Result<Json<ApiResponse<Video>>, StatusCode> {
    let user_id = optional_user_id(claims)?;
    let video = load_video(&state.db, video_id, user_id).await?;
    Ok(Json(ApiResponse::success(video)))
}

pub async fn categories(
    State(state): State<AppState>,
    claims: Option<Extension<Claims>>,
    Query(query): Query<VideoCategoryQuery>,
) -> Result<Json<ApiResponse<Vec<VideoCategory>>>, StatusCode> {
    let user_id = optional_user_id(claims)?;
    let scope = query.scope.as_deref().unwrap_or("public");
    if !matches!(scope, "public" | "accessible" | "mine") {
        return Err(StatusCode::BAD_REQUEST);
    }
    if matches!(scope, "accessible" | "mine") && user_id.is_none() {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let categories = sqlx::query_as::<_, VideoCategory>(
        r#"
        SELECT video_category.id, video_category.slug,
               video_category.name_zh, video_category.name_en
        FROM video_category
        WHERE EXISTS (
            SELECT 1
            FROM video_category_map
            INNER JOIN video ON video.id = video_category_map.video_id
            WHERE video_category_map.category_id = video_category.id
              AND video.status = 'ready'
              AND CASE $2::text
                  WHEN 'mine' THEN video.user_id = $1
                  WHEN 'accessible' THEN
                      video.user_id = $1
                      OR (
                          video.visibility = 'public'
                          AND video.published_at IS NOT NULL
                      )
                  ELSE
                      video.visibility = 'public'
                      AND video.published_at IS NOT NULL
              END
        )
        ORDER BY video_category.name_en ASC, video_category.id ASC
        "#,
    )
    .bind(user_id)
    .bind(scope)
    .fetch_all(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, "Failed to list video categories");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(ApiResponse::success(categories)))
}

pub async fn upload(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<ApiResponse<Video>>), StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    let max_upload_bytes = max_video_upload_bytes(&state.db, user_id).await?;
    let video_id = Uuid::new_v4();
    let username = load_username(&state.db, user_id).await?;
    let asset_directory = asset_directory_name(&username, video_id);
    let output_directory = FsPath::new(VIDEO_ASSET_ROOT).join(&asset_directory);
    tokio::fs::create_dir_all(&output_directory)
        .await
        .map_err(|error| {
            tracing::error!(%error, path = %output_directory.display(), "Failed to create video directory");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut source_path = None;
    let mut upload_record_created = false;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
    {
        if field.name() != Some("video") {
            continue;
        }
        if source_path.is_some() {
            cleanup_video_directory(&output_directory).await;
            return Err(StatusCode::BAD_REQUEST);
        }
        let content_type = field.content_type().map(str::to_owned);
        let original_name = field.file_name().map(str::to_owned);
        let extension = video_extension(content_type.as_deref(), original_name.as_deref())
            .ok_or(StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
        let original_title = original_name
            .as_deref()
            .map(default_video_title)
            .filter(|value| !value.is_empty());
        let final_path = output_directory.join(format!("source.{extension}"));
        let temporary_path = output_directory.join(format!(".source.{extension}.uploading"));
        let directory_url = format!("{VIDEO_ASSET_URL}/{asset_directory}");
        let inserted = sqlx::query(
            r#"
            INSERT INTO video (
                id, user_id, title, cover_url, duration, origin_file_url,
                hls_master_url, status, visibility, processing_progress, asset_directory
            )
            VALUES ($1, $2, $3, $4, 0, $5, $6, 'uploading', 'private', 0, $7)
            "#,
        )
        .bind(video_id)
        .bind(user_id)
        .bind(original_title)
        .bind(format!("{directory_url}/poster.webp"))
        .bind(format!("{directory_url}/source.{extension}"))
        .bind(format!("{directory_url}/master.m3u8"))
        .bind(&asset_directory)
        .execute(&state.db)
        .await;
        if let Err(error) = inserted {
            tracing::error!(%error, %video_id, %user_id, "Failed to create uploading video record");
            cleanup_video_directory(&output_directory).await;
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        upload_record_created = true;
        if let Err(status) =
            stream_video_to_file(&mut field, &temporary_path, max_upload_bytes).await
        {
            cleanup_failed_video_upload(&state.db, video_id, &output_directory).await;
            return Err(status);
        }
        if let Err(error) = tokio::fs::rename(&temporary_path, &final_path).await {
            tracing::error!(%error, "Failed to finalize video upload");
            cleanup_failed_video_upload(&state.db, video_id, &output_directory).await;
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        source_path = Some(final_path);
    }

    let Some(source_path) = source_path else {
        if upload_record_created {
            cleanup_failed_video_upload(&state.db, video_id, &output_directory).await;
        } else {
            cleanup_video_directory(&output_directory).await;
        }
        return Err(StatusCode::BAD_REQUEST);
    };
    let video = load_video(&state.db, video_id, Some(user_id)).await?;
    spawn_uploaded_video_processing(
        state.db.clone(),
        state.limits.transcode.clone(),
        video_id,
        source_path,
        output_directory,
    );
    Ok((StatusCode::ACCEPTED, Json(ApiResponse::success(video))))
}

pub async fn create_upload(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(input): Json<CreateVideoUpload>,
) -> Result<(StatusCode, Json<ApiResponse<VideoUploadSession>>), StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    let max_upload_bytes = max_video_upload_bytes(&state.db, user_id).await?;
    if input.total_bytes == 0 || input.total_bytes > max_upload_bytes as u64 {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    let extension = video_extension(
        input.content_type.as_deref(),
        Some(input.file_name.as_str()),
    )
    .ok_or(StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
    let video_id = Uuid::new_v4();
    let upload_id = Uuid::new_v4();
    let username = load_username(&state.db, user_id).await?;
    let asset_directory = asset_directory_name(&username, video_id);
    let output_directory = FsPath::new(VIDEO_ASSET_ROOT).join(&asset_directory);
    tokio::fs::create_dir_all(&output_directory)
        .await
        .map_err(|error| {
            tracing::error!(%error, path = %output_directory.display(), "Failed to create video upload directory");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let temporary_path = upload_temporary_path(&output_directory, &extension);
    if let Err(error) = tokio::fs::File::create(&temporary_path).await {
        tracing::error!(%error, path = %temporary_path.display(), "Failed to initialize resumable video upload");
        cleanup_video_directory(&output_directory).await;
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    let directory_url = format!("{VIDEO_ASSET_URL}/{asset_directory}");
    let title = non_empty(default_video_title(&input.file_name));
    let mut transaction = state.db.begin().await.map_err(internal_db_error)?;
    let result = async {
        sqlx::query(
            r#"
            INSERT INTO video (
                id, user_id, title, cover_url, duration, origin_file_url,
                hls_master_url, status, visibility, processing_progress, asset_directory
            )
            VALUES ($1, $2, $3, $4, 0, $5, $6, 'uploading', 'private', 0, $7)
            "#,
        )
        .bind(video_id)
        .bind(user_id)
        .bind(title)
        .bind(format!("{directory_url}/poster.webp"))
        .bind(format!("{directory_url}/source.{extension}"))
        .bind(format!("{directory_url}/master.m3u8"))
        .bind(&asset_directory)
        .execute(&mut *transaction)
        .await
        .map_err(internal_db_error)?;
        sqlx::query(
            r#"
            INSERT INTO video_upload (
                id, video_id, user_id, file_extension, total_bytes, uploaded_bytes, status
            )
            VALUES ($1, $2, $3, $4, $5, 0, 'uploading')
            "#,
        )
        .bind(upload_id)
        .bind(video_id)
        .bind(user_id)
        .bind(&extension)
        .bind(input.total_bytes as i64)
        .execute(&mut *transaction)
        .await
        .map_err(internal_db_error)?;
        transaction.commit().await.map_err(internal_db_error)
    }
    .await;
    if let Err(status) = result {
        cleanup_video_directory(&output_directory).await;
        return Err(status);
    }

    let video = load_video(&state.db, video_id, Some(user_id)).await?;
    Ok((
        StatusCode::CREATED,
        Json(ApiResponse::success(VideoUploadSession {
            upload_id,
            video,
            chunk_size: VIDEO_UPLOAD_CHUNK_BYTES as u64,
            uploaded_bytes: 0,
            total_bytes: input.total_bytes,
            complete: false,
        })),
    ))
}

pub async fn upload_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(upload_id): AxumPath<Uuid>,
) -> Result<Json<ApiResponse<VideoUploadSession>>, StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    let state_row = load_upload_state(&state.db, upload_id, user_id).await?;
    let video = load_video(&state.db, state_row.video_id, Some(user_id)).await?;
    Ok(Json(ApiResponse::success(upload_session_response(
        upload_id, video, &state_row,
    ))))
}

pub async fn upload_chunk(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(upload_id): AxumPath<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<ApiResponse<VideoUploadSession>>, StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    let offset = upload_offset(&headers)?;
    if body.is_empty() || body.len() > VIDEO_UPLOAD_CHUNK_BYTES {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut transaction = state.db.begin().await.map_err(internal_db_error)?;
    let state_row = sqlx::query_as::<_, UploadStateRow>(
        r#"
        SELECT video_upload.video_id, video.asset_directory,
               video_upload.file_extension, video_upload.total_bytes,
               video_upload.uploaded_bytes, video_upload.status
        FROM video_upload
        INNER JOIN video ON video.id = video_upload.video_id
        WHERE video_upload.id = $1 AND video_upload.user_id = $2
        FOR UPDATE OF video_upload
        "#,
    )
    .bind(upload_id)
    .bind(user_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(internal_db_error)?
    .ok_or(StatusCode::NOT_FOUND)?;
    if state_row.status != "uploading" {
        return Err(StatusCode::CONFLICT);
    }
    let expected_offset = state_row.uploaded_bytes.max(0) as u64;
    let total_bytes = state_row.total_bytes.max(0) as u64;
    if offset != expected_offset || offset.saturating_add(body.len() as u64) > total_bytes {
        return Err(StatusCode::CONFLICT);
    }

    let output_directory =
        video_directory(state_row.video_id, state_row.asset_directory.as_deref());
    let temporary_path = upload_temporary_path(&output_directory, &state_row.file_extension);
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&temporary_path)
        .await
        .map_err(|error| {
            tracing::error!(%error, path = %temporary_path.display(), "Failed to open resumable video upload");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    file.seek(SeekFrom::Start(offset))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    file.write_all(&body)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    file.flush()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let uploaded_bytes = offset + body.len() as u64;
    sqlx::query(
        r#"
        UPDATE video_upload
        SET uploaded_bytes = $3, updated_at = NOW()
        WHERE id = $1 AND user_id = $2
        "#,
    )
    .bind(upload_id)
    .bind(user_id)
    .bind(uploaded_bytes as i64)
    .execute(&mut *transaction)
    .await
    .map_err(internal_db_error)?;
    transaction.commit().await.map_err(internal_db_error)?;

    let video = load_video(&state.db, state_row.video_id, Some(user_id)).await?;
    Ok(Json(ApiResponse::success(VideoUploadSession {
        upload_id,
        video,
        chunk_size: VIDEO_UPLOAD_CHUNK_BYTES as u64,
        uploaded_bytes,
        total_bytes,
        complete: false,
    })))
}

pub async fn complete_upload(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(upload_id): AxumPath<Uuid>,
) -> Result<(StatusCode, Json<ApiResponse<Video>>), StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    let mut transaction = state.db.begin().await.map_err(internal_db_error)?;
    let state_row = sqlx::query_as::<_, UploadStateRow>(
        r#"
        SELECT video_upload.video_id, video.asset_directory,
               video_upload.file_extension, video_upload.total_bytes,
               video_upload.uploaded_bytes, video_upload.status
        FROM video_upload
        INNER JOIN video ON video.id = video_upload.video_id
        WHERE video_upload.id = $1 AND video_upload.user_id = $2
        FOR UPDATE OF video_upload
        "#,
    )
    .bind(upload_id)
    .bind(user_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(internal_db_error)?
    .ok_or(StatusCode::NOT_FOUND)?;
    if state_row.status == "complete" {
        transaction.rollback().await.map_err(internal_db_error)?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(ApiResponse::success(
                load_video(&state.db, state_row.video_id, Some(user_id)).await?,
            )),
        ));
    }
    if state_row.status != "uploading" || state_row.uploaded_bytes != state_row.total_bytes {
        return Err(StatusCode::CONFLICT);
    }

    let output_directory =
        video_directory(state_row.video_id, state_row.asset_directory.as_deref());
    let temporary_path = upload_temporary_path(&output_directory, &state_row.file_extension);
    let final_path = output_directory.join(format!("source.{}", state_row.file_extension));
    tokio::fs::rename(&temporary_path, &final_path)
        .await
        .map_err(|error| {
            tracing::error!(%error, "Failed to finalize resumable video upload");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    sqlx::query(
        r#"
        UPDATE video_upload
        SET status = 'complete', completed_at = NOW(), updated_at = NOW()
        WHERE video_upload.id = $1 AND video_upload.user_id = $2
        "#,
    )
    .bind(upload_id)
    .bind(user_id)
    .execute(&mut *transaction)
    .await
    .map_err(internal_db_error)?;
    transaction.commit().await.map_err(internal_db_error)?;

    let video = load_video(&state.db, state_row.video_id, Some(user_id)).await?;
    spawn_uploaded_video_processing(
        state.db.clone(),
        state.limits.transcode.clone(),
        state_row.video_id,
        final_path,
        output_directory,
    );
    Ok((StatusCode::ACCEPTED, Json(ApiResponse::success(video))))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(video_id): AxumPath<Uuid>,
    mut multipart: Multipart,
) -> Result<Json<ApiResponse<Video>>, StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    ensure_video_owner(&state.db, video_id, user_id).await?;

    let mut title: Option<Option<String>> = None;
    let mut description: Option<Option<String>> = None;
    let mut visibility = None;
    let mut category_slugs: Option<Vec<String>> = None;
    let mut cover_bytes = None;
    let mut publish = false;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
    {
        match field.name() {
            Some("title") => {
                let value = field.text().await.map_err(|_| StatusCode::BAD_REQUEST)?;
                if value.chars().count() > MAX_TITLE_CHARS {
                    return Err(StatusCode::PAYLOAD_TOO_LARGE);
                }
                title = Some(non_empty(value));
            }
            Some("description") => {
                let value = field.text().await.map_err(|_| StatusCode::BAD_REQUEST)?;
                if value.chars().count() > MAX_DESCRIPTION_CHARS {
                    return Err(StatusCode::PAYLOAD_TOO_LARGE);
                }
                description = Some(non_empty(value));
            }
            Some("visibility") => {
                let value = field.text().await.map_err(|_| StatusCode::BAD_REQUEST)?;
                if !matches!(value.as_str(), "public" | "private") {
                    return Err(StatusCode::BAD_REQUEST);
                }
                visibility = Some(value);
            }
            Some("categories") => {
                let value = field.text().await.map_err(|_| StatusCode::BAD_REQUEST)?;
                let parsed = serde_json::from_str::<Vec<String>>(&value)
                    .unwrap_or_else(|_| value.split(',').map(str::to_owned).collect());
                category_slugs = Some(normalize_category_slugs(parsed));
            }
            Some("publish") => {
                publish = field
                    .text()
                    .await
                    .map_err(|_| StatusCode::BAD_REQUEST)?
                    .trim()
                    == "true";
            }
            Some("cover") => {
                let bytes = read_field_limited(&mut field, MAX_COVER_BYTES).await?;
                if bytes.is_empty() {
                    return Err(StatusCode::BAD_REQUEST);
                }
                // Validate by decoding the bytes below instead of trusting the
                // browser-provided MIME type, which is missing for some images.
                cover_bytes = Some(bytes);
            }
            _ => {}
        }
    }

    if publish {
        let existing = sqlx::query_as::<_, (Option<String>, i64)>(
            r#"
            SELECT
                title,
                (
                    SELECT COUNT(*)::bigint
                    FROM video_category_map
                    WHERE video_id = video.id
                ) AS category_count
            FROM video
            WHERE id = $1 AND user_id = $2
            "#,
        )
        .bind(video_id)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_db_error)?
        .ok_or(StatusCode::NOT_FOUND)?;
        let has_title = title.as_ref().and_then(|value| value.as_ref()).map_or_else(
            || {
                existing
                    .0
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
            },
            |_| true,
        );
        let has_categories = category_slugs
            .as_ref()
            .map(|slugs| !slugs.is_empty())
            .unwrap_or(existing.1 > 0);
        if !has_title || !has_categories {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let restart_processing = if publish {
        sqlx::query_scalar::<_, bool>(
            "SELECT status = 'failed' FROM video WHERE id = $1 AND user_id = $2",
        )
        .bind(video_id)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_db_error)?
        .unwrap_or(false)
    } else {
        false
    };
    let retry_pending = if restart_processing {
        let asset_directory = sqlx::query_scalar::<_, Option<String>>(
            "SELECT asset_directory FROM video WHERE id = $1 AND user_id = $2",
        )
        .bind(video_id)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_db_error)?
        .flatten();
        let output_directory = video_directory(video_id, asset_directory.as_deref());
        Some(PendingVideo {
            source_path: source_path_for_video(&output_directory).await?,
            duration_us: video_duration_us(&state.db, video_id).await?,
            output_directory,
        })
    } else {
        None
    };

    let cover_url = if let Some(bytes) = cover_bytes {
        let encoded = tokio::task::spawn_blocking(move || encode_cover(&bytes))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .map_err(|error| {
                tracing::warn!(%error, %video_id, "Unsupported video cover image");
                StatusCode::UNSUPPORTED_MEDIA_TYPE
            })?;
        let asset_directory = sqlx::query_scalar::<_, Option<String>>(
            "SELECT asset_directory FROM video WHERE id = $1 AND user_id = $2",
        )
        .bind(video_id)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .map_err(internal_db_error)?
        .flatten();
        let directory = video_directory(video_id, asset_directory.as_deref());
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let temporary_path = directory.join(".cover.webp.uploading");
        tokio::fs::write(&temporary_path, encoded)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        tokio::fs::rename(&temporary_path, directory.join("cover.webp"))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Some(format!(
            "{VIDEO_ASSET_URL}/{}/cover.webp",
            directory
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
        ))
    } else {
        None
    };

    let mut transaction = state.db.begin().await.map_err(internal_db_error)?;
    sqlx::query(
        r#"
        UPDATE video
        SET title = CASE WHEN $3 THEN $4 ELSE title END,
            description = CASE WHEN $5 THEN $6 ELSE description END,
            visibility = COALESCE($7, visibility),
            cover_url = COALESCE($8, cover_url),
            published_at = CASE
                WHEN $9 THEN COALESCE(published_at, NOW())
                ELSE published_at
            END,
            status = CASE
                WHEN $10 THEN 'processing'
                ELSE status
            END,
            processing_progress = CASE
                WHEN $10 THEN 0
                ELSE processing_progress
            END,
            processing_error = CASE
                WHEN $10 THEN NULL
                ELSE processing_error
            END,
            updated_at = NOW()
        WHERE id = $1 AND user_id = $2
        "#,
    )
    .bind(video_id)
    .bind(user_id)
    .bind(title.is_some())
    .bind(title.flatten())
    .bind(description.is_some())
    .bind(description.flatten())
    .bind(visibility)
    .bind(cover_url)
    .bind(publish)
    .bind(restart_processing)
    .execute(&mut *transaction)
    .await
    .map_err(internal_db_error)?;

    if let Some(slugs) = category_slugs {
        replace_video_categories(&mut transaction, video_id, &slugs).await?;
    }
    transaction.commit().await.map_err(internal_db_error)?;
    if let Some(pending) = retry_pending {
        cleanup_transcoded_video_files(&pending.output_directory).await;
        spawn_video_processing(
            state.db.clone(),
            state.limits.transcode.clone(),
            video_id,
            pending,
        );
    }

    Ok(Json(ApiResponse::success(
        load_video(&state.db, video_id, Some(user_id)).await?,
    )))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(video_id): AxumPath<Uuid>,
) -> Result<Json<ApiResponse<()>>, StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    let deleted = sqlx::query_as::<_, (Uuid, Option<String>)>(
        "DELETE FROM video WHERE id = $1 AND user_id = $2 RETURNING id, asset_directory",
    )
    .bind(video_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_db_error)?;
    let (deleted_id, asset_directory) = deleted.ok_or(StatusCode::NOT_FOUND)?;
    let directory = video_directory(deleted_id, asset_directory.as_deref());
    tokio::spawn(async move {
        cleanup_video_directory(&directory).await;
    });
    Ok(Json(ApiResponse::success(())))
}

pub async fn like(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(video_id): AxumPath<Uuid>,
) -> Result<Json<ApiResponse<VideoReactionState>>, StatusCode> {
    set_video_reaction(
        &state.db,
        video_id,
        authenticated_user_id(&claims)?,
        true,
        true,
    )
    .await
    .map(|state| Json(ApiResponse::success(state)))
}

pub async fn unlike(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(video_id): AxumPath<Uuid>,
) -> Result<Json<ApiResponse<VideoReactionState>>, StatusCode> {
    set_video_reaction(
        &state.db,
        video_id,
        authenticated_user_id(&claims)?,
        true,
        false,
    )
    .await
    .map(|state| Json(ApiResponse::success(state)))
}

pub async fn favorite(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(video_id): AxumPath<Uuid>,
) -> Result<Json<ApiResponse<VideoReactionState>>, StatusCode> {
    set_video_reaction(
        &state.db,
        video_id,
        authenticated_user_id(&claims)?,
        false,
        true,
    )
    .await
    .map(|state| Json(ApiResponse::success(state)))
}

pub async fn unfavorite(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(video_id): AxumPath<Uuid>,
) -> Result<Json<ApiResponse<VideoReactionState>>, StatusCode> {
    set_video_reaction(
        &state.db,
        video_id,
        authenticated_user_id(&claims)?,
        false,
        false,
    )
    .await
    .map(|state| Json(ApiResponse::success(state)))
}

pub async fn comments(
    State(state): State<AppState>,
    claims: Option<Extension<Claims>>,
    AxumPath(video_id): AxumPath<Uuid>,
) -> Result<Json<ApiResponse<Vec<VideoComment>>>, StatusCode> {
    let user_id = optional_user_id(claims)?;
    ensure_video_access(&state.db, video_id, user_id).await?;
    let comments = sqlx::query_as::<_, VideoComment>(
        r#"
        SELECT video_comment.id, video_comment.video_id, video_comment.user_id,
               comment_user.name AS username, comment_user.avatar,
               video_comment.parent_id, video_comment.reply_to_user_id,
               reply_user.name AS reply_to_username, video_comment.content,
               video_comment.like_count,
               EXISTS (
                   SELECT 1 FROM video_comment_like
                   WHERE video_comment_like.comment_id = video_comment.id
                     AND video_comment_like.user_id = $2
               ) AS liked,
               video_comment.created_at, video_comment.updated_at
        FROM video_comment
        INNER JOIN "user" AS comment_user
            ON comment_user.id = video_comment.user_id
        LEFT JOIN "user" AS reply_user
            ON reply_user.id = video_comment.reply_to_user_id
        WHERE video_comment.video_id = $1
          AND video_comment.deleted_at IS NULL
        ORDER BY video_comment.created_at ASC, video_comment.id ASC
        "#,
    )
    .bind(video_id)
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(internal_db_error)?;
    Ok(Json(ApiResponse::success(comments)))
}

pub async fn create_comment(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(video_id): AxumPath<Uuid>,
    Json(input): Json<CreateVideoComment>,
) -> Result<(StatusCode, Json<ApiResponse<VideoComment>>), StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    let content = input.content.trim();
    if content.is_empty() || content.chars().count() > MAX_COMMENT_CHARS {
        return Err(StatusCode::BAD_REQUEST);
    }

    let comment = sqlx::query_as::<_, VideoComment>(
        r#"
        WITH target AS (
            SELECT video.id
            FROM video
            WHERE video.id = $1
              AND (
                  video.user_id = $2
                  OR (
                      video.visibility = 'public'
                      AND video.status = 'ready'
                      AND video.published_at IS NOT NULL
                  )
              )
        ),
        parent AS (
            SELECT video_comment.id
            FROM video_comment
            INNER JOIN target ON target.id = video_comment.video_id
            WHERE video_comment.id = $4
              AND video_comment.deleted_at IS NULL
        ),
        inserted AS (
            INSERT INTO video_comment (
                video_id, user_id, parent_id, reply_to_user_id, content
            )
            SELECT target.id, $2, parent.id, $5, $3
            FROM target
            LEFT JOIN parent ON TRUE
            WHERE $4::uuid IS NULL OR parent.id IS NOT NULL
            RETURNING *
        ),
        updated AS (
            UPDATE video
            SET comment_count = video.comment_count + 1,
                updated_at = NOW()
            WHERE video.id IN (SELECT video_id FROM inserted)
            RETURNING video.id
        )
        SELECT inserted.id, inserted.video_id, inserted.user_id,
               comment_user.name AS username, comment_user.avatar,
               inserted.parent_id, inserted.reply_to_user_id,
               reply_user.name AS reply_to_username, inserted.content,
               inserted.like_count, FALSE AS liked,
               inserted.created_at, inserted.updated_at
        FROM inserted
        INNER JOIN updated ON updated.id = inserted.video_id
        INNER JOIN "user" AS comment_user ON comment_user.id = inserted.user_id
        LEFT JOIN "user" AS reply_user ON reply_user.id = inserted.reply_to_user_id
        "#,
    )
    .bind(video_id)
    .bind(user_id)
    .bind(content)
    .bind(input.parent_id)
    .bind(input.reply_to_user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(internal_db_error)?
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok((StatusCode::CREATED, Json(ApiResponse::success(comment))))
}

pub async fn like_comment(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(comment_id): AxumPath<Uuid>,
) -> Result<Json<ApiResponse<VideoCommentLikeState>>, StatusCode> {
    set_comment_like(&state.db, comment_id, authenticated_user_id(&claims)?, true)
        .await
        .map(|state| Json(ApiResponse::success(state)))
}

pub async fn unlike_comment(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(comment_id): AxumPath<Uuid>,
) -> Result<Json<ApiResponse<VideoCommentLikeState>>, StatusCode> {
    set_comment_like(
        &state.db,
        comment_id,
        authenticated_user_id(&claims)?,
        false,
    )
    .await
    .map(|state| Json(ApiResponse::success(state)))
}

pub async fn view(
    State(state): State<AppState>,
    claims: Option<Extension<Claims>>,
    headers: HeaderMap,
    jar: CookieJar,
    AxumPath(video_id): AxumPath<Uuid>,
) -> Result<(CookieJar, Json<ApiResponse<VideoViewState>>), StatusCode> {
    let user_id = optional_user_id(claims)?;
    ensure_video_access(&state.db, video_id, user_id).await?;
    let (visitor_id, jar) = if user_id.is_some() {
        (None, jar)
    } else if let Some(visitor_id) = jar
        .get(VIDEO_VISITOR_COOKIE)
        .and_then(|cookie| Uuid::parse_str(cookie.value()).ok())
    {
        (Some(visitor_id), jar)
    } else {
        let visitor_id = Uuid::new_v4();
        (
            Some(visitor_id),
            jar.add(build_visitor_cookie(&state, &headers, visitor_id)),
        )
    };
    let (view_date, _) = view_date_and_ttl(Utc::now()).ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let result = sqlx::query_as::<_, VideoViewState>(
        r#"
        WITH inserted AS (
            INSERT INTO video_view (video_id, user_id, visitor_id, viewed_on)
            VALUES ($1, $2, $3, $4)
            RETURNING video_id
        ),
        updated AS (
            UPDATE video
            SET view_count = video.view_count + 1,
                updated_at = NOW()
            WHERE video.id IN (SELECT video_id FROM inserted)
            RETURNING video.id, video.view_count
        )
        SELECT updated.id AS video_id, TRUE AS counted,
               updated.view_count AS view_count
        FROM updated
        "#,
    )
    .bind(video_id)
    .bind(user_id)
    .bind(visitor_id)
    .bind(view_date)
    .fetch_optional(&state.db)
    .await;

    match result {
        Ok(Some(value)) => Ok((jar, Json(ApiResponse::success(value)))),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::error!(%error, %video_id, "Failed to count video view");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub async fn collections(
    State(state): State<AppState>,
    claims: Option<Extension<Claims>>,
    Query(query): Query<VideoCollectionQuery>,
) -> Result<Json<ApiResponse<Vec<VideoCollection>>>, StatusCode> {
    let user_id = optional_user_id(claims)?;
    if query.mine.unwrap_or(false) && user_id.is_none() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let collections = sqlx::query_as::<_, VideoCollection>(
        r#"
        SELECT video_collection.id, video_collection.user_id,
               "user".name AS username, "user".avatar,
               video_collection.title, video_collection.description,
               video_collection.visibility,
               video_collection.include_favorites,
               video_collection.category_slug,
               COUNT(DISTINCT video.id)::bigint AS video_count,
               COALESCE(SUM(video.view_count), 0)::bigint AS total_views,
               (
                   SELECT cover_video.cover_url
                   FROM video AS cover_video
                   WHERE (
                         cover_video.user_id = video_collection.user_id
                         OR (
                             video_collection.include_favorites
                             AND EXISTS (
                                 SELECT 1 FROM video_favorite
                                 WHERE video_favorite.video_id = cover_video.id
                                   AND video_favorite.user_id = video_collection.user_id
                             )
                             AND cover_video.visibility = 'public'
                             AND cover_video.status = 'ready'
                             AND cover_video.published_at IS NOT NULL
                         )
                   )
                     AND (
                         video_collection.category_slug IS NULL
                         OR EXISTS (
                             SELECT 1
                             FROM regexp_split_to_table(
                                 regexp_replace(
                                     COALESCE(cover_video.description, ''),
                                     '#',
                                     '',
                                     'g'
                                 ),
                                 '\s+'
                             ) AS video_category
                             WHERE lower(video_category)
                                 = lower(video_collection.category_slug)
                         )
                     )
                     AND (
                         cover_video.user_id = $1
                         OR (
                             cover_video.visibility = 'public'
                             AND cover_video.status = 'ready'
                             AND cover_video.published_at IS NOT NULL
                         )
                     )
                   ORDER BY cover_video.created_at DESC, cover_video.id DESC
                   LIMIT 1
               ) AS cover_url,
               video_collection.created_at, video_collection.updated_at
        FROM video_collection
        INNER JOIN "user" ON "user".id = video_collection.user_id
        LEFT JOIN video
            ON (
                video.user_id = video_collection.user_id
                OR (
                    video_collection.include_favorites
                    AND EXISTS (
                        SELECT 1 FROM video_favorite
                        WHERE video_favorite.video_id = video.id
                          AND video_favorite.user_id = video_collection.user_id
                    )
                    AND video.visibility = 'public'
                    AND video.status = 'ready'
                    AND video.published_at IS NOT NULL
                )
            )
           AND (
               video_collection.category_slug IS NULL
               OR EXISTS (
                   SELECT 1
                   FROM regexp_split_to_table(
                       regexp_replace(
                           COALESCE(video.description, ''),
                           '#',
                           '',
                           'g'
                       ),
                       '\s+'
                   ) AS video_category
                   WHERE lower(video_category)
                       = lower(video_collection.category_slug)
               )
           )
           AND (
               video.user_id = $1
               OR (
                   video.visibility = 'public'
                   AND video.status = 'ready'
                   AND video.published_at IS NOT NULL
               )
           )
        WHERE (
            CASE WHEN $2::boolean
                THEN video_collection.user_id = $1
                ELSE video_collection.visibility = 'public'
                     OR video_collection.user_id = $1
            END
        )
        GROUP BY video_collection.id, "user".name, "user".avatar
        ORDER BY video_collection.created_at DESC, video_collection.id DESC
        "#,
    )
    .bind(user_id)
    .bind(query.mine.unwrap_or(false))
    .fetch_all(&state.db)
    .await
    .map_err(internal_db_error)?;
    Ok(Json(ApiResponse::success(collections)))
}

pub async fn create_collection(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(input): Json<CreateVideoCollection>,
) -> Result<(StatusCode, Json<ApiResponse<VideoCollection>>), StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    let title = validate_collection_title(&input.title)?;
    let visibility = validate_visibility(input.visibility.as_deref().unwrap_or("public"))?;
    let category_slug = input
        .category_slug
        .as_deref()
        .map(normalize_slug)
        .filter(|value| !value.is_empty());
    if let Some(category_slug) = category_slug.as_deref() {
        let belongs_to_playlist = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM video_category_map
                INNER JOIN video_category
                    ON video_category.id = video_category_map.category_id
                INNER JOIN video
                    ON video.id = video_category_map.video_id
                WHERE video_category.slug = $1
                  AND video.user_id = $2
                  AND video.status = 'ready'
            )
            "#,
        )
        .bind(category_slug)
        .bind(user_id)
        .fetch_one(&state.db)
        .await
        .map_err(internal_db_error)?;
        if !belongs_to_playlist {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let collection_id = sqlx::query_scalar::<_, Uuid>(
        r#"
        INSERT INTO video_collection (
            user_id, title, description, visibility, include_favorites, category_slug
        )
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id
        "#,
    )
    .bind(user_id)
    .bind(title)
    .bind(input.description.and_then(non_empty))
    .bind(visibility)
    .bind(input.include_favorites)
    .bind(category_slug)
    .fetch_one(&state.db)
    .await
    .map_err(internal_db_error)?;
    let collection = load_collection(&state.db, collection_id, Some(user_id)).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::success(collection))))
}

pub async fn update_collection(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(collection_id): AxumPath<Uuid>,
    Json(input): Json<UpdateVideoCollection>,
) -> Result<Json<ApiResponse<VideoCollection>>, StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    let title = input
        .title
        .as_deref()
        .map(validate_collection_title)
        .transpose()?;
    let visibility = input
        .visibility
        .as_deref()
        .map(validate_visibility)
        .transpose()?;
    let result = sqlx::query(
        r#"
        UPDATE video_collection
        SET title = COALESCE($3, title),
            description = CASE WHEN $4 THEN $5 ELSE description END,
            visibility = COALESCE($6, visibility),
            include_favorites = COALESCE($7, include_favorites),
            category_slug = COALESCE($8, category_slug),
            updated_at = NOW()
        WHERE id = $1 AND user_id = $2
        "#,
    )
    .bind(collection_id)
    .bind(user_id)
    .bind(title)
    .bind(input.description.is_some())
    .bind(input.description.and_then(non_empty))
    .bind(visibility)
    .bind(input.include_favorites)
    .bind(
        input
            .category_slug
            .as_deref()
            .map(normalize_slug)
            .filter(|value| !value.is_empty()),
    )
    .execute(&state.db)
    .await
    .map_err(internal_db_error)?;
    if result.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(ApiResponse::success(
        load_collection(&state.db, collection_id, Some(user_id)).await?,
    )))
}

pub async fn delete_collection(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(collection_id): AxumPath<Uuid>,
) -> Result<Json<ApiResponse<()>>, StatusCode> {
    let result = sqlx::query("DELETE FROM video_collection WHERE id = $1 AND user_id = $2")
        .bind(collection_id)
        .bind(authenticated_user_id(&claims)?)
        .execute(&state.db)
        .await
        .map_err(internal_db_error)?;
    if result.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(ApiResponse::success(())))
}

async fn load_video(
    db: &PgPool,
    video_id: Uuid,
    user_id: Option<Uuid>,
) -> Result<Video, StatusCode> {
    sqlx::query_as::<_, Video>(
        r#"
        SELECT video.id, video.user_id, "user".name AS username, "user".avatar,
               video.title, video.description, video.cover_url, video.duration,
               video.width, video.height, video.fps::double precision AS fps,
               video.size, video.origin_file_url, video.hls_master_url,
               video.status, video.visibility, video.processing_progress,
               video.processing_error, video.view_count, video.like_count,
               video.comment_count, video.favorite_count,
               EXISTS (
                   SELECT 1 FROM video_like
                   WHERE video_like.video_id = video.id
                     AND video_like.user_id = $2
               ) AS liked,
               EXISTS (
                   SELECT 1 FROM video_favorite
                   WHERE video_favorite.video_id = video.id
                     AND video_favorite.user_id = $2
               ) AS favorited,
               COALESCE(video.user_id = $2, FALSE) AS owned,
               COALESCE((
                   SELECT jsonb_agg(
                       jsonb_build_object(
                           'id', video_category.id,
                           'slug', video_category.slug,
                           'name_zh', video_category.name_zh,
                           'name_en', video_category.name_en
                       )
                       ORDER BY video_category.name_en ASC
                   )
                   FROM video_category_map
                   INNER JOIN video_category
                       ON video_category.id = video_category_map.category_id
                   WHERE video_category_map.video_id = video.id
               ), '[]'::jsonb) AS categories,
               video.created_at, video.updated_at
        FROM video
        INNER JOIN "user" ON "user".id = video.user_id
        WHERE video.id = $1
          AND (
              video.user_id = $2
              OR (
                  video.visibility = 'public'
                  AND video.status = 'ready'
                  AND video.published_at IS NOT NULL
              )
          )
        "#,
    )
    .bind(video_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .map_err(internal_db_error)?
    .ok_or(StatusCode::NOT_FOUND)
}

async fn ensure_video_owner(db: &PgPool, video_id: Uuid, user_id: Uuid) -> Result<(), StatusCode> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM video WHERE id = $1 AND user_id = $2)",
    )
    .bind(video_id)
    .bind(user_id)
    .fetch_one(db)
    .await
    .map_err(internal_db_error)?;
    exists.then_some(()).ok_or(StatusCode::NOT_FOUND)
}

async fn ensure_video_access(
    db: &PgPool,
    video_id: Uuid,
    user_id: Option<Uuid>,
) -> Result<(), StatusCode> {
    let exists = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM video
            WHERE id = $1
              AND (
                  user_id = $2
                  OR (
                      visibility = 'public'
                      AND status = 'ready'
                      AND published_at IS NOT NULL
                  )
              )
        )
        "#,
    )
    .bind(video_id)
    .bind(user_id)
    .fetch_one(db)
    .await
    .map_err(internal_db_error)?;
    exists.then_some(()).ok_or(StatusCode::NOT_FOUND)
}

async fn replace_video_categories(
    transaction: &mut Transaction<'_, Postgres>,
    video_id: Uuid,
    slugs: &[String],
) -> Result<(), StatusCode> {
    sqlx::query("DELETE FROM video_category_map WHERE video_id = $1")
        .bind(video_id)
        .execute(&mut **transaction)
        .await
        .map_err(internal_db_error)?;
    if slugs.is_empty() {
        return Ok(());
    }

    sqlx::query(
        r#"
        WITH input AS (
            SELECT slug, INITCAP(REPLACE(slug, '-', ' ')) AS display
            FROM UNNEST($2::text[]) AS input_slug(slug)
        ),
        categories AS (
            INSERT INTO video_category (slug, name_zh, name_en)
            SELECT slug, display, display
            FROM input
            ON CONFLICT (slug) DO UPDATE SET slug = EXCLUDED.slug
            RETURNING id
        )
        INSERT INTO video_category_map (video_id, category_id)
        SELECT $1, id
        FROM categories
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(video_id)
    .bind(slugs)
    .execute(&mut **transaction)
    .await
    .map_err(internal_db_error)?;
    Ok(())
}

async fn set_video_reaction(
    db: &PgPool,
    video_id: Uuid,
    user_id: Uuid,
    is_like: bool,
    active: bool,
) -> Result<VideoReactionState, StatusCode> {
    let (table, counter) = if is_like {
        ("video_like", "like_count")
    } else {
        ("video_favorite", "favorite_count")
    };
    let operation = if active {
        format!(
            r#"
            WITH target AS (
                SELECT id FROM video
                WHERE id = $1
                  AND (
                      user_id = $2
                      OR (
                          visibility = 'public'
                          AND status = 'ready'
                          AND published_at IS NOT NULL
                      )
                  )
            ),
            changed AS (
                INSERT INTO {table} (video_id, user_id)
                SELECT id, $2 FROM target
                ON CONFLICT (video_id, user_id) DO NOTHING
                RETURNING video_id
            ),
            updated AS (
                UPDATE video
                SET {counter} = {counter} + (SELECT COUNT(*) FROM changed),
                    updated_at = CASE WHEN EXISTS(SELECT 1 FROM changed)
                                      THEN NOW() ELSE updated_at END
                WHERE id IN (SELECT id FROM target)
                RETURNING id, {counter}
            )
            SELECT id AS video_id, TRUE AS active, {counter} AS count
            FROM updated
            "#
        )
    } else {
        format!(
            r#"
            WITH target AS (
                SELECT id FROM video
                WHERE id = $1
                  AND (
                      user_id = $2
                      OR (
                          visibility = 'public'
                          AND status = 'ready'
                          AND published_at IS NOT NULL
                      )
                  )
            ),
            changed AS (
                DELETE FROM {table}
                WHERE video_id = $1 AND user_id = $2
                RETURNING video_id
            ),
            updated AS (
                UPDATE video
                SET {counter} = GREATEST(0, {counter} - (SELECT COUNT(*) FROM changed)),
                    updated_at = CASE WHEN EXISTS(SELECT 1 FROM changed)
                                      THEN NOW() ELSE updated_at END
                WHERE id IN (SELECT id FROM target)
                RETURNING id, {counter}
            )
            SELECT id AS video_id, FALSE AS active, {counter} AS count
            FROM updated
            "#
        )
    };
    sqlx::query_as::<_, VideoReactionState>(&operation)
        .bind(video_id)
        .bind(user_id)
        .fetch_optional(db)
        .await
        .map_err(internal_db_error)?
        .ok_or(StatusCode::NOT_FOUND)
}

async fn set_comment_like(
    db: &PgPool,
    comment_id: Uuid,
    user_id: Uuid,
    liked: bool,
) -> Result<VideoCommentLikeState, StatusCode> {
    let query = if liked {
        r#"
        WITH target AS (
            SELECT video_comment.id
            FROM video_comment
            INNER JOIN video ON video.id = video_comment.video_id
            WHERE video_comment.id = $1
              AND video_comment.deleted_at IS NULL
              AND (
                  video.user_id = $2
                  OR (
                      video.visibility = 'public'
                      AND video.status = 'ready'
                      AND video.published_at IS NOT NULL
                  )
              )
        ),
        changed AS (
            INSERT INTO video_comment_like (comment_id, user_id)
            SELECT id, $2 FROM target
            ON CONFLICT DO NOTHING
            RETURNING comment_id
        ),
        updated AS (
            UPDATE video_comment
            SET like_count = like_count + (SELECT COUNT(*) FROM changed),
                updated_at = CASE WHEN EXISTS(SELECT 1 FROM changed)
                                  THEN NOW() ELSE updated_at END
            WHERE id IN (SELECT id FROM target)
            RETURNING id, like_count
        )
        SELECT id AS comment_id, TRUE AS liked, like_count FROM updated
        "#
    } else {
        r#"
        WITH target AS (
            SELECT video_comment.id
            FROM video_comment
            INNER JOIN video ON video.id = video_comment.video_id
            WHERE video_comment.id = $1
              AND video_comment.deleted_at IS NULL
              AND (
                  video.user_id = $2
                  OR (
                      video.visibility = 'public'
                      AND video.status = 'ready'
                      AND video.published_at IS NOT NULL
                  )
              )
        ),
        changed AS (
            DELETE FROM video_comment_like
            WHERE comment_id = $1 AND user_id = $2
            RETURNING comment_id
        ),
        updated AS (
            UPDATE video_comment
            SET like_count = GREATEST(0, like_count - (SELECT COUNT(*) FROM changed)),
                updated_at = CASE WHEN EXISTS(SELECT 1 FROM changed)
                                  THEN NOW() ELSE updated_at END
            WHERE id IN (SELECT id FROM target)
            RETURNING id, like_count
        )
        SELECT id AS comment_id, FALSE AS liked, like_count FROM updated
        "#
    };
    sqlx::query_as::<_, VideoCommentLikeState>(query)
        .bind(comment_id)
        .bind(user_id)
        .fetch_optional(db)
        .await
        .map_err(internal_db_error)?
        .ok_or(StatusCode::NOT_FOUND)
}

async fn load_view_state(
    db: &PgPool,
    video_id: Uuid,
    counted: bool,
) -> Result<VideoViewState, StatusCode> {
    sqlx::query_as::<_, VideoViewState>(
        "SELECT id AS video_id, $2 AS counted, view_count FROM video WHERE id = $1",
    )
    .bind(video_id)
    .bind(counted)
    .fetch_optional(db)
    .await
    .map_err(internal_db_error)?
    .ok_or(StatusCode::NOT_FOUND)
}

async fn load_collection(
    db: &PgPool,
    collection_id: Uuid,
    user_id: Option<Uuid>,
) -> Result<VideoCollection, StatusCode> {
    sqlx::query_as::<_, VideoCollection>(
        r#"
        SELECT video_collection.id, video_collection.user_id,
               "user".name AS username, "user".avatar,
               video_collection.title, video_collection.description,
               video_collection.visibility,
               video_collection.include_favorites,
               video_collection.category_slug,
               COUNT(DISTINCT video.id)::bigint AS video_count,
               COALESCE(SUM(video.view_count), 0)::bigint AS total_views,
               (
                   SELECT cover_video.cover_url
                   FROM video AS cover_video
                   WHERE (
                         cover_video.user_id = video_collection.user_id
                         OR (
                             video_collection.include_favorites
                             AND EXISTS (
                                 SELECT 1 FROM video_favorite
                                 WHERE video_favorite.video_id = cover_video.id
                                   AND video_favorite.user_id = video_collection.user_id
                             )
                             AND cover_video.visibility = 'public'
                             AND cover_video.status = 'ready'
                             AND cover_video.published_at IS NOT NULL
                         )
                   )
                     AND (
                         video_collection.category_slug IS NULL
                         OR EXISTS (
                             SELECT 1
                             FROM regexp_split_to_table(
                                 regexp_replace(
                                     COALESCE(cover_video.description, ''),
                                     '#',
                                     '',
                                     'g'
                                 ),
                                 '\s+'
                             ) AS video_category
                             WHERE lower(video_category)
                                 = lower(video_collection.category_slug)
                         )
                     )
                     AND (
                         cover_video.user_id = $2
                         OR (
                             cover_video.visibility = 'public'
                             AND cover_video.status = 'ready'
                             AND cover_video.published_at IS NOT NULL
                         )
                     )
                   ORDER BY cover_video.created_at DESC, cover_video.id DESC
                   LIMIT 1
               ) AS cover_url,
               video_collection.created_at, video_collection.updated_at
        FROM video_collection
        INNER JOIN "user" ON "user".id = video_collection.user_id
        LEFT JOIN video
            ON (
                video.user_id = video_collection.user_id
                OR (
                    video_collection.include_favorites
                    AND EXISTS (
                        SELECT 1 FROM video_favorite
                        WHERE video_favorite.video_id = video.id
                          AND video_favorite.user_id = video_collection.user_id
                    )
                    AND video.visibility = 'public'
                    AND video.status = 'ready'
                    AND video.published_at IS NOT NULL
                )
            )
           AND (
               video_collection.category_slug IS NULL
               OR EXISTS (
                   SELECT 1
                   FROM regexp_split_to_table(
                       regexp_replace(
                           COALESCE(video.description, ''),
                           '#',
                           '',
                           'g'
                       ),
                       '\s+'
                   ) AS video_category
                   WHERE lower(video_category)
                       = lower(video_collection.category_slug)
               )
           )
           AND (
               video.user_id = $2
               OR (
                   video.visibility = 'public'
                   AND video.status = 'ready'
                   AND video.published_at IS NOT NULL
               )
           )
        WHERE video_collection.id = $1
          AND (
              video_collection.visibility = 'public'
              OR video_collection.user_id = $2
          )
        GROUP BY video_collection.id, "user".name, "user".avatar
        "#,
    )
    .bind(collection_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .map_err(internal_db_error)?
    .ok_or(StatusCode::NOT_FOUND)
}

async fn probe_video_metadata(source_path: &FsPath) -> Result<ProbedVideo, StatusCode> {
    let ffprobe = std::env::var("FFPROBE_PATH").unwrap_or_else(|_| "ffprobe".to_owned());
    let output = Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,avg_frame_rate:stream_tags=rotate:stream_side_data=rotation:format=duration",
            "-of",
            "json",
        ])
        .arg(source_path)
        .output()
        .await
        .map_err(|error| {
            tracing::error!(%error, executable = %ffprobe, "Failed to start FFprobe for video");
            StatusCode::SERVICE_UNAVAILABLE
        })?;
    if !output.status.success() {
        tracing::error!(
            error = %String::from_utf8_lossy(&output.stderr).trim(),
            "FFprobe video inspection failed"
        );
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    parse_video_metadata(&output.stdout).ok_or(StatusCode::UNPROCESSABLE_ENTITY)
}

fn parse_video_metadata(output: &[u8]) -> Option<ProbedVideo> {
    let probe = serde_json::from_slice::<VideoProbeOutput>(output).ok()?;
    let stream = probe.streams.first()?;
    if stream.width == 0 || stream.height == 0 {
        return None;
    }
    let seconds = probe.format?.duration?.parse::<f64>().ok()?;
    if !seconds.is_finite() || seconds <= 0.0 {
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
    let (width, height) = if matches!(rotation, 90 | 270) {
        (stream.height, stream.width)
    } else {
        (stream.width, stream.height)
    };
    Some(ProbedVideo {
        width,
        height,
        duration_us: (seconds * 1_000_000.0).round() as u64,
        fps: stream.avg_frame_rate.as_deref().and_then(parse_frame_rate),
    })
}

fn parse_frame_rate(value: &str) -> Option<f64> {
    let (numerator, denominator) = value.split_once('/')?;
    let numerator = numerator.parse::<f64>().ok()?;
    let denominator = denominator.parse::<f64>().ok()?;
    (denominator > 0.0)
        .then_some(numerator / denominator)
        .filter(|fps| fps.is_finite() && *fps > 0.0)
}

fn spawn_video_processing(
    db: PgPool,
    transcode_slots: Arc<Semaphore>,
    video_id: Uuid,
    pending: PendingVideo,
) {
    tokio::spawn(async move {
        let _permit = match transcode_slots.acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => {
                tracing::error!(%video_id, "Video media processing is unavailable");
                return;
            }
        };
        let ffmpeg = std::env::var("FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".to_owned());
        if let Err(status) = generate_video_poster(
            &ffmpeg,
            &pending.source_path,
            &pending.output_directory,
            pending.duration_us,
        )
        .await
        {
            tracing::warn!(
                %video_id,
                %status,
                "Unable to generate the early video poster; transcoding will retry"
            );
        } else {
            tracing::info!(%video_id, "Generated video poster before HLS transcoding");
        }

        let result = transcode_video_to_hls(
            &db,
            video_id,
            &pending.source_path,
            &pending.output_directory,
            pending.duration_us,
        )
        .await;
        let (status, error) = match result {
            Ok(()) => ("ready", None),
            Err(status) => {
                tracing::error!(%video_id, %status, "Video processing failed");
                ("failed", Some(status.to_string()))
            }
        };
        if let Err(db_error) = sqlx::query(
            r#"
            UPDATE video
            SET status = $2,
                processing_progress = CASE WHEN $2 = 'ready' THEN 100 ELSE processing_progress END,
                processing_error = $3,
                updated_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(video_id)
        .bind(status)
        .bind(error)
        .execute(&db)
        .await
        {
            tracing::error!(%db_error, %video_id, "Failed to save video processing status");
        }
    });
}

fn spawn_uploaded_video_processing(
    db: PgPool,
    transcode_slots: Arc<Semaphore>,
    video_id: Uuid,
    source_path: PathBuf,
    output_directory: PathBuf,
) {
    tokio::spawn(async move {
        tracing::info!(%video_id, "Started uploaded video metadata inspection");
        let metadata = match probe_video_metadata(&source_path).await {
            Ok(metadata) => metadata,
            Err(status) => {
                mark_video_processing_failed(
                    &db,
                    video_id,
                    format!("Unable to inspect the uploaded video ({status})."),
                )
                .await;
                return;
            }
        };
        tracing::info!(
            %video_id,
            duration_us = metadata.duration_us,
            width = metadata.width,
            height = metadata.height,
            "Uploaded video metadata inspection completed"
        );
        let size = match tokio::fs::metadata(&source_path).await {
            Ok(metadata) => metadata.len().min(i64::MAX as u64) as i64,
            Err(error) => {
                tracing::error!(%error, %video_id, "Failed to read uploaded video metadata");
                mark_video_processing_failed(
                    &db,
                    video_id,
                    "Unable to read the uploaded video.".to_owned(),
                )
                .await;
                return;
            }
        };
        let updated = sqlx::query(
            r#"
            UPDATE video
            SET duration = $2,
                width = $3,
                height = $4,
                fps = $5::double precision::numeric,
                size = $6,
                status = 'processing',
                processing_progress = 0,
                processing_error = NULL,
                updated_at = NOW()
            WHERE id = $1 AND status = 'uploading'
            "#,
        )
        .bind(video_id)
        .bind(((metadata.duration_us + 999_999) / 1_000_000) as i32)
        .bind(metadata.width as i32)
        .bind(metadata.height as i32)
        .bind(metadata.fps)
        .bind(size)
        .execute(&db)
        .await;

        match updated {
            Ok(result) if result.rows_affected() == 1 => {
                tracing::info!(%video_id, "Uploaded video marked as processing");
                spawn_video_processing(
                    db,
                    transcode_slots,
                    video_id,
                    PendingVideo {
                        source_path,
                        output_directory,
                        duration_us: metadata.duration_us,
                    },
                );
            }
            Ok(_) => {
                tracing::warn!(
                    %video_id,
                    "Skipped uploaded video processing because its status changed"
                );
            }
            Err(error) => {
                tracing::error!(%error, %video_id, "Failed to finalize uploaded video");
                mark_video_processing_failed(
                    &db,
                    video_id,
                    "Unable to prepare the uploaded video.".to_owned(),
                )
                .await;
            }
        }
    });
}

async fn mark_video_processing_failed(db: &PgPool, video_id: Uuid, error: String) {
    if let Err(db_error) = sqlx::query(
        r#"
        UPDATE video
        SET status = 'failed',
            processing_error = $2,
            updated_at = NOW()
        WHERE id = $1 AND status = 'uploading'
        "#,
    )
    .bind(video_id)
    .bind(error)
    .execute(db)
    .await
    {
        tracing::error!(%db_error, %video_id, "Failed to save video upload failure");
    }
}

async fn transcode_video_to_hls(
    db: &PgPool,
    video_id: Uuid,
    source_path: &FsPath,
    output_directory: &FsPath,
    duration_us: u64,
) -> Result<(), StatusCode> {
    tracing::info!(%video_id, "Video processing queued for transcode slot");
    tracing::info!(%video_id, "Video processing acquired transcode slot");
    let ffmpeg = std::env::var("FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".to_owned());
    let playlist_path = output_directory.join("master.m3u8");
    let segment_pattern = output_directory.join("segment-%05d.ts");
    let mut child = Command::new(&ffmpeg)
        .args([
            "-hide_banner", "-loglevel", "error", "-nostdin",
            "-progress", "pipe:1", "-nostats", "-y", "-i",
        ])
        .arg(source_path)
        .args([
            "-map", "0:v:0", "-map", "0:a:0?", "-c:v", "libx264",
            "-preset", "veryfast", "-crf", "23",
            "-vf", "scale=w='min(3840,iw)':h='min(2160,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2",
            "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "128k",
            "-ar", "48000", "-ac", "2",
            "-force_key_frames", "expr:gte(t,n_forced*6)",
            "-f", "hls", "-hls_time", HLS_SEGMENT_SECONDS,
            "-hls_playlist_type", "vod", "-hls_segment_type", "mpegts",
            "-hls_flags", "independent_segments", "-hls_segment_filename",
        ])
        .arg(segment_pattern)
        .arg(playlist_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            tracing::error!(%error, executable = %ffmpeg, "Failed to start FFmpeg for video");
            StatusCode::SERVICE_UNAVAILABLE
        })?;
    update_processing_progress(db, video_id, 1).await;
    tracing::info!(%video_id, executable = %ffmpeg, "Started FFmpeg video HLS transcoding");
    let stdout = child
        .stdout
        .take()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes).await;
        bytes
    });
    let mut lines = BufReader::new(stdout).lines();
    let mut last_progress = 0_i16;
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        let Some(out_time_us) = parse_ffmpeg_out_time_us(&line) else {
            continue;
        };
        let progress = ((out_time_us.saturating_mul(99) / duration_us.max(1)).min(99)) as i16;
        if progress > last_progress {
            last_progress = progress;
            update_processing_progress(db, video_id, progress).await;
        }
    }
    let status = child
        .wait()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let stderr = stderr_task.await.unwrap_or_default();
    if !status.success() {
        tracing::error!(
            status = ?status.code(),
            error = %String::from_utf8_lossy(&stderr).trim(),
            "FFmpeg video HLS transcoding failed"
        );
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    tracing::info!(%video_id, "FFmpeg video HLS transcoding completed; generating poster");
    generate_video_poster(&ffmpeg, source_path, output_directory, duration_us).await?;
    Ok(())
}

fn parse_ffmpeg_out_time_us(line: &str) -> Option<u64> {
    line.strip_prefix("out_time_us=")
        .or_else(|| line.strip_prefix("out_time_ms="))
        .and_then(|value| value.parse::<u64>().ok())
}

async fn update_processing_progress(db: &PgPool, video_id: Uuid, progress: i16) {
    if let Err(error) = sqlx::query(
        r#"
        UPDATE video
        SET processing_progress = GREATEST(processing_progress, $2),
            updated_at = NOW()
        WHERE id = $1 AND status = 'processing'
        "#,
    )
    .bind(video_id)
    .bind(progress)
    .execute(db)
    .await
    {
        tracing::warn!(%error, %video_id, %progress, "Failed to save video progress");
    }
}

async fn generate_video_poster(
    ffmpeg: &str,
    source_path: &FsPath,
    output_directory: &FsPath,
    duration_us: u64,
) -> Result<(), StatusCode> {
    // A user-selected cover always wins over the generated poster.
    if tokio::fs::try_exists(output_directory.join("cover.webp"))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Ok(());
    }
    if tokio::fs::try_exists(output_directory.join("poster.webp"))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Ok(());
    }
    let output = extract_video_poster_frames(ffmpeg, source_path, duration_us).await?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let selected_frame =
        select_video_poster_frame(&output.stdout).ok_or(StatusCode::UNPROCESSABLE_ENTITY)?;
    let encoded = tokio::task::spawn_blocking(move || encode_cover(&selected_frame))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
    tokio::fs::write(output_directory.join("poster.webp"), encoded)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn extract_video_poster_frames(
    ffmpeg: &str,
    source_path: &FsPath,
    duration_us: u64,
) -> Result<std::process::Output, StatusCode> {
    let duration_seconds = (duration_us as f64 / 1_000_000.0).max(1.0);
    let fps = format!(
        "{}/{}",
        VIDEO_POSTER_CANDIDATE_COUNT,
        format!("{duration_seconds:.3}")
    );
    let mut command = Command::new(ffmpeg);
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"]);
    command
        .arg(source_path)
        .args([
            "-map",
            "0:v:0",
            "-vf",
            &format!("fps={fps},scale='min(640,iw)':-2:flags=lanczos"),
            "-frames:v",
            &VIDEO_POSTER_CANDIDATE_COUNT.to_string(),
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
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}

fn select_video_poster_frame(jpeg_stream: &[u8]) -> Option<Vec<u8>> {
    let mut candidates = Vec::new();
    let mut cursor = 0;
    while let Some(start_offset) = jpeg_stream[cursor..]
        .windows(2)
        .position(|window| window == [0xff, 0xd8])
    {
        let start = cursor + start_offset;
        let end_offset = jpeg_stream[start + 2..]
            .windows(2)
            .position(|window| window == [0xff, 0xd9])?;
        let end = start + 2 + end_offset + 2;
        candidates.push(&jpeg_stream[start..end]);
        cursor = end;
    }

    let mut best: Option<(f32, Vec<u8>)> = None;
    let mut fallback: Option<(f32, Vec<u8>)> = None;
    for frame in candidates {
        let Ok(image) = image::load_from_memory(frame) else {
            continue;
        };
        let (score, average_luminance, dark_pixel_ratio) = score_video_poster_frame(&image);
        if fallback
            .as_ref()
            .is_none_or(|(fallback_score, _)| score > *fallback_score)
        {
            fallback = Some((score, frame.to_vec()));
        }
        let is_black = average_luminance < 0.08 || dark_pixel_ratio > 0.92;
        if is_black {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|(best_score, _)| score > *best_score)
        {
            best = Some((score, frame.to_vec()));
        }
    }

    best.or(fallback).map(|(_, frame)| frame)
}

fn score_video_poster_frame(image: &image::DynamicImage) -> (f32, f32, f32) {
    let rgb = image.to_rgb8();
    let mut luminance_sum = 0.0_f32;
    let mut luminance_square_sum = 0.0_f32;
    let mut dark_pixels = 0_u64;
    let mut pixel_count = 0_u64;

    for pixel in rgb.pixels() {
        let [red, green, blue] = pixel.0;
        let luminance =
            (0.2126 * red as f32 + 0.7152 * green as f32 + 0.0722 * blue as f32) / 255.0;
        luminance_sum += luminance;
        luminance_square_sum += luminance * luminance;
        if luminance < 0.08 {
            dark_pixels += 1;
        }
        pixel_count += 1;
    }

    if pixel_count == 0 {
        return (0.0, 0.0, 1.0);
    }

    let average_luminance = luminance_sum / pixel_count as f32;
    let variance = (luminance_square_sum / pixel_count as f32 - average_luminance.powi(2)).max(0.0);
    let contrast = variance.sqrt();
    let dark_pixel_ratio = dark_pixels as f32 / pixel_count as f32;

    // Prefer a normally exposed frame with visible detail over pure black or blown-out frames.
    let exposure_score = 1.0 - (average_luminance - 0.52).abs() / 0.52;
    let score = exposure_score.max(0.0) + contrast * 0.75;
    (score, average_luminance, dark_pixel_ratio)
}

async fn stream_video_to_file(
    field: &mut Field<'_>,
    path: &FsPath,
    max_bytes: usize,
) -> Result<(), StatusCode> {
    let file = tokio::fs::File::create(path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut file = BufWriter::with_capacity(1024 * 1024, file);
    let mut written = 0usize;
    while let Some(chunk) = field.chunk().await.map_err(|_| StatusCode::BAD_REQUEST)? {
        written = written.saturating_add(chunk.len());
        if written > max_bytes {
            drop(file);
            let _ = tokio::fs::remove_file(path).await;
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        file.write_all(&chunk)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    file.flush()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn max_video_upload_bytes(db: &PgPool, user_id: Uuid) -> Result<usize, StatusCode> {
    let subscription = sqlx::query_as::<_, (String, String, Option<DateTime<Utc>>)>(
        r#"SELECT plan, subscription_status, subscription_end_at FROM "user" WHERE id = $1"#,
    )
    .bind(user_id)
    .fetch_optional(db)
    .await
    .map_err(internal_db_error)?
    .ok_or(StatusCode::UNAUTHORIZED)?;

    Ok(
        if has_library_access(&subscription.0, &subscription.1, subscription.2) {
            MAX_VIDEO_BYTES
        } else {
            MAX_FREE_VIDEO_BYTES
        },
    )
}

fn upload_temporary_path(directory: &FsPath, extension: &str) -> PathBuf {
    directory.join(format!(".source.{extension}.uploading"))
}

fn upload_offset(headers: &HeaderMap) -> Result<u64, StatusCode> {
    headers
        .get("upload-offset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(StatusCode::BAD_REQUEST)
}

async fn load_username(db: &PgPool, user_id: Uuid) -> Result<String, StatusCode> {
    sqlx::query_scalar::<_, String>(r#"SELECT name FROM "user" WHERE id = $1"#)
        .bind(user_id)
        .fetch_optional(db)
        .await
        .map_err(internal_db_error)?
        .ok_or(StatusCode::UNAUTHORIZED)
}

fn video_directory(id: Uuid, asset_directory: Option<&str>) -> PathBuf {
    let directory = asset_directory
        .map(str::to_owned)
        .unwrap_or_else(|| id.to_string());
    FsPath::new(VIDEO_ASSET_ROOT).join(directory)
}

async fn load_upload_state(
    db: &PgPool,
    upload_id: Uuid,
    user_id: Uuid,
) -> Result<UploadStateRow, StatusCode> {
    sqlx::query_as::<_, UploadStateRow>(
        r#"
        SELECT video_upload.video_id, video.asset_directory,
               video_upload.file_extension, video_upload.total_bytes,
               video_upload.uploaded_bytes, video_upload.status
        FROM video_upload
        INNER JOIN video ON video.id = video_upload.video_id
        WHERE id = $1 AND user_id = $2
        "#,
    )
    .bind(upload_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .map_err(internal_db_error)?
    .ok_or(StatusCode::NOT_FOUND)
}

fn upload_session_response(
    upload_id: Uuid,
    video: Video,
    state: &UploadStateRow,
) -> VideoUploadSession {
    VideoUploadSession {
        upload_id,
        video,
        chunk_size: VIDEO_UPLOAD_CHUNK_BYTES as u64,
        uploaded_bytes: state.uploaded_bytes.max(0) as u64,
        total_bytes: state.total_bytes.max(0) as u64,
        complete: state.status == "complete",
    }
}

async fn read_field_limited(field: &mut Field<'_>, limit: usize) -> Result<Vec<u8>, StatusCode> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(|_| StatusCode::BAD_REQUEST)? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn encode_cover(bytes: &[u8]) -> image::ImageResult<Vec<u8>> {
    let image = image::load_from_memory(bytes)?;
    let resized = image.resize_to_fill(
        VIDEO_COVER_WIDTH,
        VIDEO_COVER_HEIGHT,
        image::imageops::FilterType::Lanczos3,
    );
    let mut encoded = Cursor::new(Vec::new());
    resized.write_to(&mut encoded, ImageFormat::WebP)?;
    Ok(encoded.into_inner())
}

async fn cleanup_video_directory(path: &FsPath) {
    if let Err(error) = tokio::fs::remove_dir_all(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, path = %path.display(), "Failed to clean up video directory");
    }
}

async fn cleanup_failed_video_upload(db: &PgPool, video_id: Uuid, path: &FsPath) {
    if let Err(error) = sqlx::query("DELETE FROM video WHERE id = $1")
        .bind(video_id)
        .execute(db)
        .await
    {
        tracing::warn!(%error, %video_id, "Failed to remove incomplete video record");
    }
    cleanup_video_directory(path).await;
}

async fn cleanup_transcoded_video_files(directory: &FsPath) {
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let should_remove = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name == "master.m3u8" || name.starts_with("segment-"))
            .unwrap_or(false);
        if should_remove {
            let _ = tokio::fs::remove_file(path).await;
        }
    }
}

async fn source_path_for_video(directory: &FsPath) -> Result<PathBuf, StatusCode> {
    let mut entries = tokio::fs::read_dir(directory)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("source."))
        {
            return Ok(path);
        }
    }
    Err(StatusCode::NOT_FOUND)
}

async fn video_duration_us(db: &PgPool, video_id: Uuid) -> Result<u64, StatusCode> {
    let duration = sqlx::query_scalar::<_, i32>("SELECT duration FROM video WHERE id = $1")
        .bind(video_id)
        .fetch_optional(db)
        .await
        .map_err(internal_db_error)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok((duration.max(1) as u64).saturating_mul(1_000_000))
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
        Some("video/x-matroska") => Some("mkv"),
        None | Some("application/octet-stream") => match original_name
            .and_then(|name| name.rsplit_once('.'))
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .as_deref()
        {
            Some("mp4") => Some("mp4"),
            Some("webm") => Some("webm"),
            Some("mov") => Some("mov"),
            Some("m4v") => Some("m4v"),
            Some("mkv") => Some("mkv"),
            _ => None,
        },
        _ => None,
    }
}

fn default_video_title(file_name: &str) -> String {
    file_name
        .rsplit_once('.')
        .map(|(name, _)| name)
        .unwrap_or(file_name)
        .replace(['-', '_'], " ")
        .trim()
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect()
}

fn normalize_category_slugs(values: Vec<String>) -> Vec<String> {
    let mut slugs = values
        .into_iter()
        .map(|value| normalize_slug(value.trim_start_matches('#')))
        .filter(|value| !value.is_empty() && value != "all")
        .collect::<Vec<_>>();
    slugs.sort();
    slugs.dedup();
    slugs.truncate(8);
    slugs
}

fn normalize_slug(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(80)
        .collect()
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn validate_collection_title(value: &str) -> Result<String, StatusCode> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 120 {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(value.to_owned())
}

fn validate_visibility(value: &str) -> Result<&str, StatusCode> {
    matches!(value, "public" | "private")
        .then_some(value)
        .ok_or(StatusCode::BAD_REQUEST)
}

fn authenticated_user_id(claims: &Claims) -> Result<Uuid, StatusCode> {
    claims.sub.parse().map_err(|_| StatusCode::UNAUTHORIZED)
}

fn optional_user_id(claims: Option<Extension<Claims>>) -> Result<Option<Uuid>, StatusCode> {
    claims
        .map(|Extension(claims)| authenticated_user_id(&claims))
        .transpose()
}

fn internal_db_error(error: sqlx::Error) -> StatusCode {
    tracing::error!(%error, "Video database operation failed");
    StatusCode::INTERNAL_SERVER_ERROR
}

fn build_visitor_cookie(
    state: &AppState,
    headers: &HeaderMap,
    visitor_id: Uuid,
) -> Cookie<'static> {
    let local = headers
        .get(ORIGIN)
        .or_else(|| headers.get(REFERER))
        .or_else(|| headers.get(HOST))
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value.contains("localhost") || value.contains("127.0.0.1") || value.contains("[::1]")
        })
        .unwrap_or_else(|| {
            state.frontend_url.starts_with("http://localhost")
                || state.frontend_url.starts_with("http://127.0.0.1")
        });
    let cookie = Cookie::build((VIDEO_VISITOR_COOKIE, visitor_id.to_string()))
        .http_only(true)
        .path("/")
        .max_age(time::Duration::days(730));
    if local {
        cookie.secure(false).same_site(SameSite::Lax).build()
    } else {
        cookie.secure(true).same_site(SameSite::None).build()
    }
}

fn view_date_and_ttl(now: DateTime<Utc>) -> Option<(NaiveDate, u64)> {
    let offset = Duration::hours(VIDEO_VIEW_UTC_OFFSET_HOURS);
    let local_now = now + offset;
    let view_date = local_now.date_naive();
    let next_local_midnight = view_date.succ_opt()?.and_hms_opt(0, 0, 0)?.and_utc();
    let next_midnight_utc = next_local_midnight - offset;
    Some((
        view_date,
        (next_midnight_utc - now).num_seconds().max(1) as u64,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        default_video_title, encode_cover, normalize_category_slugs, parse_frame_rate,
        parse_video_metadata,
    };
    use image::{DynamicImage, GenericImageView, ImageFormat, RgbImage};
    use std::io::Cursor;

    #[test]
    fn parses_video_probe_metadata() {
        let metadata = parse_video_metadata(
            br#"{"streams":[{"width":1920,"height":1080,"avg_frame_rate":"30000/1001"}],"format":{"duration":"12.5"}}"#,
        )
        .unwrap();
        assert_eq!((metadata.width, metadata.height), (1920, 1080));
        assert_eq!(metadata.duration_us, 12_500_000);
        assert!((metadata.fps.unwrap() - 29.970).abs() < 0.001);
    }

    #[test]
    fn normalizes_video_categories() {
        assert_eq!(
            normalize_category_slugs(vec![
                "#Travel".into(),
                " travel ".into(),
                "Game Play".into()
            ]),
            vec!["game-play", "travel"]
        );
    }

    #[test]
    fn formats_upload_title_and_frame_rate() {
        assert_eq!(
            default_video_title("quiet-valley_4k.mp4"),
            "quiet valley 4k"
        );
        assert_eq!(parse_frame_rate("60/1"), Some(60.0));
        assert_eq!(parse_frame_rate("0/0"), None);
    }

    #[test]
    fn encodes_video_cover_as_1920_by_1080_webp() {
        let source = DynamicImage::ImageRgb8(RgbImage::new(900, 1600));
        let mut input = Cursor::new(Vec::new());
        source.write_to(&mut input, ImageFormat::Png).unwrap();

        let encoded = encode_cover(&input.into_inner()).unwrap();
        assert_eq!(image::guess_format(&encoded).unwrap(), ImageFormat::WebP);
        assert_eq!(
            image::load_from_memory(&encoded).unwrap().dimensions(),
            (1920, 1080)
        );
    }
}
