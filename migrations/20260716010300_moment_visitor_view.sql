ALTER TABLE moment_view
    ALTER COLUMN user_id DROP NOT NULL,
    ADD COLUMN IF NOT EXISTS visitor_id UUID;

ALTER TABLE moment_view
    DROP CONSTRAINT IF EXISTS moment_view_viewer_check;

ALTER TABLE moment_view
    ADD CONSTRAINT moment_view_viewer_check CHECK (
        (user_id IS NOT NULL AND visitor_id IS NULL)
        OR (user_id IS NULL AND visitor_id IS NOT NULL)
    );

CREATE UNIQUE INDEX IF NOT EXISTS moment_view_visitor_daily_uidx
    ON moment_view (moment_id, visitor_id, viewed_on)
    WHERE visitor_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS moment_view_visitor_viewed_on_idx
    ON moment_view (visitor_id, viewed_on DESC)
    WHERE visitor_id IS NOT NULL;
