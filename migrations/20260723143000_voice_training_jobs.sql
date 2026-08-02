CREATE TABLE IF NOT EXISTS voice_training_job (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    character_id UUID NOT NULL REFERENCES "character"(id) ON DELETE CASCADE,
    voice_model_id UUID NOT NULL REFERENCES character_voice_model(id) ON DELETE CASCADE,
    remote_job_id VARCHAR(128),
    model_id VARCHAR(128) NOT NULL,
    nickname VARCHAR(50) NOT NULL,
    version_name VARCHAR(50) NOT NULL,
    status VARCHAR(30) NOT NULL DEFAULT 'queued',
    progress INT NOT NULL DEFAULT 0,
    stage VARCHAR(80) NOT NULL DEFAULT 'queued',
    error TEXT,
    dataset_path TEXT NOT NULL,
    artifact_archive_path TEXT,
    manifest JSONB,
    remote_acknowledged BOOLEAN NOT NULL DEFAULT FALSE,
    acknowledged_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT voice_training_job_status_check CHECK (
        status IN (
            'queued',
            'uploading',
            'training',
            'downloading',
            'ready',
            'failed'
        )
    ),
    CONSTRAINT voice_training_job_progress_check CHECK (progress BETWEEN 0 AND 100)
);

CREATE UNIQUE INDEX IF NOT EXISTS voice_training_job_remote_id_unique
    ON voice_training_job (remote_job_id)
    WHERE remote_job_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS voice_training_job_user_created_idx
    ON voice_training_job (user_id, created_at DESC);

CREATE INDEX IF NOT EXISTS voice_training_job_status_idx
    ON voice_training_job (status, updated_at DESC);
