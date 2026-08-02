DO $$
BEGIN
    IF to_regclass('public.voice_model') IS NOT NULL
       AND to_regclass('public.character_voice_model') IS NULL THEN
        ALTER TABLE voice_model RENAME TO character_voice_model;
    END IF;
END $$;
