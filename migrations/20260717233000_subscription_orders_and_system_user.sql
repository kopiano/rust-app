ALTER TABLE "user"
    ADD COLUMN IF NOT EXISTS is_admin BOOLEAN NOT NULL DEFAULT FALSE;

UPDATE "user"
SET is_admin = TRUE
WHERE name = 'admin';

INSERT INTO "user" (
    name,
    email,
    password_hash,
    plan,
    subscription_status
)
VALUES (
    'System Notifications',
    'system@internal.local',
    '$2y$08$mnpm4SdYbuv8jY6GBq5DxOeLZWTbhoyhFStR7UclYBrbt0pCQ6SYC',
    'free',
    'active'
)
ON CONFLICT DO NOTHING;

CREATE TABLE IF NOT EXISTS subscription_order (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    order_no VARCHAR(64) NOT NULL UNIQUE,
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE RESTRICT,
    plan VARCHAR(20) NOT NULL,
    billing_cycle VARCHAR(16) NOT NULL,
    payment_method VARCHAR(32) NOT NULL,
    currency VARCHAR(8) NOT NULL,
    amount NUMERIC(12, 4) NOT NULL,
    contact_email VARCHAR(254),
    status VARCHAR(32) NOT NULL DEFAULT 'pending_confirmation',
    confirmed_by UUID REFERENCES "user"(id) ON DELETE SET NULL,
    confirmed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT subscription_order_billing_cycle_check
        CHECK (billing_cycle IN ('monthly', 'yearly')),
    CONSTRAINT subscription_order_payment_method_check
        CHECK (payment_method IN ('wechat_pay', 'alipay', 'union_pay')),
    CONSTRAINT subscription_order_currency_check
        CHECK (currency IN ('CNY', 'USD')),
    CONSTRAINT subscription_order_status_check
        CHECK (status IN ('pending_confirmation', 'succeeded', 'cancelled', 'expired'))
);

CREATE INDEX IF NOT EXISTS subscription_order_user_created_idx
    ON subscription_order (user_id, created_at DESC);

CREATE INDEX IF NOT EXISTS subscription_order_pending_idx
    ON subscription_order (status, created_at ASC)
    WHERE status = 'pending_confirmation';
