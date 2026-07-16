CREATE TABLE IF NOT EXISTS moment_like (
    moment_id UUID NOT NULL REFERENCES moment(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (moment_id, user_id)
);

CREATE INDEX IF NOT EXISTS moment_like_user_id_created_at_idx
    ON moment_like (user_id, created_at DESC);
