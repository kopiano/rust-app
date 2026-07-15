CREATE TABLE IF NOT EXISTS "message" (
    id BIGSERIAL PRIMARY KEY,
    conversation_id UUID NOT NULL DEFAULT uuid_generate_v4() UNIQUE,
    chat_type VARCHAR(16) NOT NULL,
    send_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    receiver_id UUID REFERENCES "user"(id) ON DELETE CASCADE,
    group_id UUID REFERENCES "group"(id) ON DELETE CASCADE,
    content TEXT,
    message_type SMALLINT NOT NULL DEFAULT 1,
    status VARCHAR(32) NOT NULL DEFAULT 'sending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    update_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at TIMESTAMPTZ,
    file_name VARCHAR(255),
    file_url VARCHAR(2048),
    CONSTRAINT message_chat_type_check CHECK (chat_type IN ('private', 'public')),
    CONSTRAINT message_type_check CHECK (message_type IN (1, 2, 3)),
    CONSTRAINT message_private_receiver_check CHECK (
        chat_type = 'public' OR receiver_id IS NOT NULL
    )
);

CREATE INDEX IF NOT EXISTS message_group_latest_idx
    ON "message" (group_id, created_at DESC, id DESC)
    WHERE group_id IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS message_private_sender_latest_idx
    ON "message" (send_id, created_at DESC, id DESC)
    WHERE chat_type = 'private' AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS message_private_receiver_latest_idx
    ON "message" (receiver_id, created_at DESC, id DESC)
    WHERE chat_type = 'private' AND deleted_at IS NULL;
