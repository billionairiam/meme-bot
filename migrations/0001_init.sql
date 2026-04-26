CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    telegram_user_id BIGINT NOT NULL UNIQUE,
    telegram_username TEXT,
    first_name TEXT,
    last_name TEXT,
    is_group_member BOOLEAN NOT NULL DEFAULT FALSE,
    last_membership_checked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE templates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    local_path TEXT NOT NULL,
    preview_url TEXT,
    price_tier NUMERIC(12, 2) NOT NULL CHECK (price_tier IN (30, 40, 50, 60)),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id),
    order_type TEXT NOT NULL CHECK (order_type IN ('site_fee', 'domain_fee')),
    build_mode TEXT CHECK (build_mode IN ('template', 'ai_free')),
    template_id TEXT,
    amount_usdt NUMERIC(18, 6) NOT NULL CHECK (amount_usdt > 0),
    payment_chain TEXT CHECK (payment_chain IN ('erc20', 'tron', 'sol')),
    payment_status TEXT NOT NULL DEFAULT 'pending' CHECK (payment_status IN ('pending', 'paid', 'expired', 'failed')),
    site_status TEXT NOT NULL DEFAULT 'draft' CHECK (site_status IN ('draft', 'collecting_assets', 'queued', 'generating', 'deploying', 'live', 'needs_manual_review', 'failed')),
    tx_hash TEXT,
    payment_expires_at TIMESTAMPTZ,
    paid_at TIMESTAMPTZ,
    domain_quote_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE websites (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id),
    order_id UUID NOT NULL UNIQUE REFERENCES orders(id),
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
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE payment_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id UUID NOT NULL REFERENCES orders(id),
    user_id UUID NOT NULL REFERENCES users(id),
    chain TEXT NOT NULL CHECK (chain IN ('erc20', 'tron', 'sol')),
    tx_hash TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'valid', 'invalid', 'insufficient_confirmations')),
    failure_reason TEXT,
    observed_amount_usdt NUMERIC(18, 6),
    observed_to_address TEXT,
    confirmations BIGINT,
    checked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE payment_transactions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    chain TEXT NOT NULL CHECK (chain IN ('erc20', 'tron', 'sol')),
    tx_hash TEXT NOT NULL,
    order_id UUID NOT NULL REFERENCES orders(id),
    amount_usdt NUMERIC(18, 6) NOT NULL,
    to_address TEXT NOT NULL,
    confirmed_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (chain, tx_hash)
);

CREATE TABLE generation_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id UUID NOT NULL UNIQUE REFERENCES orders(id),
    website_id UUID NOT NULL UNIQUE REFERENCES websites(id),
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'needs_manual_review')),
    priority INTEGER NOT NULL DEFAULT 0,
    queued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    locked_by TEXT,
    locked_at TIMESTAMPTZ,
    attempt_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE generation_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    site_order_id UUID NOT NULL REFERENCES orders(id),
    provider_name TEXT NOT NULL,
    attempt_no INTEGER NOT NULL,
    result TEXT NOT NULL CHECK (result IN ('succeeded', 'failed')),
    error_category TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ
);

CREATE TABLE domain_quotes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id),
    website_id UUID REFERENCES websites(id),
    domain TEXT NOT NULL,
    tld TEXT NOT NULL,
    available BOOLEAN NOT NULL,
    is_premium BOOLEAN NOT NULL DEFAULT FALSE,
    namecheap_price_usd NUMERIC(18, 6),
    quoted_amount_usdt NUMERIC(18, 6),
    expires_at TIMESTAMPTZ NOT NULL,
    status TEXT NOT NULL DEFAULT 'quoted' CHECK (status IN ('quoted', 'expired', 'paid', 'purchased', 'failed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE orders
    ADD CONSTRAINT orders_domain_quote_fk
    FOREIGN KEY (domain_quote_id) REFERENCES domain_quotes(id);

CREATE TABLE domain_registrations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    domain_quote_id UUID NOT NULL UNIQUE REFERENCES domain_quotes(id),
    namecheap_order_id TEXT,
    namecheap_transaction_id TEXT,
    registration_years INTEGER NOT NULL DEFAULT 1,
    privacy_enabled BOOLEAN NOT NULL DEFAULT TRUE,
    cloudflare_zone_id TEXT,
    nameserver_status TEXT NOT NULL DEFAULT 'pending' CHECK (nameserver_status IN ('pending', 'updated', 'failed')),
    registration_status TEXT NOT NULL DEFAULT 'pending' CHECK (registration_status IN ('pending', 'registered', 'failed', 'needs_manual_review')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE deployment_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    website_id UUID NOT NULL REFERENCES websites(id),
    stage TEXT NOT NULL CHECK (stage IN ('github_create', 'git_push', 'pages_create', 'pages_deploy', 'domain_bind')),
    status TEXT NOT NULL CHECK (status IN ('running', 'succeeded', 'failed')),
    error_message TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ
);

CREATE INDEX orders_user_id_idx ON orders(user_id);
CREATE INDEX orders_payment_status_idx ON orders(payment_status);
CREATE INDEX generation_jobs_status_queued_at_idx ON generation_jobs(status, queued_at);
CREATE INDEX payment_attempts_order_id_idx ON payment_attempts(order_id);
