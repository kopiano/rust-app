use axum::{
    Extension, Json,
    extract::{Multipart, Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use image::{ImageFormat, imageops::FilterType};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Cursor, path::Path as FilePath};
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

#[derive(Serialize, sqlx::FromRow)]
pub struct StoreCartItem {
    id: Uuid,
    product_id: Uuid,
    size: String,
    temperature: String,
    sweetness: String,
    quantity: i32,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct StoreOrder {
    id: Uuid,
    status: String,
    total: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct StoreOrderItem {
    id: Uuid,
    product_id: Uuid,
    product_name: String,
    size: String,
    temperature: String,
    sweetness: String,
    quantity: i32,
    unit_price: i32,
    line_total: i64,
}

#[derive(Serialize)]
pub struct StoreOrderDetail {
    #[serde(flatten)]
    order: StoreOrder,
    items: Vec<StoreOrderItem>,
}

#[derive(Deserialize)]
pub struct CreateOrderInput {
    idempotency_key: Uuid,
}

#[derive(serde::Deserialize)]
pub struct CartItemInput {
    product_id: Uuid,
    size: String,
    temperature: String,
    sweetness: String,
    quantity: i32,
}

fn valid_cart_input(input: &CartItemInput) -> bool {
    matches!(input.size.as_str(), "small" | "medium" | "large")
        && matches!(input.temperature.as_str(), "iced" | "hot" | "room")
        && matches!(
            input.sweetness.as_str(),
            "standard" | "less" | "extra" | "none"
        )
        && (1..=99).contains(&input.quantity)
}

pub async fn cart_list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<ApiResponse<Vec<StoreCartItem>>>, StatusCode> {
    let user_id: Uuid = claims.sub.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
    let items = sqlx::query_as::<_, StoreCartItem>(
        "SELECT id, product_id, size, temperature, sweetness, quantity
         FROM store_cart WHERE user_id = $1 ORDER BY created_at ASC, id ASC",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ApiResponse::success(items)))
}

pub async fn cart_upsert(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(input): Json<CartItemInput>,
) -> Result<Json<ApiResponse<StoreCartItem>>, StatusCode> {
    let user_id: Uuid = claims.sub.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
    if !valid_cart_input(&input) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let item = sqlx::query_as::<_, StoreCartItem>(
        "INSERT INTO store_cart (id, user_id, product_id, size, temperature, sweetness, quantity)
         SELECT $1, $2, $3, $4, $5, $6, $7
         WHERE EXISTS (SELECT 1 FROM store WHERE id = $3)
         ON CONFLICT (user_id, product_id, size, temperature, sweetness)
         DO UPDATE SET quantity = LEAST(99, store_cart.quantity + EXCLUDED.quantity), updated_at = NOW()
         RETURNING id, product_id, size, temperature, sweetness, quantity",
    ).bind(Uuid::new_v4()).bind(user_id).bind(input.product_id).bind(input.size)
        .bind(input.temperature).bind(input.sweetness).bind(input.quantity)
        .fetch_optional(&state.db).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(ApiResponse::success(item)))
}

pub async fn cart_update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(input): Json<serde_json::Value>,
) -> Result<Json<ApiResponse<StoreCartItem>>, StatusCode> {
    let user_id: Uuid = claims.sub.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
    let quantity = input
        .get("quantity")
        .and_then(|v| v.as_i64())
        .ok_or(StatusCode::BAD_REQUEST)?;
    if !(1..=99).contains(&quantity) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let item = sqlx::query_as::<_, StoreCartItem>(
        "UPDATE store_cart SET quantity = $1, updated_at = NOW()
         WHERE id = $2 AND user_id = $3
         RETURNING id, product_id, size, temperature, sweetness, quantity",
    )
    .bind(quantity as i32)
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(ApiResponse::success(item)))
}

pub async fn cart_delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiResponse<()>>, StatusCode> {
    let user_id: Uuid = claims.sub.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
    sqlx::query("DELETE FROM store_cart WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ApiResponse::success(())))
}

pub async fn create_order(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(input): Json<CreateOrderInput>,
) -> Result<(StatusCode, Json<ApiResponse<StoreOrder>>), StatusCode> {
    let user_id: Uuid = claims.sub.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if let Some(order) = sqlx::query_as::<_, StoreOrder>(
        "SELECT id, status, total, created_at, updated_at
         FROM store_order WHERE user_id = $1 AND idempotency_key = $2",
    )
    .bind(user_id)
    .bind(input.idempotency_key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        tx.commit()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        return Ok((StatusCode::OK, Json(ApiResponse::success(order))));
    }

    let cart = sqlx::query_as::<_, (Uuid, String, String, String, String, i32, i32)>(
        "SELECT c.product_id, s.name, c.size, c.temperature, c.sweetness, c.quantity, s.price
         FROM store_cart c
         JOIN store s ON s.id = c.product_id
         WHERE c.user_id = $1
         ORDER BY c.created_at ASC, c.id ASC
         FOR UPDATE OF c",
    )
    .bind(user_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if cart.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let order_id = Uuid::new_v4();
    let mut total = 0_i64;
    for (_, _, size, _, _, quantity, unit_price) in &cart {
        let extra = match size.as_str() {
            "medium" => 200_i64,
            "large" => 400_i64,
            _ => 0_i64,
        };
        total = total
            .checked_add((i64::from(*unit_price) + extra) * i64::from(*quantity))
            .ok_or(StatusCode::BAD_REQUEST)?;
    }

    sqlx::query(
        "INSERT INTO store_order (id, user_id, idempotency_key, status, total)
         VALUES ($1, $2, $3, 'pending', $4)",
    )
    .bind(order_id)
    .bind(user_id)
    .bind(input.idempotency_key)
    .bind(total)
    .execute(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    for (product_id, name, size, temperature, sweetness, quantity, base_price) in cart {
        let extra = match size.as_str() {
            "medium" => 200_i32,
            "large" => 400_i32,
            _ => 0_i32,
        };
        let unit_price = base_price + extra;
        sqlx::query(
            "INSERT INTO store_order_item
             (id, order_id, product_id, product_name, size, temperature, sweetness, quantity, unit_price)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(Uuid::new_v4())
        .bind(order_id)
        .bind(product_id)
        .bind(name)
        .bind(size)
        .bind(temperature)
        .bind(sweetness)
        .bind(quantity)
        .bind(unit_price)
        .execute(&mut *tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }

    sqlx::query(
        "INSERT INTO store_order_outbox (event_id, order_id, event_type, payload)
         VALUES ($1, $2, 'store.order.created', jsonb_build_object('order_id', $2::text))",
    )
    .bind(Uuid::new_v4())
    .bind(order_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    sqlx::query("DELETE FROM store_cart WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    tx.commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state.order_outbox_notify.notify_one();

    let order = sqlx::query_as::<_, StoreOrder>(
        "SELECT id, status, total, created_at, updated_at FROM store_order WHERE id = $1",
    )
    .bind(order_id)
    .fetch_one(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(ApiResponse::success(order))))
}

pub async fn orders(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<ApiResponse<Vec<StoreOrder>>>, StatusCode> {
    let user_id: Uuid = claims.sub.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
    let orders = sqlx::query_as::<_, StoreOrder>(
        "SELECT id, status, total, created_at, updated_at
         FROM store_order WHERE user_id = $1 ORDER BY created_at DESC LIMIT 50",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ApiResponse::success(orders)))
}

pub async fn order_detail(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(order_id): Path<Uuid>,
) -> Result<Json<ApiResponse<StoreOrderDetail>>, StatusCode> {
    let user_id: Uuid = claims.sub.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
    let order = sqlx::query_as::<_, StoreOrder>(
        "SELECT id, status, total, created_at, updated_at
         FROM store_order WHERE id = $1 AND user_id = $2",
    )
    .bind(order_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    let items = sqlx::query_as::<_, StoreOrderItem>(
        "SELECT id, product_id, product_name, size, temperature, sweetness, quantity,
                unit_price, unit_price::bigint * quantity AS line_total
         FROM store_order_item WHERE order_id = $1 ORDER BY id",
    )
    .bind(order_id)
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ApiResponse::success(StoreOrderDetail {
        order,
        items,
    })))
}

pub async fn list(
    State(state): State<AppState>,
) -> Result<Json<ApiResponse<Vec<StoreProduct>>>, StatusCode> {
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
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
    {
        let field_name = field.name().unwrap_or_default().to_owned();
        if field_name == "image" {
            let bytes = field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?;
            if bytes.len() > MAX_IMAGE_BYTES {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
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
    if name.is_empty()
        || name.chars().count() > 80
        || !matches!(category.as_str(), "coffee" | "tea" | "bakery")
        || !(1..=9_999_900).contains(&price)
        || !(0..=99_999_999).contains(&sales)
        || bytes.is_empty()
        || image::load_from_memory(&bytes).is_err()
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    let encoded = tokio::task::spawn_blocking(move || encode_store_image(&bytes))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(|_| StatusCode::UNSUPPORTED_MEDIA_TYPE)?;

    let id = Uuid::new_v4();
    let image_filename = store_image_filename(&encoded);
    let image_url = format!("/api/assets/store/{image_filename}");
    let path = FilePath::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/assets/store")
        .join(&image_filename);
    tokio::fs::create_dir_all(path.parent().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    tokio::fs::write(&path, &encoded)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let result = sqlx::query_as::<_, StoreProduct>(
        "INSERT INTO store (id, user_id, name, category, price, sales, image_url) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         RETURNING id, name, category, price, sales, image_url",
    )
    .bind(id)
    .bind(user_id)
    .bind(name)
    .bind(category)
    .bind(price)
    .bind(sales)
    .bind(image_url)
    .fetch_one(&state.db)
    .await;
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
        assert_eq!(
            image::load_from_memory(&webp).unwrap().dimensions(),
            (1600, 1067)
        );
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
        assert!(
            filename[..8]
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
    }
}
