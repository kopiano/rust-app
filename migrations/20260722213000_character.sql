CREATE TABLE IF NOT EXISTS "character" (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(50) NOT NULL,
    avatar_url TEXT,
    description TEXT,
    system_prompt TEXT,
    voice_model VARCHAR(100),
    ckpt_path TEXT,
    pth_path TEXT,
    train_status VARCHAR(20) NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT character_name_check
        CHECK (char_length(btrim(name)) BETWEEN 1 AND 50),
    CONSTRAINT character_train_status_check
        CHECK (
            train_status IN (
                'pending',
                'queued',
                'training',
                'ready',
                'awaiting_training_command',
                'failed'
            )
        )
);

CREATE UNIQUE INDEX IF NOT EXISTS character_voice_model_unique
    ON "character" (voice_model)
    WHERE voice_model IS NOT NULL;

CREATE INDEX IF NOT EXISTS character_train_status_idx
    ON "character" (train_status, updated_at DESC);

CREATE TABLE IF NOT EXISTS character_chat_session (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    character_id UUID NOT NULL REFERENCES "character"(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS character_chat_session_user_idx
    ON character_chat_session (user_id, updated_at DESC);

CREATE TABLE IF NOT EXISTS character_chat_message (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES character_chat_session(id) ON DELETE CASCADE,
    role VARCHAR(20) NOT NULL,
    content TEXT NOT NULL,
    audio_url TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT character_chat_message_role_check
        CHECK (role IN ('user', 'assistant')),
    CONSTRAINT character_chat_message_content_check
        CHECK (char_length(btrim(content)) BETWEEN 1 AND 12000)
);

CREATE INDEX IF NOT EXISTS character_chat_message_session_idx
    ON character_chat_message (session_id, created_at DESC);
