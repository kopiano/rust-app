CREATE TABLE IF NOT EXISTS video_comment (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    video_id UUID NOT NULL REFERENCES video(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    parent_id UUID,
    reply_to_user_id UUID REFERENCES "user"(id) ON DELETE SET NULL,
    content TEXT NOT NULL,
    like_count BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at TIMESTAMPTZ,
    CONSTRAINT video_comment_content_check
        CHECK (char_length(btrim(content)) BETWEEN 1 AND 1000),
    CONSTRAINT video_comment_like_count_check CHECK (like_count >= 0),
    CONSTRAINT video_comment_id_video_unique UNIQUE (id, video_id),
    CONSTRAINT video_comment_parent_same_video_fk
        FOREIGN KEY (parent_id, video_id)
        REFERENCES video_comment(id, video_id)
        ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS video_comment_video_created_at_idx
    ON video_comment (video_id, created_at ASC, id ASC)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS video_comment_parent_created_at_idx
    ON video_comment (parent_id, created_at ASC, id ASC)
    WHERE parent_id IS NOT NULL AND deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS video_comment_like (
    comment_id UUID NOT NULL REFERENCES video_comment(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (comment_id, user_id)
);

CREATE INDEX IF NOT EXISTS video_comment_like_user_idx
    ON video_comment_like (user_id, created_at DESC);
