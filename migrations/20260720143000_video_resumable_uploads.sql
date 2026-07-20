CREATE TABLE IF NOT EXISTS video_upload (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    video_id UUID NOT NULL UNIQUE REFERENCES video(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    file_extension VARCHAR(16) NOT NULL,
    total_bytes BIGINT NOT NULL,
    uploaded_bytes BIGINT NOT NULL DEFAULT 0,
    status VARCHAR(20) NOT NULL DEFAULT 'uploading',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT video_upload_extension_check
        CHECK (file_extension ~ '^[a-z0-9]{1,16}$'),
    CONSTRAINT video_upload_total_bytes_check
        CHECK (total_bytes > 0 AND total_bytes <= 2147483648),
    CONSTRAINT video_upload_uploaded_bytes_check
        CHECK (uploaded_bytes >= 0 AND uploaded_bytes <= total_bytes),
    CONSTRAINT video_upload_status_check
        CHECK (status IN ('uploading', 'complete'))
);

CREATE INDEX IF NOT EXISTS video_upload_user_updated_at_idx
    ON video_upload (user_id, updated_at DESC);
