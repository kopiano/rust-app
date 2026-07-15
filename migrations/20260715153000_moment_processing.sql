ALTER TABLE moment
    ADD COLUMN IF NOT EXISTS processing_status TEXT NOT NULL DEFAULT 'ready',
    ADD COLUMN IF NOT EXISTS processing_error TEXT;

ALTER TABLE moment
    DROP CONSTRAINT IF EXISTS moment_processing_status_check;

ALTER TABLE moment
    ADD CONSTRAINT moment_processing_status_check
    CHECK (processing_status IN ('processing', 'ready', 'failed'));

CREATE INDEX IF NOT EXISTS moment_processing_status_idx
    ON moment (processing_status)
    WHERE processing_status = 'processing';
