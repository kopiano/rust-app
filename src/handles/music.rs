use std::{
    collections::HashMap,
    io::Cursor,
    path::{Path, PathBuf},
    process::Stdio,
};

use axum::{
    Extension, Json,
    extract::{Multipart, Path as AxumPath, State, multipart::Field},
    http::StatusCode,
};
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgb};
use serde::Deserialize;
use tokio::{io::AsyncWriteExt, process::Command, sync::Semaphore};
use uuid::Uuid;

use crate::{
    app::AppState,
    common::response::ApiResponse,
    middleware::jwt::Claims,
    models::music::{Music, MusicFavoriteState, UpdateMusicFavorite},
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

struct PreparedMusic {
    id: Uuid,
    directory: PathBuf,
    title: String,
    artist: String,
    album: String,
    duration_ms: i64,
    bitrate: i32,
    sample_rate: i32,
    cover_url: String,
    audio_url: String,
    original_url: String,
    original_format: String,
    size: i64,
    original_size: i64,
}

pub async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<ApiResponse<Vec<Music>>>, StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    let music = sqlx::query_as::<_, Music>(
        r#"
        SELECT id, title, artist, album, duration_ms, bitrate, sample_rate,
               cover_url, audio_url, original_url, format, original_format,
               size, original_size, is_favorite, created_at, updated_at
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

pub async fn upload(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<ApiResponse<Vec<Music>>>), StatusCode> {
    let user_id = authenticated_user_id(&claims)?;
    let mut prepared_music = Vec::new();
    let mut found_file = false;

    loop {
        let next_field = match multipart.next_field().await {
            Ok(field) => field,
            Err(_) => {
                cleanup_prepared_music(&prepared_music).await;
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

        match prepare_music(&mut field).await {
            Ok(prepared) => prepared_music.push(prepared),
            Err(status) => {
                cleanup_prepared_music(&prepared_music).await;
                return Err(status);
            }
        }
    }

    if !found_file || prepared_music.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut transaction = match state.db.begin().await {
        Ok(transaction) => transaction,
        Err(error) => {
            tracing::error!(%error, %user_id, "Failed to begin music upload transaction");
            cleanup_prepared_music(&prepared_music).await;
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let mut created = Vec::with_capacity(prepared_music.len());
    for prepared in &prepared_music {
        let inserted = insert_music(&mut transaction, user_id, prepared).await;
        match inserted {
            Ok(music) => created.push(music),
            Err(status) => {
                let _ = transaction.rollback().await;
                cleanup_prepared_music(&prepared_music).await;
                return Err(status);
            }
        }
    }
    if let Err(error) = transaction.commit().await {
        tracing::error!(%error, %user_id, "Failed to commit music upload transaction");
        cleanup_prepared_music(&prepared_music).await;
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
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

fn authenticated_user_id(claims: &Claims) -> Result<Uuid, StatusCode> {
    claims
        .sub
        .parse::<Uuid>()
        .map_err(|_| StatusCode::UNAUTHORIZED)
}

async fn prepare_music(field: &mut Field<'_>) -> Result<PreparedMusic, StatusCode> {
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

    let result = prepare_music_files(
        field,
        &original_name,
        &original_format,
        id,
        directory.clone(),
    )
    .await;
    if result.is_err() {
        cleanup_directory(&directory).await;
    }
    result
}

async fn prepare_music_files(
    field: &mut Field<'_>,
    original_name: &str,
    original_format: &str,
    id: Uuid,
    directory: PathBuf,
) -> Result<PreparedMusic, StatusCode> {
    let original_path = directory.join(format!("original.{original_format}"));
    let original_size = save_audio_field(field, &original_path).await?;
    let original_probe = probe_audio(&original_path).await?;
    let fallback_title = filename_title(original_name);
    let original_metadata = metadata_from_probe(&original_probe, &fallback_title);

    let audio_path = directory.join("audio.m4a");
    transcode_to_aac(&original_path, &audio_path).await?;
    let output_probe = probe_audio(&audio_path).await?;
    let output_metadata = metadata_from_probe(&output_probe, &original_metadata.title);
    let output_size = file_size(&audio_path).await?;

    let cover_path = directory.join("cover.webp");
    create_cover(&original_path, &cover_path, id).await?;

    let base_url = format!("/api/assets/music/{id}");
    Ok(PreparedMusic {
        id,
        directory,
        title: preferred_text(&original_metadata.title, &fallback_title),
        artist: preferred_text(&original_metadata.artist, "Unknown Artist"),
        album: preferred_text(&original_metadata.album, "Unknown Album"),
        duration_ms: output_metadata
            .duration_ms
            .max(original_metadata.duration_ms),
        bitrate: output_metadata.bitrate,
        sample_rate: output_metadata.sample_rate,
        cover_url: format!("{base_url}/cover.webp"),
        audio_url: format!("{base_url}/audio.m4a"),
        original_url: format!("{base_url}/original.{original_format}"),
        original_format: original_format.to_owned(),
        size: output_size,
        original_size,
    })
}

async fn insert_music(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    music: &PreparedMusic,
) -> Result<Music, StatusCode> {
    sqlx::query_as::<_, Music>(
        r#"
        INSERT INTO music (
            id, user_id, title, artist, album, duration_ms, bitrate,
            sample_rate, cover_url, audio_url, original_url, format,
            original_format, size, original_size
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
            'm4a', $12, $13, $14
        )
        RETURNING id, title, artist, album, duration_ms, bitrate, sample_rate,
                  cover_url, audio_url, original_url, format, original_format,
                  size, original_size, is_favorite, created_at, updated_at
        "#,
    )
    .bind(music.id)
    .bind(user_id)
    .bind(&music.title)
    .bind(&music.artist)
    .bind(&music.album)
    .bind(music.duration_ms)
    .bind(music.bitrate)
    .bind(music.sample_rate)
    .bind(&music.cover_url)
    .bind(&music.audio_url)
    .bind(&music.original_url)
    .bind(&music.original_format)
    .bind(music.size)
    .bind(music.original_size)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| {
        tracing::error!(%error, %user_id, music_id = %music.id, "Failed to save music metadata");
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

async fn create_cover(source: &Path, output: &Path, id: Uuid) -> Result<(), StatusCode> {
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
        .output()
        .await;

    let extracted_bytes = extracted
        .ok()
        .filter(|result| result.status.success() && !result.stdout.is_empty())
        .map(|result| result.stdout);
    let id_bytes = *id.as_bytes();
    let encoded =
        tokio::task::spawn_blocking(move || encode_cover(extracted_bytes.as_deref(), id_bytes))
            .await
            .map_err(|error| {
                tracing::error!(%error, "Cover encoding task failed");
                StatusCode::INTERNAL_SERVER_ERROR
            })?
            .map_err(|error| {
                tracing::error!(%error, "Failed to encode WebP cover");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    tokio::fs::write(output, encoded).await.map_err(|error| {
        tracing::error!(%error, path = %output.display(), "Failed to save WebP cover");
        StatusCode::INTERNAL_SERVER_ERROR
    })
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

async fn cleanup_prepared_music(music: &[PreparedMusic]) {
    for item in music {
        cleanup_directory(&item.directory).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ProbeFormat, ProbeOutput, ProbeStream, audio_extension, create_cover, encode_cover,
        filename_title, metadata_from_probe, probe_audio, transcode_to_aac,
    };
    use image::ImageFormat;
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
        create_cover(&source, &cover, Uuid::new_v4()).await.unwrap();
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
}
