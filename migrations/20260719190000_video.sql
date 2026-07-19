CREATE TABLE IF NOT EXISTS video (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    title VARCHAR(255),
    description TEXT,
    cover_url TEXT NOT NULL,
    duration INT NOT NULL,
    width INT,
    height INT,
    fps NUMERIC(8,3),
    size BIGINT,
    origin_file_url TEXT,
    hls_master_url TEXT,
    status VARCHAR(20) NOT NULL DEFAULT 'processing',
    visibility VARCHAR(20) NOT NULL DEFAULT 'public',
    processing_progress SMALLINT NOT NULL DEFAULT 0,
    processing_error TEXT,
    view_count BIGINT NOT NULL DEFAULT 0,
    like_count BIGINT NOT NULL DEFAULT 0,
    comment_count BIGINT NOT NULL DEFAULT 0,
    favorite_count BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT video_status_check CHECK (status IN ('processing', 'ready', 'failed')),
    CONSTRAINT video_visibility_check CHECK (visibility IN ('public', 'private')),
    CONSTRAINT video_processing_progress_check CHECK (processing_progress BETWEEN 0 AND 100),
    CONSTRAINT video_duration_check CHECK (duration >= 0),
    CONSTRAINT video_dimensions_check CHECK (
        (width IS NULL OR width > 0) AND (height IS NULL OR height > 0)
    ),
    CONSTRAINT video_size_check CHECK (size IS NULL OR size >= 0),
    CONSTRAINT video_counters_check CHECK (
        view_count >= 0 AND like_count >= 0
        AND comment_count >= 0 AND favorite_count >= 0
    )
);

CREATE INDEX IF NOT EXISTS video_public_created_at_idx
    ON video (created_at DESC, id DESC)
    WHERE visibility = 'public';

CREATE INDEX IF NOT EXISTS video_user_created_at_idx
    ON video (user_id, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS video_status_idx
    ON video (status, created_at ASC)
    WHERE status = 'processing';

CREATE INDEX IF NOT EXISTS video_title_search_idx
    ON video (LOWER(title));

CREATE TABLE IF NOT EXISTS video_category (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    slug VARCHAR(80) NOT NULL UNIQUE,
    name_zh VARCHAR(80) NOT NULL,
    name_en VARCHAR(80) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO video_category (slug, name_zh, name_en)
VALUES
    ('travel', '旅行', 'Travel'),
    ('design', '设计', 'Design'),
    ('nature', '自然', 'Nature'),
    ('movies', '电影', 'Movies'),
    ('music', '音乐', 'Music'),
    ('gaming', '游戏', 'Gaming'),
    ('technology', '科技', 'Technology'),
    ('other', '其它', 'Other')
ON CONFLICT (slug) DO UPDATE
SET name_zh = EXCLUDED.name_zh,
    name_en = EXCLUDED.name_en;

CREATE TABLE IF NOT EXISTS video_category_map (
    video_id UUID NOT NULL REFERENCES video(id) ON DELETE CASCADE,
    category_id UUID NOT NULL REFERENCES video_category(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (video_id, category_id)
);

CREATE INDEX IF NOT EXISTS video_category_map_category_idx
    ON video_category_map (category_id, video_id);

CREATE TABLE IF NOT EXISTS video_like (
    video_id UUID NOT NULL REFERENCES video(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (video_id, user_id)
);

CREATE INDEX IF NOT EXISTS video_like_user_created_at_idx
    ON video_like (user_id, created_at DESC, video_id);

CREATE TABLE IF NOT EXISTS video_favorite (
    video_id UUID NOT NULL REFERENCES video(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (video_id, user_id)
);

CREATE INDEX IF NOT EXISTS video_favorite_user_created_at_idx
    ON video_favorite (user_id, created_at DESC, video_id);

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

CREATE TABLE IF NOT EXISTS video_view (
    id BIGSERIAL PRIMARY KEY,
    video_id UUID NOT NULL REFERENCES video(id) ON DELETE CASCADE,
    user_id UUID REFERENCES "user"(id) ON DELETE CASCADE,
    visitor_id UUID,
    viewed_on DATE NOT NULL DEFAULT CURRENT_DATE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT video_view_viewer_check CHECK (
        (user_id IS NOT NULL AND visitor_id IS NULL)
        OR (user_id IS NULL AND visitor_id IS NOT NULL)
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS video_view_user_daily_uidx
    ON video_view (video_id, user_id, viewed_on)
    WHERE user_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS video_view_visitor_daily_uidx
    ON video_view (video_id, visitor_id, viewed_on)
    WHERE visitor_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS video_collection (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    title VARCHAR(120) NOT NULL,
    description TEXT,
    visibility VARCHAR(20) NOT NULL DEFAULT 'public',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT video_collection_title_check
        CHECK (char_length(btrim(title)) BETWEEN 1 AND 120),
    CONSTRAINT video_collection_visibility_check
        CHECK (visibility IN ('public', 'private'))
);

CREATE INDEX IF NOT EXISTS video_collection_user_created_at_idx
    ON video_collection (user_id, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS video_collection_public_created_at_idx
    ON video_collection (created_at DESC, id DESC)
    WHERE visibility = 'public';

CREATE TABLE IF NOT EXISTS video_collection_item (
    collection_id UUID NOT NULL REFERENCES video_collection(id) ON DELETE CASCADE,
    video_id UUID NOT NULL REFERENCES video(id) ON DELETE CASCADE,
    position INT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (collection_id, video_id),
    CONSTRAINT video_collection_item_position_check CHECK (position >= 0)
);

CREATE INDEX IF NOT EXISTS video_collection_item_order_idx
    ON video_collection_item (collection_id, position ASC, created_at ASC);
