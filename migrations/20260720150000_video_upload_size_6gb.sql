ALTER TABLE video_upload
    DROP CONSTRAINT IF EXISTS video_upload_total_bytes_check;

ALTER TABLE video_upload
    ADD CONSTRAINT video_upload_total_bytes_check
    CHECK (total_bytes > 0 AND total_bytes <= 6442450944);
