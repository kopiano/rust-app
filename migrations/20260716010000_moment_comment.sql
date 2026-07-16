CREATE TABLE IF NOT EXISTS moment_comment (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    moment_id UUID NOT NULL REFERENCES moment(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    content TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at TIMESTAMPTZ,
    CONSTRAINT moment_comment_content_check
        CHECK (char_length(btrim(content)) BETWEEN 1 AND 1000)
);

CREATE INDEX IF NOT EXISTS moment_comment_moment_created_at_idx
    ON moment_comment (moment_id, created_at ASC, id ASC)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS moment_comment_user_id_idx
    ON moment_comment (user_id, created_at DESC);
