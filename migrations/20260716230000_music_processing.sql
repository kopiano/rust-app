ALTER TABLE music
    ADD COLUMN IF NOT EXISTS processing_status VARCHAR(16) NOT NULL DEFAULT 'ready',
    ADD COLUMN IF NOT EXISTS processing_error TEXT;

ALTER TABLE music
    DROP CONSTRAINT IF EXISTS music_processing_status_check;

ALTER TABLE music
    ADD CONSTRAINT music_processing_status_check
    CHECK (processing_status IN ('processing', 'ready', 'failed'));

CREATE INDEX IF NOT EXISTS music_processing_status_idx
    ON music (processing_status)
    WHERE processing_status = 'processing';
