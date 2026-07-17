ALTER TABLE music
    ADD COLUMN IF NOT EXISTS file_hash VARCHAR(64);

CREATE UNIQUE INDEX IF NOT EXISTS music_user_file_hash_unique_idx
    ON music (user_id, file_hash)
    WHERE file_hash IS NOT NULL;
