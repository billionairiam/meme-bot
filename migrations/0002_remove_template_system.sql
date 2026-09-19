CREATE TABLE orders_new (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    status TEXT NOT NULL DEFAULT 'need_chain' CHECK (status IN (
        'need_chain', 'need_logo', 'need_ticker', 'need_name', 'need_contract',
        'need_x_link', 'need_tme', 'need_domain', 'need_pay',
        'paid_and_generating', 'need_manual_review', 'done', 'cancelled'
    )),
    token_chain TEXT CHECK (token_chain IN ('eth', 'bsc', 'sol', 'ton')),
    token_ticker TEXT,
    token_name TEXT,
    contract_address TEXT,
    x_url TEXT,
    telegram_url TEXT,
    logo_asset_path TEXT,
    requested_domain TEXT,
    alternative_domains_json TEXT,
    site_price_usdt TEXT NOT NULL DEFAULT '0',
    domain_price_usdt TEXT NOT NULL DEFAULT '0',
    total_usdt TEXT NOT NULL DEFAULT '0',
    payment_chain TEXT CHECK (payment_chain IN ('erc20', 'tron', 'sol')),
    tx_hash TEXT,
    payment_expires_at TEXT,
    paid_at TEXT,
    pending_contract_address TEXT,
    pending_x_url TEXT,
    pending_telegram_url TEXT,
    pending_edit_field TEXT CHECK (pending_edit_field IN ('contract', 'x', 'telegram')),
    edit_status TEXT NOT NULL DEFAULT 'none' CHECK (edit_status IN (
        'none', 'draft', 'need_pay', 'applying', 'need_manual_review'
    )),
    edit_payment_chain TEXT CHECK (edit_payment_chain IN ('erc20', 'tron', 'sol')),
    edit_tx_hash TEXT,
    edit_payment_expires_at TEXT,
    edit_paid_at TEXT,
    edit_success_count INTEGER NOT NULL DEFAULT 0,
    locked_by TEXT,
    locked_at TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 3,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO orders_new (
    id, user_id, status, token_chain, token_ticker, token_name,
    contract_address, x_url, telegram_url, logo_asset_path, requested_domain,
    alternative_domains_json, site_price_usdt, domain_price_usdt, total_usdt,
    payment_chain, tx_hash, payment_expires_at, paid_at,
    pending_contract_address, pending_x_url, pending_telegram_url,
    pending_edit_field, edit_status, edit_payment_chain, edit_tx_hash,
    edit_payment_expires_at, edit_paid_at, edit_success_count,
    locked_by, locked_at, attempt_count, max_attempts, created_at, updated_at
)
SELECT
    id,
    user_id,
    CASE
        WHEN build_mode = 'template' AND status NOT IN ('done', 'cancelled')
            THEN 'need_manual_review'
        ELSE status
    END,
    token_chain, token_ticker, token_name, contract_address, x_url,
    telegram_url, logo_asset_path, requested_domain, alternative_domains_json,
    site_price_usdt, domain_price_usdt, total_usdt, payment_chain, tx_hash,
    payment_expires_at, paid_at, pending_contract_address, pending_x_url,
    pending_telegram_url, pending_edit_field, edit_status, edit_payment_chain,
    edit_tx_hash, edit_payment_expires_at, edit_paid_at, edit_success_count,
    CASE
        WHEN build_mode = 'template' AND status NOT IN ('done', 'cancelled')
            THEN NULL
        ELSE locked_by
    END,
    CASE
        WHEN build_mode = 'template' AND status NOT IN ('done', 'cancelled')
            THEN NULL
        ELSE locked_at
    END,
    attempt_count, max_attempts, created_at, updated_at
FROM orders;

CREATE TABLE websites_new (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    order_id TEXT NOT NULL UNIQUE REFERENCES orders_new(id),
    token_chain TEXT NOT NULL,
    token_ticker TEXT NOT NULL,
    token_name TEXT NOT NULL,
    contract_address TEXT NOT NULL,
    x_url TEXT NOT NULL,
    telegram_url TEXT NOT NULL,
    logo_asset_path TEXT NOT NULL,
    requested_domain TEXT,
    github_repo_url TEXT,
    cloudflare_pages_url TEXT,
    custom_domain TEXT,
    generation_provider_used TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO websites_new (
    id, user_id, order_id, token_chain, token_ticker, token_name,
    contract_address, x_url, telegram_url, logo_asset_path, requested_domain,
    github_repo_url, cloudflare_pages_url, custom_domain,
    generation_provider_used, created_at, updated_at
)
SELECT
    id, user_id, order_id, token_chain, token_ticker, token_name,
    contract_address, x_url, telegram_url, logo_asset_path, requested_domain,
    github_repo_url, cloudflare_pages_url, custom_domain,
    generation_provider_used, created_at, updated_at
FROM websites;

DROP TABLE websites;
DROP TABLE orders;
ALTER TABLE orders_new RENAME TO orders;
ALTER TABLE websites_new RENAME TO websites;
DROP TABLE templates;

CREATE INDEX orders_user_id_idx ON orders(user_id);
CREATE INDEX orders_status_idx ON orders(status);
CREATE INDEX orders_status_generating_idx ON orders(status, locked_by)
    WHERE status = 'paid_and_generating';
