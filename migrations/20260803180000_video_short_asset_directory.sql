ALTER TABLE video
    DROP CONSTRAINT IF EXISTS video_asset_directory_check;

ALTER TABLE video
    ADD CONSTRAINT video_asset_directory_check
    CHECK (
        asset_directory IS NULL
        OR asset_directory ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}-[A-Za-z0-9_-]+-[0-9a-fA-F]{8}$'
    ) NOT VALID;
