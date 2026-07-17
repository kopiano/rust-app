use std::{
    error::Error,
    fmt,
    fs::File,
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

use ncm2mp3_core::{
    AudioFormat, CoverMime,
    crypto::NcmStreamCipher,
    format::is_ncm_magic,
    parser::{read_and_verify_magic, read_metadata, read_rc4_key},
};
use tokio::io::AsyncReadExt;

const NCM_MAGIC_BYTES: usize = 8;
const STREAM_CHUNK_BYTES: usize = 32 * 1024;
const FORMAT_PROBE_BYTES: usize = 16;
const MAX_COVER_BYTES: u32 = 64 * 1024 * 1024;

#[derive(Debug)]
pub struct DecryptedNcm {
    pub audio_path: PathBuf,
    pub cover_path: Option<PathBuf>,
    pub title: String,
    pub artist: String,
    pub album: String,
}

impl Drop for DecryptedNcm {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.audio_path);
        if let Some(path) = &self.cover_path {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[derive(Debug)]
pub struct NcmDecryptError(String);

impl fmt::Display for NcmDecryptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for NcmDecryptError {}

pub async fn is_ncm_file(path: &Path) -> std::io::Result<bool> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut magic = [0_u8; NCM_MAGIC_BYTES];
    match file.read_exact(&mut magic).await {
        Ok(_) => Ok(is_ncm_magic(&magic)),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error),
    }
}

pub async fn decrypt_ncm(
    source: &Path,
    output_directory: &Path,
) -> Result<DecryptedNcm, NcmDecryptError> {
    let source = source.to_owned();
    let output_directory = output_directory.to_owned();
    tokio::task::spawn_blocking(move || decrypt_ncm_sync(&source, &output_directory))
        .await
        .map_err(|error| NcmDecryptError(format!("NCM decryption task failed: {error}")))?
}

fn decrypt_ncm_sync(
    source: &Path,
    output_directory: &Path,
) -> Result<DecryptedNcm, NcmDecryptError> {
    let file = File::open(source)
        .map_err(|error| NcmDecryptError(format!("Failed to open NCM container: {error}")))?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    read_and_verify_magic(&mut reader)
        .map_err(|error| NcmDecryptError(format!("Failed to parse NCM magic: {error}")))?;
    let key = read_rc4_key(&mut reader)
        .map_err(|error| NcmDecryptError(format!("Failed to parse NCM audio key: {error}")))?;
    if key.is_empty() {
        return Err(NcmDecryptError(
            "NCM audio key is empty after decryption".to_owned(),
        ));
    }
    let metadata = read_metadata(&mut reader)
        .map_err(|error| NcmDecryptError(format!("Failed to parse NCM metadata: {error}")))?;
    let cover = read_cover_with_reserved_space(&mut reader)?;

    let temporary_audio_path = output_directory.join("ncm-decrypted.part");
    let decode_result = (|| -> Result<(u64, AudioFormat), NcmDecryptError> {
        let output = File::create(&temporary_audio_path).map_err(|error| {
            NcmDecryptError(format!("Failed to create decrypted audio: {error}"))
        })?;
        let mut writer = BufWriter::new(output);
        let cipher = NcmStreamCipher::new(&key);
        let mut buffer = vec![0_u8; STREAM_CHUNK_BYTES];
        let mut probe = Vec::with_capacity(FORMAT_PROBE_BYTES);
        let mut written = 0_u64;
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|error| NcmDecryptError(format!("Failed to read NCM audio: {error}")))?;
            if read == 0 {
                break;
            }
            cipher.apply(&mut buffer[..read], written as usize);
            if probe.len() < FORMAT_PROBE_BYTES {
                let remaining = FORMAT_PROBE_BYTES - probe.len();
                probe.extend_from_slice(&buffer[..read.min(remaining)]);
            }
            writer.write_all(&buffer[..read]).map_err(|error| {
                NcmDecryptError(format!("Failed to write decrypted NCM audio: {error}"))
            })?;
            written += read as u64;
        }
        writer.flush().map_err(|error| {
            NcmDecryptError(format!("Failed to flush decrypted audio: {error}"))
        })?;
        if written == 0 {
            return Err(NcmDecryptError(
                "NCM audio payload is empty after decryption".to_owned(),
            ));
        }
        let detected_format = AudioFormat::detect(&probe);
        if detected_format == AudioFormat::Unknown {
            return Err(NcmDecryptError(
                "NCM audio payload format could not be identified after decryption".to_owned(),
            ));
        }
        Ok((written, detected_format))
    })();
    let (_, audio_format) = decode_result.inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary_audio_path);
    })?;
    let audio_path = output_directory.join(format!("ncm-decrypted.{}", audio_format.extension()));
    std::fs::rename(&temporary_audio_path, &audio_path)
        .map_err(|error| NcmDecryptError(format!("Failed to publish decrypted audio: {error}")))?;

    let artist = metadata.artists_joined(", ");
    let cover_path = match cover {
        Some((mime, data)) => {
            let extension = match mime {
                CoverMime::Jpeg => "jpg",
                CoverMime::Png => "png",
                CoverMime::Unknown => "bin",
            };
            let path = output_directory.join(format!("ncm-cover.{extension}"));
            if let Err(error) = std::fs::write(&path, data) {
                let _ = std::fs::remove_file(&audio_path);
                return Err(NcmDecryptError(format!(
                    "Failed to save NCM embedded cover: {error}"
                )));
            }
            Some(path)
        }
        None => None,
    };

    Ok(DecryptedNcm {
        audio_path,
        cover_path,
        title: metadata.title,
        artist,
        album: metadata.album,
    })
}

fn read_cover_with_reserved_space<R: Read>(
    reader: &mut R,
) -> Result<Option<(CoverMime, Vec<u8>)>, NcmDecryptError> {
    let mut crc = [0_u8; 4];
    reader
        .read_exact(&mut crc)
        .map_err(|error| NcmDecryptError(format!("Failed to read NCM CRC: {error}")))?;
    let mut gap = [0_u8; 1];
    reader
        .read_exact(&mut gap)
        .map_err(|error| NcmDecryptError(format!("Failed to read NCM cover gap: {error}")))?;

    let reserved = read_u32_le(reader, "cover reserved length")?;
    let actual = read_u32_le(reader, "cover length")?;
    if reserved > MAX_COVER_BYTES || actual > reserved {
        return Err(NcmDecryptError(format!(
            "Invalid NCM cover lengths: reserved={reserved}, actual={actual}"
        )));
    }

    let mut data = vec![0_u8; actual as usize];
    reader
        .read_exact(&mut data)
        .map_err(|error| NcmDecryptError(format!("Failed to read NCM cover: {error}")))?;
    skip_exact(reader, u64::from(reserved - actual), "cover reserved space")?;

    if data.is_empty() {
        Ok(None)
    } else {
        Ok(Some((CoverMime::detect(&data), data)))
    }
}

fn read_u32_le<R: Read>(reader: &mut R, name: &str) -> Result<u32, NcmDecryptError> {
    let mut bytes = [0_u8; 4];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| NcmDecryptError(format!("Failed to read NCM {name}: {error}")))?;
    Ok(u32::from_le_bytes(bytes))
}

fn skip_exact<R: Read>(reader: &mut R, length: u64, name: &str) -> Result<(), NcmDecryptError> {
    let copied = std::io::copy(&mut reader.take(length), &mut std::io::sink())
        .map_err(|error| NcmDecryptError(format!("Failed to skip NCM {name}: {error}")))?;
    if copied != length {
        return Err(NcmDecryptError(format!(
            "NCM ended while skipping {name}: expected {length} bytes, found {copied}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{decrypt_ncm, is_ncm_file, read_cover_with_reserved_space};
    use std::io::{Cursor, Read};
    use uuid::Uuid;

    #[tokio::test]
    async fn detects_ncm_magic_instead_of_trusting_the_extension() {
        let directory = std::env::temp_dir().join(format!("rust-app-ncm-magic-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let valid = directory.join("valid.bin");
        let invalid = directory.join("invalid.ncm");
        tokio::fs::write(&valid, b"CTENFDAMpayload").await.unwrap();
        tokio::fs::write(&invalid, b"not an ncm").await.unwrap();

        assert!(is_ncm_file(&valid).await.unwrap());
        assert!(!is_ncm_file(&invalid).await.unwrap());

        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn rejects_invalid_ncm_without_leaving_decrypted_files() {
        let directory =
            std::env::temp_dir().join(format!("rust-app-invalid-ncm-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let source = directory.join("invalid.ncm");
        tokio::fs::write(&source, b"CTENFDAMinvalid").await.unwrap();

        assert!(decrypt_ncm(&source, &directory).await.is_err());
        assert!(!directory.join("ncm-decrypted.part").exists());

        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[test]
    fn skips_unused_cover_reservation_before_audio_payload() {
        let mut container = Vec::new();
        container.extend_from_slice(&[0; 4]);
        container.push(1);
        container.extend_from_slice(&8_u32.to_le_bytes());
        container.extend_from_slice(&3_u32.to_le_bytes());
        container.extend_from_slice(&[0xff, 0xd8, 0xff]);
        container.extend_from_slice(&[0; 5]);
        container.extend_from_slice(b"ID3");

        let mut reader = Cursor::new(container);
        let cover = read_cover_with_reserved_space(&mut reader)
            .unwrap()
            .expect("embedded cover");
        assert_eq!(cover.1, [0xff, 0xd8, 0xff]);

        let mut audio_magic = [0_u8; 3];
        reader.read_exact(&mut audio_magic).unwrap();
        assert_eq!(&audio_magic, b"ID3");
    }
}
