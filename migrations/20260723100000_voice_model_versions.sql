CREATE TABLE IF NOT EXISTS voice_model (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    character_id UUID NOT NULL REFERENCES "character"(id) ON DELETE CASCADE,
    model_id VARCHAR(128),
    version INT NOT NULL,
    name VARCHAR(50) NOT NULL,
    ckpt_path TEXT,
    pth_path TEXT,
    reference_audio_path TEXT,
    dataset_path TEXT,
    status VARCHAR(30) NOT NULL DEFAULT 'queued',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT voice_model_version_positive CHECK (version > 0),
    CONSTRAINT voice_model_status_check CHECK (
        status IN (
            'queued',
            'training',
            'ready',
            'awaiting_training_command',
            'failed'
        )
    ),
    CONSTRAINT voice_model_character_version_unique UNIQUE (character_id, version)
);

ALTER TABLE voice_model
    ADD COLUMN IF NOT EXISTS model_id VARCHAR(128);

CREATE UNIQUE INDEX IF NOT EXISTS voice_model_character_model_id_unique
    ON voice_model (character_id, model_id)
    WHERE model_id IS NOT NULL;

ALTER TABLE "character"
    ADD COLUMN IF NOT EXISTS active_voice_model_id UUID;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'character_active_voice_model_fk'
    ) THEN
        ALTER TABLE "character"
            ADD CONSTRAINT character_active_voice_model_fk
            FOREIGN KEY (active_voice_model_id)
            REFERENCES voice_model(id)
            ON DELETE SET NULL;
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS voice_model_character_idx
    ON voice_model (character_id, version DESC);

CREATE INDEX IF NOT EXISTS voice_model_status_idx
    ON voice_model (status, updated_at DESC);
