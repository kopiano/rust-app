CREATE TABLE IF NOT EXISTS "group" (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name VARCHAR(255) NOT NULL,
    avatar VARCHAR(2048),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS group_member (
    group_id UUID NOT NULL REFERENCES "group"(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (group_id, user_id)
);

ALTER TABLE "message"
    ADD COLUMN IF NOT EXISTS group_id UUID REFERENCES "group"(id) ON DELETE CASCADE;

CREATE INDEX IF NOT EXISTS group_member_user_id_idx
    ON group_member (user_id, group_id);

CREATE INDEX IF NOT EXISTS message_group_latest_idx
    ON "message" (group_id, created_at DESC, id DESC)
    WHERE group_id IS NOT NULL AND deleted_at IS NULL;
