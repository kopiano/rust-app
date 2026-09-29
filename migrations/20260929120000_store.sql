CREATE TABLE IF NOT EXISTS store (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES "user"(id),
    name TEXT NOT NULL,
    category TEXT NOT NULL CHECK (category IN ('coffee', 'tea', 'bakery')),
    price INTEGER NOT NULL CHECK (price > 0 AND price <= 9999900),
    sales INTEGER NOT NULL DEFAULT 0 CHECK (sales >= 0),
    image_url TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS store_created_idx ON store(created_at ASC, id ASC);
