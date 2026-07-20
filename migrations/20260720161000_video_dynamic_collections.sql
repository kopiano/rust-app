ALTER TABLE video_collection
    ADD COLUMN IF NOT EXISTS category_slug VARCHAR(80);

ALTER TABLE video_collection_item
    DROP CONSTRAINT IF EXISTS video_collection_item_position_check;

DROP TABLE IF EXISTS video_collection_item;
