PRAGMA foreign_keys = ON;

CREATE TABLE users (
    id TEXT PRIMARY KEY,
    telegram_user_id INTEGER NOT NULL UNIQUE,
    telegram_username TEXT,
    first_name TEXT,
    last_name TEXT,
    is_group_member INTEGER NOT NULL DEFAULT 0,
    last_membership_checked_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE templates (
    id TEXT PRIMARY KEY,
    template_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    local_path TEXT NOT NULL,
    preview_url TEXT,
    price_tier TEXT NOT NULL CHECK (CAST(price_tier AS REAL) > 0),
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE orders (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    status TEXT NOT NULL DEFAULT 'need_logo' CHECK (status IN (
        'need_logo', 'need_ticker', 'need_name', 'need_contract',
        'need_x_link', 'need_tme', 'need_domain', 'need_pay',
        'paid_and_generating', 'need_manual_review', 'done', 'cancelled'
    )),
    build_mode TEXT CHECK (build_mode IN ('template', 'ai_free')),
    template_id TEXT REFERENCES templates(template_id),

    -- wizard asset fields
    token_ticker TEXT,
    token_name TEXT,
    contract_address TEXT,
    x_url TEXT,
    telegram_url TEXT,
    logo_asset_path TEXT,
    requested_domain TEXT,

    -- pricing
    site_price_usdt TEXT NOT NULL DEFAULT '0',
    domain_price_usdt TEXT NOT NULL DEFAULT '0',
    total_usdt TEXT NOT NULL DEFAULT '0',

    -- payment
    payment_chain TEXT CHECK (payment_chain IN ('erc20', 'tron', 'sol')),
    tx_hash TEXT,
    payment_expires_at TEXT,
    paid_at TEXT,

    -- queue / generation
    locked_by TEXT,
    locked_at TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 3,

    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE websites (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    order_id TEXT NOT NULL UNIQUE REFERENCES orders(id),
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

CREATE INDEX orders_user_id_idx ON orders(user_id);
CREATE INDEX orders_status_idx ON orders(status);
CREATE INDEX orders_status_generating_idx ON orders(status, locked_by)
    WHERE status = 'paid_and_generating';
