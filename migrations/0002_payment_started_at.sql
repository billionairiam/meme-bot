ALTER TABLE orders ADD COLUMN payment_started_at TEXT;

UPDATE orders
SET payment_started_at = datetime(payment_expires_at, '-30 minutes')
WHERE payment_started_at IS NULL
  AND payment_expires_at IS NOT NULL;
