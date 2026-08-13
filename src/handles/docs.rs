use axum::{
    Extension, Json,
    extract::{FromRequest, Multipart, Path, Query, Request, State},
    http::StatusCode,
};
use image::{ImageFormat, imageops::FilterType};
use serde::Serialize;
use serde::Deserialize;
use std::{path::PathBuf, time::SystemTime};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::{app::AppState, common::response::ApiResponse, middleware::jwt::Claims};

const MAX_MARKDOWN_BYTES: usize = 10 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

pub async fn backfill_content(db: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    let rows = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT id, markdown_path, content FROM docs",
    )
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
    pub image: Option<String>,
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
                      image_url AS image, 'violet' AS accent, content, public
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
                  image_url AS image, 'violet' AS accent, content, public
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
    let mut image = None;
    while let Some(field) = multipart.next_field().await.map_err(|_| StatusCode::BAD_REQUEST)? {
        let name = field.name().unwrap_or_default().to_string();
        if name == "image" {
            let bytes = field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?;
            if bytes.len() > MAX_IMAGE_BYTES {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            image = Some(bytes);
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

    let title = title.map(|value| value.trim().to_string()).filter(|value| !value.is_empty()).ok_or(StatusCode::BAD_REQUEST)?;
    let content = content.unwrap_or_default();
    let category = category.map(|value| value.trim().to_string()).filter(|value| !value.is_empty()).ok_or(StatusCode::BAD_REQUEST)?;
    let slug = title_slugify(&title);
    let username_slug = slugify(&username);
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/assets/docs").join(&username_slug);
    fs::create_dir_all(&directory).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let markdown_path = directory.join(format!("{slug}.md"));

    let image_url = if let Some(bytes) = image {
        let decoded = image::load_from_memory(&bytes).map_err(|_| StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
        let resized = decoded.resize_to_fill(720, 480, FilterType::Lanczos3);
        let image_name = format!("{username_slug}-{slug}.webp");
        let image_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/assets/docs-image");
        fs::create_dir_all(&image_dir).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let image_path = image_dir.join(&image_name);
        let mut output = std::io::Cursor::new(Vec::new());
        resized.write_to(&mut output, ImageFormat::WebP).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut image_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&image_path)
            .await
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            })?;
        if let Err(error) = image_file.write_all(&output.into_inner()).await {
            let _ = fs::remove_file(&image_path).await;
            return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            });
        }
        Some(format!("/api/assets/docs-image/{image_name}"))
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
            if let Some(image_url) = &image_url {
                if let Some(image_name) = image_url.rsplit('/').next() {
                    let image_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("src/assets/docs-image")
                        .join(image_name);
                    let _ = fs::remove_file(image_path).await;
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
        if let Some(image_url) = &image_url {
            if let Some(image_name) = image_url.rsplit('/').next() {
                let image_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("src/assets/docs-image")
                    .join(image_name);
                let _ = fs::remove_file(image_path).await;
            }
        }
        return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        });
    }

    let row = sqlx::query_as::<_, DocListItem>(
        r#"INSERT INTO docs (user_id, title, category, content, markdown_path, image_url)
           VALUES ($1, $2, $3, $4, $5, $6)
           RETURNING id, title, category, TO_CHAR(updated_at, 'YYYY-MM-DD HH24:MI') AS date,
                     image_url AS image, 'violet' AS accent, content, public"#,
    )
    .bind(user_id)
    .bind(&title)
    .bind(&category)
    .bind(&content)
    .bind(markdown_path.to_string_lossy().to_string())
    .bind(&image_url)
    .fetch_one(&state.db)
    .await;
    let row = match row {
        Ok(row) => row,
        Err(error) => {
            tracing::error!(%error, "Failed to insert document");
            let _ = fs::remove_file(&markdown_path).await;
            if let Some(image_url) = &image_url {
                if let Some(image_name) = image_url.rsplit('/').next() {
                    let image_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("src/assets/docs-image")
                        .join(image_name);
                    let _ = fs::remove_file(image_path).await;
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
        r#"INSERT INTO docs (user_id, title, category, content, markdown_path, image_url)
           VALUES ($1, $2, $3, $4, $5, NULL)
           RETURNING id, title, category, TO_CHAR(updated_at, 'YYYY-MM-DD HH24:MI') AS date,
                     image_url AS image, 'violet' AS accent, content"#,
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
    pub image: Option<String>,
    pub content: String,
    pub public: bool,
}

pub async fn get(
    State(state): State<AppState>,
    claims: Option<Extension<Claims>>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<DocContent>>, StatusCode> {
    let user_id = claims.as_ref().map(|Extension(claims)| user_id(claims)).transpose()?;
    let row = sqlx::query_as::<_, (Uuid, String, String, Option<String>, String, String, bool)>(
        r#"SELECT id, title, category, image_url, markdown_path, content, public
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
        image: row.3,
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
    let mut image = None;
    let mut public: Option<bool> = None;
    while let Some(field) = multipart.next_field().await.map_err(|_| StatusCode::BAD_REQUEST)? {
        let name = field.name().unwrap_or_default().to_string();
        if name == "image" {
            let bytes = field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?;
            if bytes.len() > MAX_IMAGE_BYTES {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            image = Some(bytes);
        } else {
            let value = String::from_utf8(field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?.to_vec())
                .map_err(|_| StatusCode::BAD_REQUEST)?;
            match name.as_str() {
                "title" => title = Some(value.trim().to_string()),
                "category" => category = Some(value.trim().to_string()),
                "public" => public = Some(value.parse::<bool>().map_err(|_| StatusCode::BAD_REQUEST)?),
                _ => {}
            }
        }
    }
    let title = title.filter(|value| !value.is_empty()).ok_or(StatusCode::BAD_REQUEST)?;
    let category = category.filter(|value| !value.is_empty()).ok_or(StatusCode::BAD_REQUEST)?;
    let image_url = if let Some(bytes) = image {
        let decoded = image::load_from_memory(&bytes).map_err(|_| StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
        let resized = decoded.resize_to_fill(720, 480, FilterType::Lanczos3);
        let image_name = format!(
            "{}-{}.webp",
            slugify(&username_for_doc(&state.db, user_id).await?),
            title_slugify(&title),
        );
        let image_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/assets/docs-image");
        fs::create_dir_all(&image_dir).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let image_path = image_dir.join(&image_name);
        let mut output = std::io::Cursor::new(Vec::new());
        resized.write_to(&mut output, ImageFormat::WebP).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        fs::write(&image_path, output.into_inner()).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Some(format!("/api/assets/docs-image/{image_name}"))
    } else {
        None
    };
    let row = sqlx::query_as::<_, (Uuid, String, String, Option<String>, String, String, bool)>(
        r#"UPDATE docs SET title = $2, category = $3, image_url = COALESCE($4, image_url),
               public = COALESCE($5, public), updated_at = NOW()
           WHERE id = $1 AND user_id = $6
           RETURNING id, title, category, image_url, markdown_path, content, public"#,
    )
    .bind(id).bind(&title).bind(&category).bind(&image_url).bind(&public).bind(user_id)
    .fetch_optional(&state.db).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(ApiResponse::success(DocContent { id: row.0, title: row.1, category: row.2, image: row.3, content: row.5, public: row.6 })))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<()>>, StatusCode> {
    let user_id = user_id(&claims)?;
    let row = sqlx::query_as::<_, (String, Option<String>)>(
        "DELETE FROM docs WHERE id = $1 AND user_id = $2 RETURNING markdown_path, image_url",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    let _ = fs::remove_file(row.0).await;
    if let Some(image_url) = row.1 {
        if let Some(image_name) = image_url.rsplit('/').next() {
            let image_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src/assets/docs-image")
                .join(image_name);
            let _ = fs::remove_file(image_path).await;
        }
    }
    Ok(Json(ApiResponse::success(())))
}

async fn username_for_doc(db: &sqlx::PgPool, user_id: Uuid) -> Result<String, StatusCode> {
    sqlx::query_scalar::<_, String>(r#"SELECT name FROM "user" WHERE id = $1"#)
        .bind(user_id).fetch_optional(db).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)
}

#[derive(Debug, Deserialize)]
pub struct UpdateDocInput {
    pub content: String,
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
    let old_content = fs::read(&path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let temp_path = path.with_extension(format!("md.{}.tmp", Uuid::new_v4()));
    fs::write(&temp_path, input.content.as_bytes())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if let Err(error) = fs::rename(&temp_path, &path).await {
        let _ = fs::remove_file(&temp_path).await;
        tracing::error!(%error, "Failed to replace document content");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let update_result = sqlx::query("UPDATE docs SET content = $2, updated_at = NOW() WHERE id = $1")
        .bind(id)
        .bind(&input.content)
        .execute(&state.db)
        .await;
    if let Err(error) = update_result {
        tracing::error!(%error, "Failed to update document metadata");
        if let Err(restore_error) = fs::write(&path, old_content).await {
            tracing::error!(%restore_error, "Failed to restore document content");
        }
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    Ok(Json(ApiResponse::success(())))
}

fn user_id(claims: &Claims) -> Result<Uuid, StatusCode> {
    claims.sub.parse().map_err(|_| StatusCode::UNAUTHORIZED)
}

fn slugify(value: &str) -> String {
    let slug = value.chars().map(|character| {
        if character.is_alphanumeric() {
            if character.is_ascii() { character.to_ascii_lowercase() } else { character }
        } else {
            '-'
        }
    }).collect::<String>();
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
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|duration| duration.as_secs()).unwrap_or_default()
}
