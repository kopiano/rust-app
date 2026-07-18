ALTER TABLE "user"
    ALTER COLUMN subscription_status SET DEFAULT '';

UPDATE "user"
SET subscription_status = ''
WHERE plan = 'free'
  AND subscription_status = 'active'
  AND subscription_start_at IS NULL
  AND subscription_end_at IS NULL;
