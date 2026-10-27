CREATE TABLE IF NOT EXISTS store_order (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES "user"(id),
    idempotency_key UUID NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'processing', 'confirmed', 'failed')),
    total BIGINT NOT NULL CHECK (total > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (user_id, idempotency_key)
);

CREATE INDEX IF NOT EXISTS store_order_user_created_idx
    ON store_order(user_id, created_at DESC);

CREATE TABLE IF NOT EXISTS store_order_item (
    id UUID PRIMARY KEY,
    order_id UUID NOT NULL REFERENCES store_order(id) ON DELETE CASCADE,
    product_id UUID NOT NULL,
    product_name TEXT NOT NULL,
    size TEXT NOT NULL CHECK (size IN ('small', 'medium', 'large')),
    temperature TEXT NOT NULL CHECK (temperature IN ('iced', 'hot', 'room')),
    sweetness TEXT NOT NULL CHECK (sweetness IN ('standard', 'less', 'extra', 'none')),
    quantity INTEGER NOT NULL CHECK (quantity > 0 AND quantity <= 99),
    unit_price INTEGER NOT NULL CHECK (unit_price > 0)
);

CREATE INDEX IF NOT EXISTS store_order_item_order_idx ON store_order_item(order_id);

CREATE TABLE IF NOT EXISTS store_order_outbox (
    event_id UUID PRIMARY KEY,
    order_id UUID NOT NULL REFERENCES store_order(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,
    payload JSONB NOT NULL,
    published_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS store_order_outbox_pending_idx
    ON store_order_outbox(created_at) WHERE published_at IS NULL;
