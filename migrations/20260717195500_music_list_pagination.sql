CREATE INDEX IF NOT EXISTS music_visible_created_at_idx
    ON music (created_at DESC, id DESC)
    WHERE processing_status <> 'failed';

CREATE INDEX IF NOT EXISTS music_user_visible_created_at_idx
    ON music (user_id, created_at DESC, id DESC)
    WHERE processing_status <> 'failed';

CREATE INDEX IF NOT EXISTS music_favorite_visible_created_at_idx
    ON music (created_at DESC, id DESC)
    WHERE processing_status <> 'failed' AND is_favorite = TRUE;

CREATE INDEX IF NOT EXISTS music_user_favorite_visible_created_at_idx
    ON music (user_id, created_at DESC, id DESC)
    WHERE processing_status <> 'failed' AND is_favorite = TRUE;
