ALTER TABLE video
    ADD COLUMN IF NOT EXISTS published_at TIMESTAMPTZ;

ALTER TABLE video
    DROP CONSTRAINT IF EXISTS video_status_check;

ALTER TABLE video
    ADD CONSTRAINT video_status_check
    CHECK (status IN ('uploading', 'processing', 'ready', 'failed'));

UPDATE video
SET published_at = COALESCE(updated_at, created_at)
WHERE published_at IS NULL
  AND visibility = 'public'
  AND status = 'ready';

CREATE INDEX IF NOT EXISTS video_ready_published_created_at_idx
    ON video (created_at DESC, id DESC)
    WHERE visibility = 'public'
      AND status = 'ready'
      AND published_at IS NOT NULL;
