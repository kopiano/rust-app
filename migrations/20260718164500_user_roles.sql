ALTER TABLE "user"
    ADD COLUMN IF NOT EXISTS role VARCHAR(20) NOT NULL DEFAULT 'user';

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_schema = 'public'
          AND table_name = 'user'
          AND column_name = 'is_admin'
    ) THEN
        UPDATE "user"
        SET role = 'super_admin'
        WHERE is_admin = TRUE;
    END IF;
END
$$;

ALTER TABLE "user"
    DROP CONSTRAINT IF EXISTS user_role_check;

ALTER TABLE "user"
    ADD CONSTRAINT user_role_check
    CHECK (role IN ('user', 'admin', 'super_admin'));

CREATE INDEX IF NOT EXISTS user_role_idx
    ON "user" (role);

ALTER TABLE "user"
    DROP COLUMN IF EXISTS is_admin;
