ALTER TABLE domain_registrations ADD COLUMN charged_amount_usd TEXT;

WITH ranked_orders AS (
    SELECT
        id,
        ROW_NUMBER() OVER (
            PARTITION BY domain_quote_id
            ORDER BY
                CASE WHEN payment_status = 'paid' THEN 0 ELSE 1 END,
                created_at ASC
        ) AS rank
    FROM orders
    WHERE domain_quote_id IS NOT NULL
)
UPDATE orders
SET domain_quote_id = NULL,
    site_status = CASE
        WHEN payment_status = 'paid' THEN 'needs_manual_review'
        ELSE site_status
    END,
    updated_at = datetime('now')
WHERE id IN (SELECT id FROM ranked_orders WHERE rank > 1);

CREATE UNIQUE INDEX IF NOT EXISTS orders_domain_quote_id_unique_idx
ON orders(domain_quote_id)
WHERE domain_quote_id IS NOT NULL;
