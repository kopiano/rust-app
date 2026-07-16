CREATE TABLE IF NOT EXISTS moment (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    content TEXT,
    media JSONB NOT NULL DEFAULT '[]'::jsonb,
    like_count INT NOT NULL DEFAULT 0,
    comment_count INT NOT NULL DEFAULT 0,
    view_count BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS moment_user_id_created_at_idx
    ON moment (user_id, created_at DESC);

CREATE INDEX IF NOT EXISTS moment_created_at_idx
    ON moment (created_at DESC);
