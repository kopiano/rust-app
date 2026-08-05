-- Collection queries frequently resolve favorites by owner first, while the
-- existing primary key starts with video_id.
CREATE INDEX IF NOT EXISTS video_favorite_user_video_idx
    ON video_favorite (user_id, video_id);

-- Supports the public/ready branch used by dynamic collections and covers the
-- ordering needed when selecting collection covers.
CREATE INDEX IF NOT EXISTS video_public_ready_user_created_at_idx
    ON video (user_id, created_at DESC, id DESC)
    WHERE visibility = 'public'
      AND status = 'ready'
      AND published_at IS NOT NULL;
