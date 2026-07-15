CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

CREATE TABLE IF NOT EXISTS "user" (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name VARCHAR(255) NOT NULL,
    email VARCHAR(255) NOT NULL UNIQUE,
    password_hash VARCHAR(255) NOT NULL DEFAULT '',
    github_id VARCHAR(255),
    avatar VARCHAR(2048),
    last_login_at TIMESTAMPTZ,
    status BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS user_name_unique
    ON "user" (name);

CREATE UNIQUE INDEX IF NOT EXISTS user_github_id_unique
    ON "user" (github_id)
    WHERE github_id IS NOT NULL;
