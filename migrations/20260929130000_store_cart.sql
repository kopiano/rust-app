CREATE TABLE IF NOT EXISTS store_cart (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    product_id UUID NOT NULL REFERENCES store(id) ON DELETE CASCADE,
    size TEXT NOT NULL CHECK (size IN ('small', 'medium', 'large')),
    temperature TEXT NOT NULL CHECK (temperature IN ('iced', 'hot', 'room')),
    sweetness TEXT NOT NULL CHECK (sweetness IN ('standard', 'less', 'extra', 'none')),
    quantity INTEGER NOT NULL CHECK (quantity > 0 AND quantity <= 99),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (user_id, product_id, size, temperature, sweetness)
);

CREATE INDEX IF NOT EXISTS store_cart_user_idx ON store_cart(user_id, created_at ASC);
