ALTER TABLE moment
    ADD COLUMN IF NOT EXISTS processing_progress SMALLINT NOT NULL DEFAULT 100;

UPDATE moment
SET processing_progress = 0
WHERE processing_status = 'processing';

ALTER TABLE moment
    DROP CONSTRAINT IF EXISTS moment_processing_progress_check;

ALTER TABLE moment
    ADD CONSTRAINT moment_processing_progress_check
    CHECK (processing_progress BETWEEN 0 AND 100);
