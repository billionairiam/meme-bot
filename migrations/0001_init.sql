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
    order_type TEXT NOT NULL CHECK (order_type IN ('site_fee', 'domain_fee')),
    build_mode TEXT CHECK (build_mode IN ('template', 'ai_free')),
    template_id TEXT REFERENCES templates(template_id),
    amount_usdt TEXT NOT NULL CHECK (CAST(amount_usdt AS REAL) > 0),
    payment_chain TEXT CHECK (payment_chain IN ('erc20', 'tron', 'sol')),
    payment_status TEXT NOT NULL DEFAULT 'pending' CHECK (payment_status IN ('pending', 'paid', 'expired', 'failed')),
    site_status TEXT NOT NULL DEFAULT 'draft' CHECK (site_status IN ('draft', 'collecting_assets', 'queued', 'generating', 'deploying', 'live', 'needs_manual_review', 'failed')),
    tx_hash TEXT,
    payment_expires_at TEXT,
    payment_started_at TEXT,
    paid_at TEXT,
    domain_quote_id TEXT,
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
    github_owner TEXT,
    github_repo_name TEXT,
    github_repo_url TEXT,
    github_commit_sha TEXT,
    cloudflare_project_name TEXT,
    cloudflare_project_id TEXT,
    cloudflare_deployment_id TEXT,
    cloudflare_pages_url TEXT,
    custom_domain TEXT,
    generation_provider_used TEXT,
    deployment_status TEXT NOT NULL DEFAULT 'pending' CHECK (deployment_status IN ('pending', 'generated', 'repo_pushed', 'deploying', 'live', 'deployment_failed', 'domain_bind_failed')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE payment_attempts (
    id TEXT PRIMARY KEY,
    order_id TEXT NOT NULL REFERENCES orders(id),
    user_id TEXT NOT NULL REFERENCES users(id),
    chain TEXT NOT NULL CHECK (chain IN ('erc20', 'tron', 'sol')),
    tx_hash TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'valid', 'invalid', 'insufficient_confirmations')),
    failure_reason TEXT,
    observed_amount_usdt TEXT,
    observed_to_address TEXT,
    confirmations INTEGER,
    checked_at TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE payment_transactions (
    id TEXT PRIMARY KEY,
    chain TEXT NOT NULL CHECK (chain IN ('erc20', 'tron', 'sol')),
    tx_hash TEXT NOT NULL,
    order_id TEXT NOT NULL REFERENCES orders(id),
    amount_usdt TEXT NOT NULL,
    to_address TEXT NOT NULL,
    confirmed_at TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (chain, tx_hash)
);

CREATE TABLE generation_jobs (
    id TEXT PRIMARY KEY,
    order_id TEXT NOT NULL UNIQUE REFERENCES orders(id),
    website_id TEXT NOT NULL UNIQUE REFERENCES websites(id),
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'needs_manual_review')),
    priority INTEGER NOT NULL DEFAULT 0,
    queued_at TEXT NOT NULL DEFAULT (datetime('now')),
    started_at TEXT,
    finished_at TEXT,
    locked_by TEXT,
    locked_at TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE generation_attempts (
    id TEXT PRIMARY KEY,
    order_id TEXT NOT NULL REFERENCES orders(id),
    provider_name TEXT NOT NULL,
    attempt_no INTEGER NOT NULL,
    result TEXT NOT NULL CHECK (result IN ('succeeded', 'failed')),
    error_category TEXT,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at TEXT
);

CREATE TABLE domain_quotes (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    website_id TEXT REFERENCES websites(id),
    domain TEXT NOT NULL,
    tld TEXT NOT NULL,
    available INTEGER NOT NULL,
    is_premium INTEGER NOT NULL DEFAULT 0,
    namecheap_price_usd TEXT,
    quoted_amount_usdt TEXT,
    expires_at TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'quoted' CHECK (status IN ('quoted', 'expired', 'paid', 'purchased', 'failed')),
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE domain_registrations (
    id TEXT PRIMARY KEY,
    domain_quote_id TEXT NOT NULL UNIQUE REFERENCES domain_quotes(id),
    namecheap_order_id TEXT,
    namecheap_transaction_id TEXT,
    registration_years INTEGER NOT NULL DEFAULT 1,
    privacy_enabled INTEGER NOT NULL DEFAULT 1,
    cloudflare_zone_id TEXT,
    nameserver_status TEXT NOT NULL DEFAULT 'pending' CHECK (nameserver_status IN ('pending', 'updated', 'failed')),
    registration_status TEXT NOT NULL DEFAULT 'pending' CHECK (registration_status IN ('pending', 'registered', 'failed', 'needs_manual_review')),
    charged_amount_usd TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE deployment_attempts (
    id TEXT PRIMARY KEY,
    website_id TEXT NOT NULL REFERENCES websites(id),
    stage TEXT NOT NULL CHECK (stage IN ('github_create', 'git_push', 'pages_create', 'pages_deploy', 'domain_bind')),
    status TEXT NOT NULL CHECK (status IN ('running', 'succeeded', 'failed')),
    error_message TEXT,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at TEXT
);

CREATE INDEX orders_user_id_idx ON orders(user_id);
CREATE INDEX orders_payment_status_idx ON orders(payment_status);
CREATE UNIQUE INDEX orders_domain_quote_id_unique_idx
    ON orders(domain_quote_id)
    WHERE domain_quote_id IS NOT NULL;
CREATE INDEX generation_jobs_status_queued_at_idx ON generation_jobs(status, queued_at);
CREATE INDEX payment_attempts_order_id_idx ON payment_attempts(order_id);
