PRAGMA foreign_keys = OFF;

DROP INDEX IF EXISTS orders_user_id_idx;
DROP INDEX IF EXISTS orders_payment_status_idx;
DROP INDEX IF EXISTS orders_domain_quote_id_unique_idx;

CREATE TABLE orders_new (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    order_type TEXT NOT NULL CHECK (order_type IN ('site_fee', 'domain_fee')),
    build_mode TEXT CHECK (build_mode IN ('template', 'ai_free')),
    template_id TEXT,
    amount_usdt TEXT NOT NULL CHECK (CAST(amount_usdt AS REAL) > 0),
    payment_chain TEXT CHECK (payment_chain IN ('erc20', 'tron', 'sol')),
    payment_status TEXT NOT NULL DEFAULT 'pending' CHECK (payment_status IN ('pending', 'paid', 'expired', 'failed', 'cancelled')),
    site_status TEXT NOT NULL DEFAULT 'draft' CHECK (site_status IN ('draft', 'collecting_assets', 'queued', 'generating', 'deploying', 'live', 'needs_manual_review', 'failed', 'cancelled')),
    tx_hash TEXT,
    payment_expires_at TEXT,
    payment_started_at TEXT,
    paid_at TEXT,
    domain_quote_id TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO orders_new (
    id, user_id, order_type, build_mode, template_id, amount_usdt,
    payment_chain, payment_status, site_status, tx_hash, payment_expires_at,
    payment_started_at, paid_at, domain_quote_id, created_at, updated_at
)
SELECT
    id, user_id, order_type, build_mode, template_id, amount_usdt,
    payment_chain, payment_status, site_status, tx_hash, payment_expires_at,
    payment_started_at, paid_at, domain_quote_id, created_at, updated_at
FROM orders;

DROP TABLE orders;
ALTER TABLE orders_new RENAME TO orders;

CREATE INDEX orders_user_id_idx ON orders(user_id);
CREATE INDEX orders_payment_status_idx ON orders(payment_status);
CREATE UNIQUE INDEX IF NOT EXISTS orders_domain_quote_id_unique_idx
ON orders(domain_quote_id)
WHERE domain_quote_id IS NOT NULL;

PRAGMA foreign_keys = ON;
