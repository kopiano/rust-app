ALTER TABLE "message"
    ADD COLUMN IF NOT EXISTS client_message_id UUID;

ALTER TABLE "message"
    DROP CONSTRAINT IF EXISTS message_sender_client_id_unique;

ALTER TABLE "message"
    ADD CONSTRAINT message_sender_client_id_unique
    UNIQUE (send_id, client_message_id);
