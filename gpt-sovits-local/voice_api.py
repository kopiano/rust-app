import asyncio
import hashlib
import logging
import os
import re
import subprocess
import uuid
import wave
from contextlib import asynccontextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Literal

import httpx
from fastapi import FastAPI, HTTPException
from fastapi.responses import Response, StreamingResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel, Field


TRAIN_DIR = Path(os.getenv("VOICE_TRAIN_DIR", "/data/train")).resolve()
MEDIA_DIR = Path(os.getenv("VOICE_MEDIA_DIR", "/data/media")).resolve()
GPT_SOVITS_BASE_URL = os.getenv(
    "GPT_SOVITS_BASE_URL", "http://gpt-sovits:9880"
).rstrip("/")
PUBLIC_BASE_URL = os.getenv("PUBLIC_BASE_URL", "http://localhost:8200").rstrip("/")
MODEL_ID_PATTERN = re.compile(r"^(?P<character>[\w-]+)-v(?P<version>[1-9]\d*)$")
INFERENCE_LOCK = asyncio.Lock()
TTS_BATCH_SIZE = max(1, int(os.getenv("VOICE_TTS_BATCH_SIZE", "4")))
TTS_CACHE_ENABLED = os.getenv("VOICE_TTS_CACHE_ENABLED", "true").lower() not in {
    "0",
    "false",
    "no",
}
TTS_CHUNK_MAX_CHARACTERS = max(
    12, int(os.getenv("VOICE_TTS_CHUNK_MAX_CHARACTERS", "48"))
)
PRELOAD_MODEL_IDS = tuple(
    model_id.strip()
    for model_id in os.getenv("VOICE_PRELOAD_MODELS", "").split(",")
    if model_id.strip()
)
PRELOAD_WARMUP_ENABLED = os.getenv("VOICE_PRELOAD_WARMUP", "true").lower() not in {
    "0",
    "false",
    "no",
}
LOADED_MODEL_SIGNATURE: tuple[str, str] | None = None
GPT_SOVITS_CLIENT: httpx.AsyncClient | None = None
LOGGER = logging.getLogger("voice_api")


@dataclass(frozen=True)
class CachedSpeakerAssets:
    assets: tuple[Path, Path, Path, str, str]
    watched_paths: tuple[Path, ...]
    signatures: tuple[tuple[int, int], ...]


SPEAKER_ASSET_CACHE: dict[str, CachedSpeakerAssets] = {}

MEDIA_DIR.mkdir(parents=True, exist_ok=True)


@asynccontextmanager
async def lifespan(_: FastAPI):
    global GPT_SOVITS_CLIENT

    GPT_SOVITS_CLIENT = httpx.AsyncClient(
        timeout=httpx.Timeout(600, connect=15),
        limits=httpx.Limits(max_connections=16, max_keepalive_connections=8),
    )
    try:
        preload_assets = []
        for model_id in PRELOAD_MODEL_IDS:
            try:
                preload_assets.append((model_id, inference_assets(model_id)))
            except HTTPException as error:
                LOGGER.warning("Unable to cache speaker %s: %s", model_id, error.detail)
        if preload_assets:
            model_id, assets = preload_assets[0]
            try:
                if PRELOAD_WARMUP_ENABLED:
                    await warmup_model(model_id, assets)
                else:
                    async with INFERENCE_LOCK:
                        await ensure_model_loaded(GPT_SOVITS_CLIENT, assets[0], assets[1])
                LOGGER.info("Preloaded GPT-SoVITS model %s", model_id)
            except (HTTPException, httpx.HTTPError) as error:
                LOGGER.warning("Unable to preload GPT-SoVITS model %s: %s", model_id, error)
        yield
    finally:
        await GPT_SOVITS_CLIENT.aclose()
        GPT_SOVITS_CLIENT = None


app = FastAPI(
    title="Local GPT-SoVITS Voice API",
    version="2.1.0",
    lifespan=lifespan,
)
app.mount("/media", StaticFiles(directory=MEDIA_DIR), name="media")


class TtsRequest(BaseModel):
    text: str = Field(min_length=1, max_length=12000)
    model_id: str = Field(min_length=1, max_length=128)
    language: Literal["zh", "en"] = "zh"
    speed_factor: float = Field(default=1.0, ge=0.5, le=2.0)


class PreloadRequest(BaseModel):
    model_id: str = Field(min_length=1, max_length=128)


def api_response(data: Any = None, message: str = "ok") -> dict[str, Any]:
    return {"code": 200, "message": message, "data": data}


def model_directory(model_id: str) -> Path:
    match = MODEL_ID_PATTERN.fullmatch(model_id.strip())
    if not match:
        raise HTTPException(400, "Invalid voice model ID")
    path = (
        TRAIN_DIR / match.group("character") / f"v{match.group('version')}"
    ).resolve()
    try:
        path.relative_to(TRAIN_DIR)
    except ValueError as error:
        raise HTTPException(400, "Invalid voice model path") from error
    if not path.is_dir():
        raise HTTPException(404, "Voice model not found")
    return path


def first_file(directory: Path, suffix: str) -> Path:
    matches = sorted(path for path in directory.glob(f"*{suffix}") if path.is_file())
    if not matches:
        raise HTTPException(409, f"Voice model {suffix} artifact is missing")
    return matches[0]


def reference_record(model_dir: Path) -> tuple[Path, str, str]:
    dataset_path = model_dir / "dataset.list"
    if not dataset_path.is_file():
        raise HTTPException(409, "Voice model dataset.list is missing")

    for raw_line in dataset_path.read_text(encoding="utf-8-sig").splitlines():
        line = raw_line.strip()
        if not line:
            continue
        parts = [part.strip() for part in line.split("|", 3)]
        if len(parts) == 2:
            audio_name, prompt_text = parts
            prompt_language = "zh"
        elif len(parts) == 4:
            audio_name, _, language, prompt_text = parts
            prompt_language = "en" if language.lower().startswith("en") else "zh"
        else:
            continue
        audio_path = Path(audio_name)
        if not audio_path.is_absolute():
            candidates = [
                model_dir / audio_path,
                model_dir / "audio" / audio_path.name,
            ]
            audio_path = next(
                (candidate for candidate in candidates if candidate.is_file()),
                candidates[-1],
            )
        if audio_path.is_file() and prompt_text:
            return audio_path.resolve(), prompt_text, prompt_language

    raise HTTPException(409, "No usable reference audio and text in dataset.list")


def wav_duration(path: Path) -> float:
    try:
        with wave.open(str(path), "rb") as audio:
            return audio.getnframes() / audio.getframerate()
    except (OSError, wave.Error, ZeroDivisionError) as error:
        raise HTTPException(409, "Reference audio is not a readable WAV file") from error


def inference_reference(model_id: str, source: Path) -> Path:
    duration = wav_duration(source)
    if 3 <= duration <= 10:
        return source

    cache_dir = MEDIA_DIR / "reference-cache"
    cache_dir.mkdir(parents=True, exist_ok=True)
    cache_key = f"{model_id}-{source.stat().st_mtime_ns}"
    compact_path = cache_dir / f"{cache_key}-compact.wav"
    output_path = cache_dir / f"{cache_key}.wav"
    if output_path.is_file() and 3 <= wav_duration(output_path) <= 10:
        return output_path

    compact_command = [
        "ffmpeg",
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-i",
        str(source),
        "-af",
        (
            "silenceremove="
            "start_periods=1:start_duration=0.1:start_threshold=-35dB:"
            "stop_periods=-1:stop_duration=0.25:stop_threshold=-35dB"
        ),
        "-ac",
        "1",
        "-ar",
        "32000",
        str(compact_path),
    ]
    try:
        subprocess.run(compact_command, check=True, capture_output=True)
        compact_duration = wav_duration(compact_path)
        if compact_duration > 9.8:
            speed = compact_duration / 9.5
            subprocess.run(
                [
                    "ffmpeg",
                    "-y",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-i",
                    str(compact_path),
                    "-af",
                    f"atempo={speed:.6f}",
                    str(output_path),
                ],
                check=True,
                capture_output=True,
            )
        else:
            compact_path.replace(output_path)
    except (OSError, subprocess.CalledProcessError) as error:
        raise HTTPException(500, "Unable to prepare reference audio") from error
    finally:
        compact_path.unlink(missing_ok=True)

    final_duration = wav_duration(output_path)
    if not 3 <= final_duration <= 10:
        raise HTTPException(
            409, "Reference audio cannot be normalized to the 3-10 second range"
        )
    return output_path


def inference_assets(model_id: str) -> tuple[Path, Path, Path, str, str]:
    cached = SPEAKER_ASSET_CACHE.get(model_id)
    if cached is not None:
        try:
            signatures = tuple(
                (path.stat().st_size, path.stat().st_mtime_ns)
                for path in cached.watched_paths
            )
        except OSError:
            signatures = ()
        if signatures == cached.signatures:
            return cached.assets

    model_dir = model_directory(model_id)
    checkpoint_path = first_file(model_dir / "models", ".ckpt")
    sovits_path = first_file(model_dir / "models", ".pth")
    reference_audio, prompt_text, prompt_language = reference_record(model_dir)
    source_reference_audio = reference_audio
    reference_audio = inference_reference(model_id, reference_audio)
    assets = (
        checkpoint_path,
        sovits_path,
        reference_audio,
        prompt_text,
        prompt_language,
    )
    watched_paths = (
        model_dir / "dataset.list",
        checkpoint_path,
        sovits_path,
        source_reference_audio,
    )
    signatures = tuple(
        (path.stat().st_size, path.stat().st_mtime_ns) for path in watched_paths
    )
    SPEAKER_ASSET_CACHE[model_id] = CachedSpeakerAssets(
        assets=assets,
        watched_paths=watched_paths,
        signatures=signatures,
    )
    return assets


def split_tts_text(text: str, max_characters: int = TTS_CHUNK_MAX_CHARACTERS) -> list[str]:
    normalized = text.strip()
    if not normalized:
        return []

    chunks: list[str] = []
    current: list[str] = []
    boundaries = set("。！？!?；;，,：:\n")
    for character in normalized:
        current.append(character)
        current_text = "".join(current).strip()
        if (
            (character in boundaries and len(current_text) >= 8)
            or len(current_text) >= max_characters
        ):
            chunks.append(current_text)
            current = []
    remaining = "".join(current).strip()
    if remaining:
        chunks.append(remaining)
    return chunks


def shared_gpt_sovits_client() -> httpx.AsyncClient:
    if GPT_SOVITS_CLIENT is None:
        raise HTTPException(503, "Voice inference client is not ready")
    return GPT_SOVITS_CLIENT


def audio_url(path: Path) -> str:
    relative_path = path.relative_to(MEDIA_DIR).as_posix()
    return f"{PUBLIC_BASE_URL}/media/{relative_path}"


def tts_cache_path(
    request: TtsRequest,
    checkpoint_path: Path,
    sovits_path: Path,
    reference_audio: Path,
    prompt_text: str,
    prompt_language: str,
) -> Path:
    digest = hashlib.sha256()
    for value in (
        "voice-api-v3",
        request.model_id,
        request.text,
        request.language,
        f"{request.speed_factor:.4f}",
        str(TTS_BATCH_SIZE),
        prompt_text,
        prompt_language,
    ):
        digest.update(value.encode("utf-8"))
        digest.update(b"\0")
    for path in (checkpoint_path, sovits_path, reference_audio):
        stat = path.stat()
        digest.update(str(path).encode("utf-8"))
        digest.update(f":{stat.st_size}:{stat.st_mtime_ns}".encode("ascii"))
        digest.update(b"\0")
    cache_dir = MEDIA_DIR / "tts-cache"
    cache_dir.mkdir(parents=True, exist_ok=True)
    return cache_dir / f"{digest.hexdigest()}.wav"


async def ensure_model_loaded(
    client: httpx.AsyncClient,
    checkpoint_path: Path,
    sovits_path: Path,
) -> None:
    global LOADED_MODEL_SIGNATURE

    signature = (str(checkpoint_path), str(sovits_path))
    if LOADED_MODEL_SIGNATURE == signature:
        return

    for endpoint, artifact in (
        ("set_gpt_weights", checkpoint_path),
        ("set_sovits_weights", sovits_path),
    ):
        response = await client.get(
            f"{GPT_SOVITS_BASE_URL}/{endpoint}",
            params={"weights_path": str(artifact)},
        )
        if response.status_code >= 400:
            LOADED_MODEL_SIGNATURE = None
            raise HTTPException(
                502,
                f"GPT-SoVITS {endpoint} failed: {response.text[:500]}",
            )
    LOADED_MODEL_SIGNATURE = signature


async def gpt_sovits_request(
    request: TtsRequest,
    checkpoint_path: Path,
    sovits_path: Path,
    reference_audio: Path,
    prompt_text: str,
    prompt_language: str,
) -> httpx.Response:
    client = shared_gpt_sovits_client()
    async with INFERENCE_LOCK:
        await ensure_model_loaded(client, checkpoint_path, sovits_path)

        return await client.post(
            f"{GPT_SOVITS_BASE_URL}/tts",
            json={
                "text": request.text,
                "text_lang": request.language,
                "ref_audio_path": str(reference_audio),
                "prompt_text": prompt_text,
                "prompt_lang": prompt_language,
                "speed_factor": request.speed_factor,
                "text_split_method": "cut5",
                "batch_size": TTS_BATCH_SIZE,
                "split_bucket": True,
                "parallel_infer": True,
                "streaming_mode": False,
                "media_type": "wav",
            },
        )


async def warmup_model(
    model_id: str,
    assets: tuple[Path, Path, Path, str, str],
) -> None:
    prompt_text = assets[3].strip()
    warmup_chunks = split_tts_text(prompt_text, max_characters=24)
    warmup_text = warmup_chunks[0] if warmup_chunks else "你好。"
    response = await gpt_sovits_request(
        TtsRequest(
            text=warmup_text,
            model_id=model_id,
            language=assets[4],
            speed_factor=1.0,
        ),
        *assets,
    )
    if response.status_code >= 400 or not response.content:
        raise HTTPException(502, "GPT-SoVITS warmup failed")


async def gpt_sovits_stream(
    request: TtsRequest,
    checkpoint_path: Path,
    sovits_path: Path,
    reference_audio: Path,
    prompt_text: str,
    prompt_language: str,
):
    client = shared_gpt_sovits_client()
    try:
        async with INFERENCE_LOCK:
            await ensure_model_loaded(client, checkpoint_path, sovits_path)
            for text_chunk in split_tts_text(request.text):
                async with client.stream(
                    "POST",
                    f"{GPT_SOVITS_BASE_URL}/tts",
                    json={
                        "text": text_chunk,
                        "text_lang": request.language,
                        "ref_audio_path": str(reference_audio),
                        "prompt_text": prompt_text,
                        "prompt_lang": prompt_language,
                        "speed_factor": request.speed_factor,
                        "text_split_method": "cut5",
                        "batch_size": 1,
                        "split_bucket": False,
                        "parallel_infer": False,
                        "streaming_mode": 3,
                        "media_type": "aac",
                        "min_chunk_length": 8,
                        "overlap_length": 2,
                        "fragment_interval": 0.01,
                    },
                ) as response:
                    if response.status_code >= 400:
                        detail = (await response.aread())[:500].decode(
                            "utf-8", errors="replace"
                        )
                        LOGGER.warning(
                            "GPT-SoVITS stream failed with status %s: %s",
                            response.status_code,
                            detail,
                        )
                        return
                    content_type = response.headers.get("content-type", "").lower()
                    if not content_type.startswith("audio/"):
                        await response.aread()
                        LOGGER.warning("GPT-SoVITS stream returned invalid audio")
                        return
                    async for chunk in response.aiter_bytes():
                        if chunk:
                            yield chunk
    except HTTPException as error:
        LOGGER.warning("GPT-SoVITS stream setup failed: %s", error.detail)
    except httpx.HTTPError as error:
        LOGGER.warning("GPT-SoVITS stream connection ended early: %s", error)


@app.get("/health")
async def health() -> dict[str, Any]:
    try:
        async with httpx.AsyncClient(timeout=5) as client:
            response = await client.get(f"{GPT_SOVITS_BASE_URL}/docs")
        inference_ready = response.status_code < 500
    except httpx.HTTPError:
        inference_ready = False
    return api_response(
        {
            "service": "local-gpt-sovits-voice-api",
            "inference_ready": inference_ready,
            "train_dir": str(TRAIN_DIR),
            "loaded_model": (
                list(LOADED_MODEL_SIGNATURE)
                if LOADED_MODEL_SIGNATURE is not None
                else None
            ),
            "tts_batch_size": TTS_BATCH_SIZE,
            "tts_cache_enabled": TTS_CACHE_ENABLED,
            "tts_chunk_max_characters": TTS_CHUNK_MAX_CHARACTERS,
            "speaker_cache_size": len(SPEAKER_ASSET_CACHE),
            "preload_models": list(PRELOAD_MODEL_IDS),
            "preload_warmup": PRELOAD_WARMUP_ENABLED,
        }
    )


@app.post("/voice/models/preload")
async def preload_model(request: PreloadRequest) -> dict[str, Any]:
    assets = inference_assets(request.model_id)
    if PRELOAD_WARMUP_ENABLED:
        await warmup_model(request.model_id, assets)
    else:
        async with INFERENCE_LOCK:
            await ensure_model_loaded(shared_gpt_sovits_client(), assets[0], assets[1])
    return api_response(
        {
            "model_id": request.model_id,
            "speaker_cached": request.model_id in SPEAKER_ASSET_CACHE,
        },
        "Voice model preloaded",
    )


@app.post("/voice/tts")
async def text_to_speech(request: TtsRequest) -> dict[str, Any]:
    assets = inference_assets(request.model_id)
    cache_path = tts_cache_path(request, *assets) if TTS_CACHE_ENABLED else None
    if cache_path is not None and cache_path.is_file():
        return api_response(
            {
                "text": request.text,
                "audio_url": audio_url(cache_path),
                "cached": True,
            },
            "Character speech loaded from cache",
        )
    try:
        response = await gpt_sovits_request(request, *assets)
    except httpx.RequestError as error:
        raise HTTPException(502, "GPT-SoVITS inference service is unavailable") from error
    if response.status_code >= 400:
        raise HTTPException(
            502, f"GPT-SoVITS inference failed: {response.text[:500]}"
        )
    content_type = response.headers.get("content-type", "").lower()
    if not response.content or not content_type.startswith("audio/"):
        raise HTTPException(502, "GPT-SoVITS returned invalid audio")

    audio_path = cache_path or (MEDIA_DIR / f"{uuid.uuid4().hex}.wav")
    audio_path.write_bytes(response.content)
    return api_response(
        {
            "text": request.text,
            "audio_url": audio_url(audio_path),
            "cached": False,
        },
        "Character speech generated",
    )


@app.post("/voice/tts/stream")
async def text_to_speech_stream(request: TtsRequest):
    assets = inference_assets(request.model_id)
    return StreamingResponse(
        gpt_sovits_stream(request, *assets),
        media_type="audio/aac",
        headers={
            "Cache-Control": "no-store, no-transform",
            "X-Accel-Buffering": "no",
            "X-Voice-Stream": "byte",
        },
    )


@app.post("/voice/tts/audio")
async def text_to_speech_audio(request: TtsRequest):
    assets = inference_assets(request.model_id)
    cache_path = tts_cache_path(request, *assets) if TTS_CACHE_ENABLED else None
    if cache_path is not None and cache_path.is_file():
        return Response(
            content=cache_path.read_bytes(),
            media_type="audio/wav",
            headers={"Cache-Control": "private, max-age=3600", "X-Voice-Cache": "hit"},
        )
    try:
        response = await gpt_sovits_request(request, *assets)
    except httpx.RequestError as error:
        raise HTTPException(502, "GPT-SoVITS inference service is unavailable") from error
    if response.status_code >= 400:
        raise HTTPException(
            502, f"GPT-SoVITS inference failed: {response.text[:500]}"
        )
    content_type = response.headers.get("content-type", "").lower()
    if not response.content or not content_type.startswith("audio/"):
        raise HTTPException(502, "GPT-SoVITS returned invalid audio")
    if cache_path is not None:
        cache_path.write_bytes(response.content)
    return Response(
        content=response.content,
        media_type=content_type.split(";", 1)[0],
        headers={"Cache-Control": "no-store", "X-Voice-Cache": "miss"},
    )
