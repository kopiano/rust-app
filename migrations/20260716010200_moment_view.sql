ALTER TABLE moment
    ADD COLUMN IF NOT EXISTS like_count INT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS comment_count INT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS view_count BIGINT NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS moment_view (
    id BIGSERIAL PRIMARY KEY,
    moment_id UUID NOT NULL REFERENCES moment(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    viewed_on DATE NOT NULL DEFAULT CURRENT_DATE,
    viewed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (moment_id, user_id, viewed_on)
);

CREATE INDEX IF NOT EXISTS moment_view_user_viewed_on_idx
    ON moment_view (user_id, viewed_on DESC);

UPDATE moment
SET like_count = (
        SELECT COUNT(*)::int
        FROM moment_like
        WHERE moment_like.moment_id = moment.id
    ),
    comment_count = (
        SELECT COUNT(*)::int
        FROM moment_comment
        WHERE moment_comment.moment_id = moment.id
          AND moment_comment.deleted_at IS NULL
    );
