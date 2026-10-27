UPDATE docs
SET image_url = REPLACE(image_url, '/api/assets/docs-image/', '/api/assets/docs-cover/')
WHERE image_url LIKE '%/api/assets/docs-image/%';
