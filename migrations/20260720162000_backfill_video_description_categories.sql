INSERT INTO video_category (slug, name_zh, name_en)
SELECT DISTINCT
    lower(tag_match[1]),
    lower(tag_match[1]),
    lower(tag_match[1])
FROM video
CROSS JOIN LATERAL regexp_matches(
    COALESCE(video.description, ''),
    '#([^[:space:]#]+)',
    'g'
) AS tag_match
WHERE tag_match[1] <> ''
ON CONFLICT (slug) DO NOTHING;

WITH description_tags AS (
    SELECT DISTINCT
        video.id AS video_id,
        lower(tag_match[1]) AS slug
    FROM video
    CROSS JOIN LATERAL regexp_matches(
        COALESCE(video.description, ''),
        '#([^[:space:]#]+)',
        'g'
    ) AS tag_match
    WHERE tag_match[1] <> ''
)
INSERT INTO video_category_map (video_id, category_id)
SELECT description_tags.video_id, video_category.id
FROM description_tags
INNER JOIN video_category ON video_category.slug = description_tags.slug
ON CONFLICT DO NOTHING;
