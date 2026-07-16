CREATE TABLE IF NOT EXISTS music (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    title VARCHAR(512) NOT NULL,
    artist VARCHAR(512) NOT NULL DEFAULT 'Unknown Artist',
    album VARCHAR(512) NOT NULL DEFAULT 'Unknown Album',
    duration_ms BIGINT NOT NULL CHECK (duration_ms >= 0),
    bitrate INTEGER NOT NULL CHECK (bitrate >= 0),
    sample_rate INTEGER NOT NULL CHECK (sample_rate >= 0),
    cover_url VARCHAR(2048) NOT NULL,
    audio_url VARCHAR(2048) NOT NULL,
    original_url VARCHAR(2048) NOT NULL,
    format VARCHAR(32) NOT NULL DEFAULT 'm4a',
    original_format VARCHAR(32) NOT NULL,
    size BIGINT NOT NULL CHECK (size >= 0),
    original_size BIGINT NOT NULL CHECK (original_size >= 0),
    is_favorite BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS music_user_created_at_idx
    ON music (user_id, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS music_user_favorite_created_at_idx
    ON music (user_id, is_favorite, created_at DESC, id DESC);
