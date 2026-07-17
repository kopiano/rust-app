CREATE TABLE IF NOT EXISTS music_favorite (
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    music_id UUID NOT NULL REFERENCES music(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, music_id)
);

CREATE INDEX IF NOT EXISTS music_favorite_user_created_at_idx
    ON music_favorite (user_id, created_at DESC, music_id);

CREATE INDEX IF NOT EXISTS music_favorite_music_id_idx
    ON music_favorite (music_id);

INSERT INTO music_favorite (user_id, music_id, created_at)
SELECT user_id, id, updated_at
FROM music
WHERE is_favorite = TRUE
ON CONFLICT (user_id, music_id) DO NOTHING;
