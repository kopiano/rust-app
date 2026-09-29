use axum::{Extension, Json, extract::{Multipart, State}, http::StatusCode};
use image::{ImageFormat, imageops::FilterType};
use sha2::{Digest, Sha256};
use std::{io::Cursor, path::Path};
use serde::Serialize;
use uuid::Uuid;

use crate::{app::AppState, common::response::ApiResponse, middleware::jwt::Claims};

const MAX_IMAGE_BYTES: usize = 12 * 1024 * 1024;

#[derive(Serialize, sqlx::FromRow)]
pub struct StoreProduct {
    id: Uuid,
    name: String,
    category: String,
    price: i32,
    sales: i32,
    image_url: String,
}

pub async fn list(State(state): State<AppState>) -> Result<Json<ApiResponse<Vec<StoreProduct>>>, StatusCode> {
    let products = sqlx::query_as::<_, StoreProduct>(
        "SELECT id, name, category, price, sales, image_url FROM store ORDER BY created_at ASC, id ASC",
    )
    .fetch_all(&state.db).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ApiResponse::success(products)))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<ApiResponse<StoreProduct>>), StatusCode> {
    let user_id: Uuid = claims.sub.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
    let mut name = None;
    let mut category = None;
    let mut price = None;
    let mut sales = None;
    let mut image = None;
    while let Some(field) = multipart.next_field().await.map_err(|_| StatusCode::BAD_REQUEST)? {
        let field_name = field.name().unwrap_or_default().to_owned();
        if field_name == "image" {
            let bytes = field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?;
            if bytes.len() > MAX_IMAGE_BYTES { return Err(StatusCode::PAYLOAD_TOO_LARGE); }
            image = Some(bytes);
        } else {
            let value = field.text().await.map_err(|_| StatusCode::BAD_REQUEST)?;
            match field_name.as_str() {
                "name" => name = Some(value),
                "category" => category = Some(value),
                "price" => price = value.parse::<i32>().ok(),
                "sales" => sales = value.parse::<i32>().ok(),
                _ => {}
            }
        }
    }
    let name = name.ok_or(StatusCode::BAD_REQUEST)?.trim().to_owned();
    let category = category.ok_or(StatusCode::BAD_REQUEST)?;
    let price = price.ok_or(StatusCode::BAD_REQUEST)?;
    let sales = sales.unwrap_or(0);
    let bytes = image.ok_or(StatusCode::BAD_REQUEST)?;
    if name.is_empty() || name.chars().count() > 80
        || !matches!(category.as_str(), "coffee" | "tea" | "bakery")
        || !(1..=9_999_900).contains(&price) || !(0..=99_999_999).contains(&sales)
        || bytes.is_empty() || image::load_from_memory(&bytes).is_err()
    { return Err(StatusCode::BAD_REQUEST); }

    let encoded = tokio::task::spawn_blocking(move || encode_store_image(&bytes))
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| StatusCode::UNSUPPORTED_MEDIA_TYPE)?;

    let id = Uuid::new_v4();
    let image_filename = store_image_filename(&encoded);
    let image_url = format!("/api/assets/store/{image_filename}");
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/assets/store").join(&image_filename);
    tokio::fs::create_dir_all(path.parent().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    tokio::fs::write(&path, &encoded).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let result = sqlx::query_as::<_, StoreProduct>(
        "INSERT INTO store (id, user_id, name, category, price, sales, image_url) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         RETURNING id, name, category, price, sales, image_url",
    )
    .bind(id).bind(user_id).bind(name).bind(category).bind(price).bind(sales).bind(image_url)
    .fetch_one(&state.db).await;
    match result {
        Ok(product) => Ok((StatusCode::CREATED, Json(ApiResponse::success(product)))),
        Err(error) => {
            tracing::error!(%error, "Store product insertion failed");
            if let Err(cleanup_error) = tokio::fs::remove_file(&path).await {
                tracing::warn!(%cleanup_error, "Store image cleanup failed");
            }
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

fn encode_store_image(bytes: &[u8]) -> image::ImageResult<Vec<u8>> {
    let image = image::load_from_memory(bytes)?;
    let resized = image.resize(1600, 1600, FilterType::Lanczos3);
    let mut output = Cursor::new(Vec::new());
    resized.write_to(&mut output, ImageFormat::WebP)?;
    Ok(output.into_inner())
}

fn store_image_filename(encoded: &[u8]) -> String {
    let digest = Sha256::digest(encoded);
    format!("{:x}.webp", digest)[..8].to_owned() + ".webp"
}

#[cfg(test)]
mod tests {
    use super::{encode_store_image, store_image_filename};
    use image::{DynamicImage, GenericImageView, ImageFormat, RgbImage};
    use std::io::Cursor;

    #[test]
    fn converts_png_upload_to_resized_webp() {
        let source = DynamicImage::ImageRgb8(RgbImage::new(1800, 1200));
        let mut png = Cursor::new(Vec::new());
        source.write_to(&mut png, ImageFormat::Png).unwrap();

        let webp = encode_store_image(&png.into_inner()).unwrap();
        assert_eq!(image::guess_format(&webp).unwrap(), ImageFormat::WebP);
        assert_eq!(image::load_from_memory(&webp).unwrap().dimensions(), (1600, 1067));
    }

    #[test]
    fn rejects_non_image_upload() {
        assert!(encode_store_image(b"not an image").is_err());
    }

    #[test]
    fn names_store_images_with_eight_hash_characters() {
        let filename = store_image_filename(b"encoded webp");
        assert_eq!(filename.len(), 13);
        assert!(filename.ends_with(".webp"));
        assert!(filename[..8].chars().all(|character| character.is_ascii_hexdigit()));
    }
}
