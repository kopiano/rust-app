CREATE TABLE IF NOT EXISTS user_contact_character (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    contact_user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    character_id UUID NOT NULL REFERENCES "character"(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ DEFAULT now(),
    CONSTRAINT user_contact_character_not_self
        CHECK (user_id <> contact_user_id),
    CONSTRAINT user_contact_character_user_contact_unique
        UNIQUE (user_id, contact_user_id)
);

CREATE INDEX IF NOT EXISTS user_contact_character_contact_idx
    ON user_contact_character (contact_user_id);
