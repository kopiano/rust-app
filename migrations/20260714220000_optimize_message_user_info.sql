CREATE INDEX IF NOT EXISTS message_private_sender_latest_idx
    ON "message" (send_id, created_at DESC, id DESC)
    WHERE chat_type = 'private' AND deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS message_private_receiver_latest_idx
    ON "message" (receiver_id, created_at DESC, id DESC)
    WHERE chat_type = 'private' AND deleted_at IS NULL;
