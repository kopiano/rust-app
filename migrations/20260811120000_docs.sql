CREATE TABLE IF NOT EXISTS docs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    slug TEXT NOT NULL,
    category TEXT NOT NULL DEFAULT 'Personal',
    excerpt TEXT NOT NULL DEFAULT '',
    markdown_path TEXT NOT NULL,
    image_url TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT docs_user_slug_unique UNIQUE (user_id, slug)
);

CREATE INDEX IF NOT EXISTS docs_user_updated_idx ON docs(user_id, updated_at DESC);
