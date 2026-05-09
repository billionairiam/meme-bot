-- No schema change is required for order cancellation.
-- Cancellations reuse existing states:
--   orders.payment_status = 'expired'
--   orders.site_status = 'failed'
SELECT 1;
