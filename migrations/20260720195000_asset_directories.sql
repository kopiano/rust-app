ALTER TABLE music
    ADD COLUMN IF NOT EXISTS asset_directory TEXT;

ALTER TABLE video
    ADD COLUMN IF NOT EXISTS asset_directory TEXT;

ALTER TABLE music
    ADD CONSTRAINT music_asset_directory_check
    CHECK (asset_directory IS NULL OR asset_directory ~ '^[A-Za-z0-9_-]+-[0-9a-fA-F-]{36}$');

ALTER TABLE video
    ADD CONSTRAINT video_asset_directory_check
    CHECK (asset_directory IS NULL OR asset_directory ~ '^[A-Za-z0-9_-]+-[0-9a-fA-F-]{36}$');
