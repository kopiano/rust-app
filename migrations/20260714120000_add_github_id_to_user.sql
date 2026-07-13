ALTER TABLE "user"
    ADD COLUMN IF NOT EXISTS github_id VARCHAR(255);

CREATE UNIQUE INDEX IF NOT EXISTS user_github_id_unique
    ON "user" (github_id)
    WHERE github_id IS NOT NULL;
