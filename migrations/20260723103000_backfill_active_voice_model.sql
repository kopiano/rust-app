UPDATE "character" AS character
SET
    active_voice_model_id = model.id,
    voice_model = COALESCE(model.model_id, model.name),
    ckpt_path = model.ckpt_path,
    pth_path = model.pth_path,
    train_status = model.status,
    updated_at = NOW()
FROM voice_model AS model
WHERE character.active_voice_model_id IS NULL
  AND model.character_id = character.id
  AND model.status = 'ready'
  AND (
      model.name = character.voice_model
      OR NOT EXISTS (
          SELECT 1
          FROM voice_model AS existing
          WHERE existing.character_id = character.id
            AND existing.name = character.voice_model
      )
  )
  AND model.version = (
      SELECT MAX(candidate.version)
      FROM voice_model AS candidate
      WHERE candidate.character_id = character.id
        AND candidate.status = 'ready'
        AND (
            candidate.name = character.voice_model
            OR NOT EXISTS (
                SELECT 1
                FROM voice_model AS matching
                WHERE matching.character_id = character.id
                  AND matching.name = character.voice_model
            )
        )
  );
