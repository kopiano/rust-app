ALTER TABLE voice_model
    ADD COLUMN IF NOT EXISTS model_id VARCHAR(128);

CREATE UNIQUE INDEX IF NOT EXISTS voice_model_character_model_id_unique
    ON voice_model (character_id, model_id)
    WHERE model_id IS NOT NULL;
