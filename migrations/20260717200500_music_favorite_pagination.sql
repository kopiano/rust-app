CREATE INDEX IF NOT EXISTS music_favorite_visible_created_at_idx
    ON music (created_at DESC, id DESC)
    WHERE processing_status <> 'failed' AND is_favorite = TRUE;
