use axum::{
    Extension, Json,
    extract::{FromRequest, Multipart, Path, Query, Request, State},
    http::StatusCode,
};
use image::{ImageFormat, imageops::FilterType};
use serde::Deserialize;
use serde::Serialize;
use std::{path::PathBuf, time::SystemTime};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::{app::AppState, common::response::ApiResponse, middleware::jwt::Claims};

const MAX_MARKDOWN_BYTES: usize = 10 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const DOC_IMAGE_WIDTH: u32 = 1200;
const DOC_IMAGE_HEIGHT: u32 = 800;

pub async fn backfill_content(db: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    let rows =
        sqlx::query_as::<_, (Uuid, String, String)>("SELECT id, markdown_path, content FROM docs")
            .fetch_all(db)
            .await?;

    for (id, markdown_path, stored_content) in rows {
        let file_content = match fs::read_to_string(&markdown_path).await {
            Ok(content) => content,
            Err(error) => {
                tracing::warn!(%error, %id, path = %markdown_path, "Unable to backfill document content");
                continue;
            }
        };
        if stored_content.trim().is_empty() && !file_content.is_empty() {
            sqlx::query("UPDATE docs SET content = $2 WHERE id = $1")
                .bind(id)
                .bind(file_content)
                .execute(db)
                .await?;
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct CreateDocInput {
    pub title: String,
    pub category: String,
    #[serde(default)]
    pub content: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct DocListItem {
    pub id: Uuid,
    pub title: String,
    pub category: String,
    pub date: String,
    pub cover: Option<String>,
    pub accent: String,
    pub content: String,
    pub public: bool,
}

#[derive(Debug, Deserialize)]
pub struct DocsListQuery {
    #[serde(default)]
    pub public: bool,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<DocsListQuery>,
    claims: Option<Extension<Claims>>,
) -> Result<Json<ApiResponse<Vec<DocListItem>>>, StatusCode> {
    if query.public {
        let docs = sqlx::query_as::<_, DocListItem>(
            r#"SELECT id, title, category, TO_CHAR(updated_at, 'YYYY-MM-DD HH24:MI') AS date,
                      cover_url AS cover, 'violet' AS accent, content, public
               FROM docs
               WHERE public = TRUE
               ORDER BY updated_at DESC"#,
        )
        .fetch_all(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        return Ok(Json(ApiResponse::success(docs)));
    }

    let Some(Extension(claims)) = claims else {
        return Ok(Json(ApiResponse::success(Vec::new())));
    };
    let user_id = user_id(&claims)?;
    let docs = sqlx::query_as::<_, DocListItem>(
        r#"SELECT id, title, category, TO_CHAR(updated_at, 'YYYY-MM-DD HH24:MI') AS date,
                  cover_url AS cover, 'violet' AS accent, content, public
           FROM docs WHERE user_id = $1 ORDER BY updated_at DESC"#,
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ApiResponse::success(docs)))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<ApiResponse<DocListItem>>), StatusCode> {
    let user_id = user_id(&claims)?;
    let username = sqlx::query_scalar::<_, String>(r#"SELECT name FROM "user" WHERE id = $1"#)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let mut title = None;
    let mut category = None;
    let mut content = None;
    let mut cover = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
    {
        let name = field.name().unwrap_or_default().to_string();
        if name == "cover" {
            let bytes = field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?;
            if bytes.len() > MAX_IMAGE_BYTES {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            cover = Some(bytes);
        } else {
            let bytes = field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?;
            if bytes.len() > MAX_MARKDOWN_BYTES {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            let value = String::from_utf8(bytes.to_vec()).map_err(|_| StatusCode::BAD_REQUEST)?;
            match name.as_str() {
                "title" => title = Some(value),
                "category" => category = Some(value),
                "content" => content = Some(value),
                _ => {}
            }
        }
    }

    let title = title
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let content = content.unwrap_or_default();
    let category = category
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let slug = title_slugify(&title);
    let username_slug = slugify(&username);
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/assets/docs")
        .join(&username_slug);
    fs::create_dir_all(&directory)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let markdown_path = directory.join(format!("{slug}.md"));

    let cover_url = if let Some(bytes) = cover {
        let decoded =
            image::load_from_memory(&bytes).map_err(|_| StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
        let resized = decoded.resize_to_fill(720, 480, FilterType::Lanczos3);
        let cover_name = format!("{username_slug}-{slug}.webp");
        let cover_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/assets/docs-cover");
        fs::create_dir_all(&cover_dir)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let cover_path = cover_dir.join(&cover_name);
        let mut output = std::io::Cursor::new(Vec::new());
        resized
            .write_to(&mut output, ImageFormat::WebP)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut image_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&cover_path)
            .await
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            })?;
        if let Err(error) = image_file.write_all(&output.into_inner()).await {
            let _ = fs::remove_file(&cover_path).await;
            return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            });
        }
        Some(format!("/api/assets/docs-cover/{cover_name}"))
    } else {
        None
    };

    let mut markdown_file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&markdown_path)
        .await
    {
        Ok(file) => file,
        Err(error) => {
            if let Some(cover_url) = &cover_url {
                if let Some(cover_name) = cover_url.rsplit('/').next() {
                    let cover_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("src/assets/docs-cover")
                        .join(cover_name);
                    let _ = fs::remove_file(cover_path).await;
                }
            }
            return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            });
        }
    };
    if let Err(error) = markdown_file.write_all(content.as_bytes()).await {
        let _ = fs::remove_file(&markdown_path).await;
        if let Some(cover_url) = &cover_url {
            if let Some(cover_name) = cover_url.rsplit('/').next() {
                let cover_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("src/assets/docs-cover")
                    .join(cover_name);
                let _ = fs::remove_file(cover_path).await;
            }
        }
        return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        });
    }

    let row = sqlx::query_as::<_, DocListItem>(
        r#"INSERT INTO docs (user_id, title, category, content, markdown_path, cover_url)
           VALUES ($1, $2, $3, $4, $5, $6)
           RETURNING id, title, category, TO_CHAR(updated_at, 'YYYY-MM-DD HH24:MI') AS date,
                     cover_url AS cover, 'violet' AS accent, content, public"#,
    )
    .bind(user_id)
    .bind(&title)
    .bind(&category)
    .bind(&content)
    .bind(markdown_path.to_string_lossy().to_string())
    .bind(&cover_url)
    .fetch_one(&state.db)
    .await;
    let row = match row {
        Ok(row) => row,
        Err(error) => {
            tracing::error!(%error, "Failed to insert document");
            let _ = fs::remove_file(&markdown_path).await;
            if let Some(cover_url) = &cover_url {
                if let Some(cover_name) = cover_url.rsplit('/').next() {
                    let cover_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("src/assets/docs-cover")
                        .join(cover_name);
                    let _ = fs::remove_file(cover_path).await;
                }
            }
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    Ok((StatusCode::CREATED, Json(ApiResponse::success(row))))
}

pub async fn create_dispatch(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    request: Request,
) -> Result<(StatusCode, Json<ApiResponse<DocListItem>>), StatusCode> {
    let is_json = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"));

    if !is_json {
        let multipart = Multipart::from_request(request, &state)
            .await
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        return create(State(state), Extension(claims), multipart).await;
    }

    let Json(input) = Json::<CreateDocInput>::from_request(request, &state)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let title = input.title.trim().to_string();
    let category = input.category.trim().to_string();
    if title.is_empty() || category.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if input.content.len() > MAX_MARKDOWN_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    let username = sqlx::query_scalar::<_, String>(r#"SELECT name FROM "user" WHERE id = $1"#)
        .bind(user_id(&claims)?)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let user_slug = slugify(&username);
    let slug = title_slugify(&title);
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/assets/docs")
        .join(&user_slug);
    fs::create_dir_all(&directory)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let markdown_path = directory.join(format!("{slug}.md"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&markdown_path)
        .await
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        })?;
    if let Err(error) = file.write_all(input.content.as_bytes()).await {
        let _ = fs::remove_file(&markdown_path).await;
        return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        });
    }

    let user_id = user_id(&claims)?;
    let result = sqlx::query_as::<_, DocListItem>(
        r#"INSERT INTO docs (user_id, title, category, content, markdown_path, cover_url)
           VALUES ($1, $2, $3, $4, $5, NULL)
           RETURNING id, title, category, TO_CHAR(updated_at, 'YYYY-MM-DD HH24:MI') AS date,
                     cover_url AS cover, 'violet' AS accent, content"#,
    )
    .bind(user_id)
    .bind(&title)
    .bind(&category)
    .bind(&input.content)
    .bind(markdown_path.to_string_lossy().to_string())
    .fetch_one(&state.db)
    .await;
    match result {
        Ok(row) => Ok((StatusCode::CREATED, Json(ApiResponse::success(row)))),
        Err(error) => {
            let _ = fs::remove_file(&markdown_path).await;
            tracing::error!(%error, "Failed to insert document");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DocContent {
    pub id: Uuid,
    pub title: String,
    pub category: String,
    pub markdown_stem: String,
    pub cover: Option<String>,
    pub content: String,
    pub public: bool,
}

pub async fn get(
    State(state): State<AppState>,
    claims: Option<Extension<Claims>>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<DocContent>>, StatusCode> {
    let user_id = claims
        .as_ref()
        .map(|Extension(claims)| user_id(claims))
        .transpose()?;
    let row = sqlx::query_as::<_, (Uuid, String, String, Option<String>, String, String, bool)>(
        r#"SELECT id, title, category, cover_url, markdown_path, content, public
           FROM docs
           WHERE id = $1 AND (public = TRUE OR ($2::uuid IS NOT NULL AND user_id = $2))"#,
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(ApiResponse::success(DocContent {
        id: row.0,
        title: row.1,
        category: row.2,
        markdown_stem: PathBuf::from(&row.4)
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string(),
        cover: row.3,
        content: row.5,
        public: row.6,
    })))
}

pub async fn update_info(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    mut multipart: Multipart,
) -> Result<Json<ApiResponse<DocContent>>, StatusCode> {
    let user_id = user_id(&claims)?;
    let mut title = None;
    let mut category = None;
    let mut cover = None;
    let mut public: Option<bool> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
    {
        let name = field.name().unwrap_or_default().to_string();
        if name == "cover" {
            let bytes = field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?;
            if bytes.len() > MAX_IMAGE_BYTES {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            cover = Some(bytes);
        } else {
            let value = String::from_utf8(
                field
                    .bytes()
                    .await
                    .map_err(|_| StatusCode::BAD_REQUEST)?
                    .to_vec(),
            )
            .map_err(|_| StatusCode::BAD_REQUEST)?;
            match name.as_str() {
                "title" => title = Some(value.trim().to_string()),
                "category" => category = Some(value.trim().to_string()),
                "public" => {
                    public = Some(value.parse::<bool>().map_err(|_| StatusCode::BAD_REQUEST)?)
                }
                _ => {}
            }
        }
    }
    let title = title
        .filter(|value| !value.is_empty())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let category = category
        .filter(|value| !value.is_empty())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let cover_url = if let Some(bytes) = cover {
        let decoded =
            image::load_from_memory(&bytes).map_err(|_| StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
        let resized = decoded.resize_to_fill(720, 480, FilterType::Lanczos3);
        let cover_name = format!(
            "{}-{}.webp",
            slugify(&username_for_doc(&state.db, user_id).await?),
            title_slugify(&title),
        );
        let cover_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/assets/docs-cover");
        fs::create_dir_all(&cover_dir)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let cover_path = cover_dir.join(&cover_name);
        let mut output = std::io::Cursor::new(Vec::new());
        resized
            .write_to(&mut output, ImageFormat::WebP)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        fs::write(&cover_path, output.into_inner())
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Some(format!("/api/assets/docs-cover/{cover_name}"))
    } else {
        None
    };
    let row = sqlx::query_as::<_, (Uuid, String, String, Option<String>, String, String, bool)>(
        r#"UPDATE docs SET title = $2, category = $3, cover_url = COALESCE($4, cover_url),
               public = COALESCE($5, public), updated_at = NOW()
           WHERE id = $1 AND user_id = $6
           RETURNING id, title, category, cover_url, markdown_path, content, public"#,
    )
    .bind(id)
    .bind(&title)
    .bind(&category)
    .bind(&cover_url)
    .bind(&public)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(ApiResponse::success(DocContent {
        id: row.0,
        title: row.1,
        category: row.2,
        markdown_stem: PathBuf::from(&row.4)
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string(),
        cover: row.3,
        content: row.5,
        public: row.6,
    })))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<()>>, StatusCode> {
    let user_id = user_id(&claims)?;
    let row = sqlx::query_as::<_, (String, Option<String>)>(
        "DELETE FROM docs WHERE id = $1 AND user_id = $2 RETURNING markdown_path, cover_url",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    let _ = fs::remove_file(row.0).await;
    if let Some(cover_url) = row.1 {
        if let Some(cover_name) = cover_url.rsplit('/').next() {
            let cover_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src/assets/docs-cover")
                .join(cover_name);
            let _ = fs::remove_file(cover_path).await;
        }
    }
    Ok(Json(ApiResponse::success(())))
}

async fn username_for_doc(db: &sqlx::PgPool, user_id: Uuid) -> Result<String, StatusCode> {
    sqlx::query_scalar::<_, String>(r#"SELECT name FROM "user" WHERE id = $1"#)
        .bind(user_id)
        .fetch_optional(db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)
}

#[derive(Debug, Deserialize)]
pub struct UpdateDocInput {
    pub content: String,
    #[serde(default)]
    pub image_renames: Vec<DocImageRename>,
}

#[derive(Debug, Deserialize)]
pub struct DocImageRename {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Serialize)]
pub struct DocImageUpload {
    pub path: String,
    pub url: String,
}

pub async fn upload_image(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<ApiResponse<DocImageUpload>>), StatusCode> {
    let user_id = user_id(&claims)?;
    let markdown_path = sqlx::query_scalar::<_, String>(
        "SELECT markdown_path FROM docs WHERE id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    let markdown_stem = PathBuf::from(&markdown_path)
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut image_bytes = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
    {
        if field.name() == Some("file") {
            let bytes = field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?;
            if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
                return Err(if bytes.len() > MAX_IMAGE_BYTES {
                    StatusCode::PAYLOAD_TOO_LARGE
                } else {
                    StatusCode::BAD_REQUEST
                });
            }
            image_bytes = Some(bytes);
            break;
        }
    }
    let image_bytes = image_bytes.ok_or(StatusCode::BAD_REQUEST)?;
    let thumbnail_bytes = tokio::task::spawn_blocking(move || {
        let is_standard_webp = image::guess_format(&image_bytes)
            .is_ok_and(|format| format == ImageFormat::WebP)
            && webp::Decoder::new(&image_bytes)
                .decode()
                .is_some_and(|image| {
                    image.width() == DOC_IMAGE_WIDTH && image.height() == DOC_IMAGE_HEIGHT
                });
        if is_standard_webp {
            return Ok::<Vec<u8>, StatusCode>(image_bytes.to_vec());
        }

        let decoded = image::load_from_memory(&image_bytes)
            .map_err(|_| StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
        let thumbnail = decoded
            .resize_to_fill(DOC_IMAGE_WIDTH, DOC_IMAGE_HEIGHT, FilterType::Triangle)
            .to_rgba8();
        let encoded =
            webp::Encoder::from_rgba(thumbnail.as_raw(), thumbnail.width(), thumbnail.height())
                .encode(78.0);
        Ok::<Vec<u8>, StatusCode>(encoded.to_vec())
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)??;

    let image_name = format!(
        "{}.webp",
        Uuid::new_v4().simple().to_string()[..6].to_string()
    );
    let image_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/assets/docs-img")
        .join(&markdown_stem);
    fs::create_dir_all(&image_dir)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let image_path = image_dir.join(&image_name);
    fs::write(&image_path, thumbnail_bytes)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let relative_path = format!("src/assets/docs-img/{markdown_stem}/{image_name}");
    let asset_url = format!("/api/assets/docs-img/{markdown_stem}/{image_name}");
    Ok((
        StatusCode::CREATED,
        Json(ApiResponse::success(DocImageUpload {
            path: relative_path,
            url: asset_url,
        })),
    ))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateDocInput>,
) -> Result<Json<ApiResponse<()>>, StatusCode> {
    if input.content.len() > MAX_MARKDOWN_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let user_id = user_id(&claims)?;
    let path = sqlx::query_scalar::<_, String>(
        "SELECT markdown_path FROM docs WHERE id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)
    .map(PathBuf::from)?;
    let old_content = fs::read_to_string(&path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let markdown_stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let image_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/assets/docs-img")
        .join(markdown_stem);
    let old_images = doc_image_paths(&old_content, markdown_stem);
    let new_images = doc_image_paths(&input.content, markdown_stem);
    let removed_images: Vec<String> = old_images
        .iter()
        .filter(|name| !new_images.contains(name))
        .filter(|name| image_name_is_valid(name))
        .cloned()
        .collect();
    let rename_pairs = image_rename_pairs(&old_images, &new_images, &input.image_renames);
    let mut renamed_images = Vec::new();
    for (old_name, new_name) in rename_pairs {
        if old_name == new_name || !image_name_is_valid(old_name) || !image_name_is_valid(new_name)
        {
            continue;
        }
        let old_path = image_dir.join(old_name);
        let new_path = image_dir.join(new_name);
        if fs::metadata(&old_path).await.is_err() {
            for (from, to) in renamed_images.iter().rev() {
                let _ = fs::rename(to, from).await;
            }
            return Err(StatusCode::NOT_FOUND);
        }
        if fs::metadata(&new_path).await.is_ok() {
            for (from, to) in renamed_images.iter().rev() {
                let _ = fs::rename(to, from).await;
            }
            return Err(StatusCode::CONFLICT);
        }
        if let Err(error) = fs::rename(&old_path, &new_path).await {
            for (from, to) in renamed_images.iter().rev() {
                let _ = fs::rename(to, from).await;
            }
            tracing::error!(%error, "Failed to rename document image");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        renamed_images.push((old_path, new_path));
    }
    let temp_path = path.with_extension(format!("md.{}.tmp", Uuid::new_v4()));
    fs::write(&temp_path, input.content.as_bytes())
        .await
        .map_err(|_| {
            for (from, to) in renamed_images.iter().rev() {
                let _ = std::fs::rename(to, from);
            }
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    if let Err(error) = fs::rename(&temp_path, &path).await {
        let _ = fs::remove_file(&temp_path).await;
        for (from, to) in renamed_images.iter().rev() {
            let _ = fs::rename(to, from).await;
        }
        tracing::error!(%error, "Failed to replace document content");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let update_result =
        sqlx::query("UPDATE docs SET content = $2, updated_at = NOW() WHERE id = $1")
            .bind(id)
            .bind(&input.content)
            .execute(&state.db)
            .await;
    if let Err(error) = update_result {
        tracing::error!(%error, "Failed to update document metadata");
        if let Err(restore_error) = fs::write(&path, old_content.as_bytes()).await {
            tracing::error!(%restore_error, "Failed to restore document content");
        }
        for (from, to) in renamed_images.iter().rev() {
            let _ = fs::rename(to, from).await;
        }
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    for image_name in removed_images {
        let image_path = image_dir.join(image_name);
        if let Err(error) = fs::remove_file(&image_path).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    %error,
                    path = %image_path.display(),
                    "Failed to remove unused document image"
                );
            }
        }
    }
    Ok(Json(ApiResponse::success(())))
}

fn image_rename_pairs<'a>(
    old_images: &'a [String],
    new_images: &'a [String],
    explicit_renames: &'a [DocImageRename],
) -> Vec<(&'a str, &'a str)> {
    if !explicit_renames.is_empty() {
        return explicit_renames
            .iter()
            .filter_map(|rename| {
                let from = old_images
                    .iter()
                    .find(|name| name.as_str() == rename.from)?;
                let to = new_images.iter().find(|name| name.as_str() == rename.to)?;
                Some((from.as_str(), to.as_str()))
            })
            .collect();
    }

    let removed: Vec<&str> = old_images
        .iter()
        .filter(|name| !new_images.contains(name))
        .map(String::as_str)
        .collect();
    let added: Vec<&str> = new_images
        .iter()
        .filter(|name| !old_images.contains(name))
        .map(String::as_str)
        .collect();
    if removed.len() == 1 && added.len() == 1 {
        vec![(removed[0], added[0])]
    } else {
        Vec::new()
    }
}

fn doc_image_paths(markdown: &str, markdown_stem: &str) -> Vec<String> {
    let prefix = format!("src/assets/docs-img/{markdown_stem}/");
    markdown
        .split("![")
        .skip(1)
        .filter_map(|part| part.split("](").nth(1))
        .filter_map(|part| part.split(')').next())
        .filter_map(|path| path.strip_prefix(&prefix))
        .map(str::to_string)
        .collect()
}

fn image_name_is_valid(name: &str) -> bool {
    name.ends_with(".webp")
        && name.len() > 5
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || character == '.'
                || character == '_'
                || character == '-'
        })
        && !name.contains("..")
}

fn user_id(claims: &Claims) -> Result<Uuid, StatusCode> {
    claims.sub.parse().map_err(|_| StatusCode::UNAUTHORIZED)
}

fn slugify(value: &str) -> String {
    let slug = value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                if character.is_ascii() {
                    character.to_ascii_lowercase()
                } else {
                    character
                }
            } else {
                '-'
            }
        })
        .collect::<String>();
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        format!("document-{}", timestamp())
    } else {
        slug.chars().take(80).collect()
    }
}

fn title_slugify(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len() * 2);

    for character in value.chars() {
        match character {
            '+' => normalized.push_str(" plus "),
            '#' => normalized.push_str(" sharp "),
            '&' => normalized.push_str(" and "),
            _ => normalized.push(character),
        }
    }

    slugify(&normalized)
}

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{doc_image_paths, image_rename_pairs};

    #[test]
    fn extracts_only_document_image_references() {
        let markdown = concat!(
            "![cover](src/assets/docs-img/guide/first.webp)\n",
            "[download](src/assets/docs-img/guide/linked.webp)\n",
            "![other](src/assets/docs-img/other/ignored.webp)"
        );

        assert_eq!(
            doc_image_paths(markdown, "guide"),
            vec!["first.webp".to_string()]
        );
    }

    #[test]
    fn infers_only_unambiguous_single_image_rename() {
        let old_images = vec!["first.webp".to_string(), "kept.webp".to_string()];
        let new_images = vec!["renamed.webp".to_string(), "kept.webp".to_string()];

        assert_eq!(
            image_rename_pairs(&old_images, &new_images, &[]),
            vec![("first.webp", "renamed.webp")]
        );
    }

    #[test]
    fn does_not_infer_ambiguous_multiple_image_changes() {
        let old_images = vec!["first.webp".to_string(), "second.webp".to_string()];
        let new_images = vec!["third.webp".to_string(), "fourth.webp".to_string()];

        assert!(image_rename_pairs(&old_images, &new_images, &[]).is_empty());
    }
}
