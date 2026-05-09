use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use rust_decimal::Decimal;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::deployment::DeploymentResult;
use crate::domain::{CloudflareZoneResult, DomainPurchaseResult, DomainQuote};
use crate::models::{BuildMode, OrderType, PaymentChain, WebsiteInput};
use crate::payment::{PaymentVerificationError, PaymentVerificationOutcome};

pub const PAYMENT_WINDOW_MINUTES: i64 = 30;

#[derive(Debug, Clone)]
pub struct PaymentWindow {
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

pub async fn upsert_user(
    pool: &SqlitePool,
    telegram_user_id: i64,
    username: Option<&str>,
    first_name: Option<&str>,
    last_name: Option<&str>,
    is_group_member: bool,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    let row = sqlx::query(
        r#"
        INSERT INTO users (
            id, telegram_user_id, telegram_username, first_name, last_name,
            is_group_member, last_membership_checked_at, updated_at
        )
        VALUES (?, ?, ?, ?, ?, ?, datetime('now'), datetime('now'))
        ON CONFLICT (telegram_user_id) DO UPDATE SET
            telegram_username = EXCLUDED.telegram_username,
            first_name = EXCLUDED.first_name,
            last_name = EXCLUDED.last_name,
            is_group_member = EXCLUDED.is_group_member,
            last_membership_checked_at = datetime('now'),
            updated_at = datetime('now')
        RETURNING id
        "#,
    )
    .bind(id)
    .bind(telegram_user_id)
    .bind(username)
    .bind(first_name)
    .bind(last_name)
    .bind(is_group_member)
    .fetch_one(pool)
    .await?;

    Ok(Uuid::parse_str(row.get::<&str, _>("id"))?)
}

pub async fn create_site_order(
    pool: &SqlitePool,
    user_id: Uuid,
    build_mode: BuildMode,
    template_id: Option<&str>,
    amount_usdt: Decimal,
    domain_quote_id: Option<Uuid>,
) -> anyhow::Result<Uuid> {
    get_or_create_site_order(
        pool,
        user_id,
        build_mode,
        template_id,
        amount_usdt,
        domain_quote_id,
    )
    .await
}

pub async fn get_or_create_site_order(
    pool: &SqlitePool,
    user_id: Uuid,
    build_mode: BuildMode,
    template_id: Option<&str>,
    amount_usdt: Decimal,
    domain_quote_id: Option<Uuid>,
) -> anyhow::Result<Uuid> {
    if let Some(quote_id) = domain_quote_id
        && let Some(existing) = find_order_by_domain_quote(pool, quote_id).await?
    {
        return Ok(existing);
    }

    let id = Uuid::new_v4();
    let result = sqlx::query(
        r#"
        INSERT INTO orders (
            id, user_id, order_type, build_mode, template_id, amount_usdt,
            payment_status, site_status, domain_quote_id
        )
        VALUES (?, ?, ?, ?, ?, ?, 'pending', 'collecting_assets', ?)
        RETURNING id
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(OrderType::SiteFee.as_str())
    .bind(build_mode.as_str())
    .bind(template_id)
    .bind(amount_usdt.to_string())
    .bind(domain_quote_id)
    .fetch_one(pool)
    .await;

    match result {
        Ok(row) => Ok(Uuid::parse_str(row.get::<&str, _>("id"))?),
        Err(error) => {
            if let Some(quote_id) = domain_quote_id
                && let Some(existing) = find_order_by_domain_quote(pool, quote_id).await?
            {
                return Ok(existing);
            }
            Err(error.into())
        }
    }
}

async fn find_order_by_domain_quote(
    pool: &SqlitePool,
    quote_id: Uuid,
) -> anyhow::Result<Option<Uuid>> {
    let row = sqlx::query(
        r#"
        SELECT id
        FROM orders
        WHERE domain_quote_id = ?
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(quote_id)
    .fetch_optional(pool)
    .await?;

    Ok(row
        .map(|row| Uuid::parse_str(row.get::<&str, _>("id")))
        .transpose()?)
}

pub async fn attach_payment_chain(
    pool: &SqlitePool,
    order_id: Uuid,
    chain: PaymentChain,
) -> anyhow::Result<PaymentWindow> {
    let now = Utc::now();
    let expires_at = now + Duration::minutes(PAYMENT_WINDOW_MINUTES);
    let row = sqlx::query(
        r#"
        UPDATE orders
        SET payment_chain = ?,
            payment_started_at = COALESCE(payment_started_at, ?),
            payment_expires_at = COALESCE(payment_expires_at, ?),
            updated_at = datetime('now')
        WHERE id = ?
          AND payment_status = 'pending'
        RETURNING payment_started_at, payment_expires_at
        "#,
    )
    .bind(chain.as_str())
    .bind(now.to_rfc3339())
    .bind(expires_at.to_rfc3339())
    .bind(order_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        anyhow::bail!("order is not pending or does not exist");
    };
    Ok(PaymentWindow {
        started_at: parse_db_datetime(row.get::<&str, _>("payment_started_at"))?,
        expires_at: parse_db_datetime(row.get::<&str, _>("payment_expires_at"))?,
    })
}

pub async fn mark_order_paid(
    pool: &SqlitePool,
    order_id: Uuid,
    chain: PaymentChain,
    tx_hash: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE orders
        SET payment_status = 'paid',
            site_status = CASE
                WHEN order_type = 'site_fee' THEN 'queued'
                ELSE site_status
            END,
            payment_chain = ?,
            tx_hash = ?,
            paid_at = datetime('now'),
            updated_at = datetime('now')
        WHERE id = ? AND payment_status <> 'paid'
        "#,
    )
    .bind(chain.as_str())
    .bind(tx_hash)
    .bind(order_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_order_payment_info(
    pool: &SqlitePool,
    order_id: Uuid,
) -> anyhow::Result<OrderPaymentInfo> {
    let row = sqlx::query(
        r#"
        SELECT id, user_id, amount_usdt, created_at, payment_started_at, payment_expires_at, payment_status
        FROM orders
        WHERE id = ?
        "#,
    )
    .bind(order_id)
    .fetch_one(pool)
    .await?;

    Ok(OrderPaymentInfo {
        id: Uuid::parse_str(row.get::<&str, _>("id"))?,
        user_id: Uuid::parse_str(row.get::<&str, _>("user_id"))?,
        amount_usdt: Decimal::from_str_exact(row.get::<&str, _>("amount_usdt"))?,
        created_at: parse_db_datetime(row.get::<&str, _>("created_at"))?,
        payment_started_at: row
            .get::<Option<String>, _>("payment_started_at")
            .as_deref()
            .map(parse_db_datetime)
            .transpose()?,
        payment_expires_at: row
            .get::<Option<String>, _>("payment_expires_at")
            .as_deref()
            .map(parse_db_datetime)
            .transpose()?,
        payment_status: row.get("payment_status"),
    })
}

pub async fn tx_hash_already_used(
    pool: &SqlitePool,
    chain: PaymentChain,
    tx_hash: &str,
) -> anyhow::Result<bool> {
    let canonical_tx_hash = canonical_tx_hash(chain, tx_hash);
    let row = sqlx::query(
        "SELECT COUNT(*) AS count FROM payment_transactions WHERE chain = ? AND tx_hash = ?",
    )
    .bind(chain.as_str())
    .bind(&canonical_tx_hash)
    .fetch_one(pool)
    .await?;
    Ok(row.get::<i64, _>("count") > 0)
}

pub async fn record_payment_attempt(
    pool: &SqlitePool,
    order_id: Uuid,
    user_id: Uuid,
    chain: PaymentChain,
    tx_hash: &str,
    result: Result<&PaymentVerificationOutcome, &PaymentVerificationError>,
) -> anyhow::Result<()> {
    let id = Uuid::new_v4();
    let (status, failure_reason, observed_amount, observed_to, confirmations) = match result {
        Ok(outcome) => (
            "valid",
            None,
            Some(outcome.observed_amount_usdt.to_string()),
            Some(outcome.observed_to_address.clone()),
            outcome.confirmations.map(|value| value as i64),
        ),
        Err(error) => (
            match error {
                PaymentVerificationError::InsufficientConfirmations => "insufficient_confirmations",
                _ => "invalid",
            },
            Some(error.to_string()),
            None,
            None,
            None,
        ),
    };

    sqlx::query(
        r#"
        INSERT INTO payment_attempts (
            id, order_id, user_id, chain, tx_hash, status, failure_reason,
            observed_amount_usdt, observed_to_address, confirmations, checked_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, datetime('now'))
        "#,
    )
    .bind(id)
    .bind(order_id)
    .bind(user_id)
    .bind(chain.as_str())
    .bind(tx_hash)
    .bind(status)
    .bind(failure_reason)
    .bind(observed_amount)
    .bind(observed_to)
    .bind(confirmations)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn record_payment_transaction(
    pool: &SqlitePool,
    order_id: Uuid,
    chain: PaymentChain,
    tx_hash: &str,
    outcome: &PaymentVerificationOutcome,
) -> anyhow::Result<()> {
    let canonical_tx_hash = canonical_tx_hash(chain, tx_hash);
    sqlx::query(
        r#"
        INSERT INTO payment_transactions (
            id, chain, tx_hash, order_id, amount_usdt, to_address, confirmed_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(chain.as_str())
    .bind(&canonical_tx_hash)
    .bind(order_id)
    .bind(outcome.observed_amount_usdt.to_string())
    .bind(&outcome.observed_to_address)
    .bind(outcome.confirmed_at.to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn confirm_payment_and_mark_paid(
    pool: &SqlitePool,
    order_id: Uuid,
    chain: PaymentChain,
    tx_hash: &str,
    outcome: &PaymentVerificationOutcome,
) -> anyhow::Result<()> {
    let canonical_tx_hash = canonical_tx_hash(chain, tx_hash);
    let mut tx = pool.begin().await?;

    let existing = sqlx::query(
        "SELECT COUNT(*) AS count FROM payment_transactions WHERE chain = ? AND tx_hash = ?",
    )
    .bind(chain.as_str())
    .bind(&canonical_tx_hash)
    .fetch_one(&mut *tx)
    .await?
    .get::<i64, _>("count");
    if existing > 0 {
        anyhow::bail!("payment transaction already used");
    }

    let update_result = sqlx::query(
        r#"
        UPDATE orders
        SET payment_status = 'paid',
            site_status = CASE
                WHEN order_type = 'site_fee' THEN 'queued'
                ELSE site_status
            END,
            payment_chain = ?,
            tx_hash = ?,
            paid_at = datetime('now'),
            updated_at = datetime('now')
        WHERE id = ? AND payment_status = 'pending'
        "#,
    )
    .bind(chain.as_str())
    .bind(&canonical_tx_hash)
    .bind(order_id)
    .execute(&mut *tx)
    .await?;
    if update_result.rows_affected() != 1 {
        anyhow::bail!("order is not pending or does not exist");
    }

    sqlx::query(
        r#"
        INSERT INTO payment_transactions (
            id, chain, tx_hash, order_id, amount_usdt, to_address, confirmed_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(chain.as_str())
    .bind(&canonical_tx_hash)
    .bind(order_id)
    .bind(outcome.observed_amount_usdt.to_string())
    .bind(&outcome.observed_to_address)
    .bind(outcome.confirmed_at.to_rfc3339())
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "UPDATE domain_quotes SET status = 'paid', updated_at = datetime('now') WHERE id = (SELECT domain_quote_id FROM orders WHERE id = ?)",
    )
    .bind(order_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

fn canonical_tx_hash(chain: PaymentChain, tx_hash: &str) -> String {
    let trimmed = tx_hash.trim();
    match chain {
        PaymentChain::Erc20 | PaymentChain::Tron => trimmed.to_ascii_lowercase(),
        PaymentChain::Sol => trimmed.to_string(),
    }
}

pub async fn create_website(
    pool: &SqlitePool,
    user_id: Uuid,
    order_id: Uuid,
    input: &WebsiteInput,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    let row = sqlx::query(
        r#"
        INSERT INTO websites (
            id, user_id, order_id, token_ticker, token_name, contract_address,
            x_url, telegram_url, logo_asset_path, requested_domain
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        RETURNING id
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(order_id)
    .bind(&input.token_ticker)
    .bind(&input.token_name)
    .bind(&input.contract_address)
    .bind(&input.x_url)
    .bind(&input.telegram_url)
    .bind(&input.logo_asset_path)
    .bind(&input.requested_domain)
    .fetch_one(pool)
    .await?;

    Ok(Uuid::parse_str(row.get::<&str, _>("id"))?)
}

pub async fn upsert_website_for_order(
    pool: &SqlitePool,
    user_id: Uuid,
    order_id: Uuid,
    input: &WebsiteInput,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    let row = sqlx::query(
        r#"
        INSERT INTO websites (
            id, user_id, order_id, token_ticker, token_name, contract_address,
            x_url, telegram_url, logo_asset_path, requested_domain
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(order_id) DO UPDATE SET
            token_ticker = EXCLUDED.token_ticker,
            token_name = EXCLUDED.token_name,
            contract_address = EXCLUDED.contract_address,
            x_url = EXCLUDED.x_url,
            telegram_url = EXCLUDED.telegram_url,
            logo_asset_path = EXCLUDED.logo_asset_path,
            requested_domain = EXCLUDED.requested_domain,
            updated_at = datetime('now')
        RETURNING id
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(order_id)
    .bind(&input.token_ticker)
    .bind(&input.token_name)
    .bind(&input.contract_address)
    .bind(&input.x_url)
    .bind(&input.telegram_url)
    .bind(&input.logo_asset_path)
    .bind(&input.requested_domain)
    .fetch_one(pool)
    .await?;

    Ok(Uuid::parse_str(row.get::<&str, _>("id"))?)
}

pub async fn update_website_logo_path(
    pool: &SqlitePool,
    website_id: Uuid,
    logo_asset_path: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE websites SET logo_asset_path = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(logo_asset_path)
    .bind(website_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn record_domain_quote(
    pool: &SqlitePool,
    user_id: Uuid,
    quote: &DomainQuote,
    quoted_amount_usdt: Decimal,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    let expires_at = (Utc::now() + Duration::minutes(30)).to_rfc3339();
    sqlx::query(
        r#"
        INSERT INTO domain_quotes (
            id, user_id, domain, tld, available, is_premium,
            namecheap_price_usd, quoted_amount_usdt, expires_at, status
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'quoted')
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(&quote.domain)
    .bind(&quote.tld)
    .bind(quote.available)
    .bind(quote.is_premium)
    .bind(quote.price_usd.to_string())
    .bind(quoted_amount_usdt.to_string())
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn attach_quote_website(
    pool: &SqlitePool,
    quote_id: Uuid,
    website_id: Uuid,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE domain_quotes SET website_id = ?, updated_at = datetime('now') WHERE id = ?",
    )
    .bind(website_id)
    .bind(quote_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn record_domain_purchase_started(
    pool: &SqlitePool,
    quote_id: Uuid,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO domain_registrations (id, domain_quote_id, registration_status, nameserver_status)
        VALUES (?, ?, 'pending', 'pending')
        ON CONFLICT(domain_quote_id) DO UPDATE SET
            registration_status = 'pending',
            updated_at = datetime('now')
        "#,
    )
    .bind(id)
    .bind(quote_id)
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn record_domain_purchase_success(
    pool: &SqlitePool,
    quote_id: Uuid,
    purchase: &DomainPurchaseResult,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE domain_registrations
        SET namecheap_order_id = ?,
            namecheap_transaction_id = ?,
            charged_amount_usd = ?,
            registration_status = 'registered',
            updated_at = datetime('now')
        WHERE domain_quote_id = ?
        "#,
    )
    .bind(&purchase.namecheap_order_id)
    .bind(&purchase.namecheap_transaction_id)
    .bind(purchase.charged_amount_usd.to_string())
    .bind(quote_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "UPDATE domain_quotes SET status = 'purchased', updated_at = datetime('now') WHERE id = ?",
    )
    .bind(quote_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn record_domain_dns_result(
    pool: &SqlitePool,
    quote_id: Uuid,
    result: Result<&CloudflareZoneResult, &str>,
) -> anyhow::Result<()> {
    match result {
        Ok(zone) => {
            sqlx::query(
                r#"
                UPDATE domain_registrations
                SET cloudflare_zone_id = ?,
                    nameserver_status = 'updated',
                    updated_at = datetime('now')
                WHERE domain_quote_id = ?
                "#,
            )
            .bind(&zone.zone_id)
            .bind(quote_id)
            .execute(pool)
            .await?;
        }
        Err(_) => {
            sqlx::query(
                r#"
                UPDATE domain_registrations
                SET nameserver_status = 'failed',
                    registration_status = 'needs_manual_review',
                    updated_at = datetime('now')
                WHERE domain_quote_id = ?
                "#,
            )
            .bind(quote_id)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

pub async fn mark_domain_purchase_failed(pool: &SqlitePool, quote_id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE domain_registrations
        SET registration_status = 'failed', updated_at = datetime('now')
        WHERE domain_quote_id = ?
        "#,
    )
    .bind(quote_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "UPDATE domain_quotes SET status = 'failed', updated_at = datetime('now') WHERE id = ?",
    )
    .bind(quote_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_domain_purchase_manual_review(
    pool: &SqlitePool,
    quote_id: Uuid,
    reason: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE domain_registrations
        SET registration_status = 'needs_manual_review', updated_at = datetime('now')
        WHERE domain_quote_id = ?
        "#,
    )
    .bind(quote_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "UPDATE domain_quotes SET status = 'failed', updated_at = datetime('now') WHERE id = ?",
    )
    .bind(quote_id)
    .execute(pool)
    .await?;
    tracing::warn!(%quote_id, %reason, "domain purchase requires manual review");
    Ok(())
}

pub async fn mark_order_manual_review(pool: &SqlitePool, order_id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE orders SET site_status = 'needs_manual_review', updated_at = datetime('now') WHERE id = ?",
    )
    .bind(order_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_website_deployment_success(
    pool: &SqlitePool,
    order_id: Uuid,
    deployment: &DeploymentResult,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE websites
        SET github_owner = ?,
            github_repo_name = ?,
            github_repo_url = ?,
            github_commit_sha = ?,
            cloudflare_project_name = ?,
            cloudflare_pages_url = ?,
            custom_domain = ?,
            deployment_status = 'live',
            updated_at = datetime('now')
        WHERE order_id = ?
        "#,
    )
    .bind(&deployment.github_owner)
    .bind(&deployment.github_repo_name)
    .bind(&deployment.github_repo_url)
    .bind(&deployment.github_commit_sha)
    .bind(&deployment.cloudflare_project_name)
    .bind(&deployment.cloudflare_pages_url)
    .bind(&deployment.custom_domain)
    .bind(order_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn record_deployment_attempt(
    pool: &SqlitePool,
    website_id: Uuid,
    stage: &str,
    status: &str,
    error_message: Option<&str>,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO deployment_attempts (
            id, website_id, stage, status, error_message, finished_at
        )
        VALUES (?, ?, ?, ?, ?, CASE WHEN ? <> 'running' THEN datetime('now') ELSE NULL END)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(website_id)
    .bind(stage)
    .bind(status)
    .bind(error_message)
    .bind(status)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn enqueue_generation_job(
    pool: &SqlitePool,
    order_id: Uuid,
    website_id: Uuid,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    let row = sqlx::query(
        r#"
        INSERT INTO generation_jobs (id, order_id, website_id, status)
        VALUES (?, ?, ?, 'queued')
        ON CONFLICT (order_id) DO UPDATE SET order_id = EXCLUDED.order_id
        RETURNING id
        "#,
    )
    .bind(id)
    .bind(order_id)
    .bind(website_id)
    .fetch_one(pool)
    .await?;

    Ok(Uuid::parse_str(row.get::<&str, _>("id"))?)
}

pub async fn queue_position(pool: &SqlitePool, job_id: Uuid) -> anyhow::Result<i64> {
    let row = sqlx::query(
        r#"
        SELECT COUNT(*) AS position
        FROM generation_jobs earlier
        JOIN generation_jobs current ON current.id = ?
        WHERE earlier.status = 'queued'
          AND earlier.queued_at < current.queued_at
        "#,
    )
    .bind(job_id)
    .fetch_one(pool)
    .await?;

    let ahead: i64 = row.get("position");
    Ok(ahead + 1)
}

pub async fn get_user_order(
    pool: &SqlitePool,
    telegram_user_id: i64,
) -> anyhow::Result<Option<UserOrderStatus>> {
    let row = sqlx::query(
        r#"
        SELECT o.payment_status, o.site_status, o.amount_usdt, o.payment_chain,
               w.requested_domain
        FROM orders o
        JOIN users u ON o.user_id = u.id
        LEFT JOIN websites w ON w.order_id = o.id
        WHERE u.telegram_user_id = ?
        ORDER BY o.created_at DESC
        LIMIT 1
        "#,
    )
    .bind(telegram_user_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| UserOrderStatus {
        payment_status: r.get("payment_status"),
        site_status: r.get("site_status"),
        amount_usdt: Decimal::from_str_exact(r.get::<&str, _>("amount_usdt")).unwrap_or_default(),
        payment_chain: r.get("payment_chain"),
        requested_domain: r.get("requested_domain"),
    }))
}

pub async fn get_payment_context_by_order(
    pool: &SqlitePool,
    order_id: Uuid,
) -> anyhow::Result<PaymentOrderContext> {
    let sql = payment_context_select_sql("o.id = ?");
    let row = sqlx::query(&sql).bind(order_id).fetch_one(pool).await?;
    payment_context_from_row(&row)
}

pub async fn get_latest_pending_payment_context_for_user(
    pool: &SqlitePool,
    telegram_user_id: i64,
) -> anyhow::Result<Option<PaymentOrderContext>> {
    let row = sqlx::query(
        r#"
        SELECT o.id AS order_id, o.user_id, o.amount_usdt, o.created_at,
               o.payment_started_at, o.payment_expires_at, o.payment_status,
               o.payment_chain, o.domain_quote_id,
               w.id AS website_id, w.token_ticker, w.token_name, w.contract_address,
               w.x_url, w.telegram_url, w.logo_asset_path, w.requested_domain
        FROM orders o
        JOIN users u ON u.id = o.user_id
        LEFT JOIN websites w ON w.order_id = o.id
        WHERE u.telegram_user_id = ?
          AND o.payment_status = 'pending'
          AND o.payment_chain IS NOT NULL
          AND o.payment_started_at IS NOT NULL
        ORDER BY o.payment_started_at DESC
        LIMIT 1
        "#,
    )
    .bind(telegram_user_id)
    .fetch_optional(pool)
    .await?;

    row.as_ref().map(payment_context_from_row).transpose()
}

pub async fn get_domain_quote_for_purchase(
    pool: &SqlitePool,
    quote_id: Uuid,
) -> anyhow::Result<DomainQuoteForPurchase> {
    let row = sqlx::query(
        r#"
        SELECT domain, namecheap_price_usd, quoted_amount_usdt
        FROM domain_quotes
        WHERE id = ?
        "#,
    )
    .bind(quote_id)
    .fetch_one(pool)
    .await?;

    let namecheap_price_usd = row
        .get::<Option<String>, _>("namecheap_price_usd")
        .ok_or_else(|| anyhow::anyhow!("domain quote missing Namecheap price"))?;
    let quoted_amount_usdt = row
        .get::<Option<String>, _>("quoted_amount_usdt")
        .ok_or_else(|| anyhow::anyhow!("domain quote missing quoted amount"))?;

    Ok(DomainQuoteForPurchase {
        domain: row.get("domain"),
        namecheap_price_usd: Decimal::from_str_exact(&namecheap_price_usd)?,
        quoted_amount_usdt: Decimal::from_str_exact(&quoted_amount_usdt)?,
    })
}

fn payment_context_select_sql(where_clause: &str) -> String {
    format!(
        r#"
        SELECT o.id AS order_id, o.user_id, o.amount_usdt, o.created_at,
               o.payment_started_at, o.payment_expires_at, o.payment_status,
               o.payment_chain, o.domain_quote_id,
               w.id AS website_id, w.token_ticker, w.token_name, w.contract_address,
               w.x_url, w.telegram_url, w.logo_asset_path, w.requested_domain
        FROM orders o
        LEFT JOIN websites w ON w.order_id = o.id
        WHERE {where_clause}
        LIMIT 1
        "#
    )
}

fn payment_context_from_row(row: &sqlx::sqlite::SqliteRow) -> anyhow::Result<PaymentOrderContext> {
    let chain_value = row
        .get::<Option<String>, _>("payment_chain")
        .ok_or_else(|| anyhow::anyhow!("order payment chain is not set"))?;
    let payment_chain = PaymentChain::from_db(&chain_value)
        .ok_or_else(|| anyhow::anyhow!("unsupported payment chain: {chain_value}"))?;
    let website_id = row
        .get::<Option<String>, _>("website_id")
        .as_deref()
        .map(Uuid::parse_str)
        .transpose()?;
    let website_input = if website_id.is_some() {
        Some(WebsiteInput {
            token_ticker: row.get("token_ticker"),
            token_name: row.get("token_name"),
            contract_address: row.get("contract_address"),
            x_url: row.get("x_url"),
            telegram_url: row.get("telegram_url"),
            logo_asset_path: row.get("logo_asset_path"),
            requested_domain: row.get("requested_domain"),
        })
    } else {
        None
    };

    Ok(PaymentOrderContext {
        order_id: Uuid::parse_str(row.get::<&str, _>("order_id"))?,
        user_id: Uuid::parse_str(row.get::<&str, _>("user_id"))?,
        amount_usdt: Decimal::from_str_exact(row.get::<&str, _>("amount_usdt"))?,
        created_at: parse_db_datetime(row.get::<&str, _>("created_at"))?,
        payment_started_at: row
            .get::<Option<String>, _>("payment_started_at")
            .as_deref()
            .map(parse_db_datetime)
            .transpose()?,
        payment_expires_at: row
            .get::<Option<String>, _>("payment_expires_at")
            .as_deref()
            .map(parse_db_datetime)
            .transpose()?,
        payment_status: row.get("payment_status"),
        payment_chain,
        domain_quote_id: row
            .get::<Option<String>, _>("domain_quote_id")
            .as_deref()
            .map(Uuid::parse_str)
            .transpose()?,
        website_id,
        website_input,
    })
}

#[derive(Debug, Clone)]
pub struct UserOrderStatus {
    pub payment_status: String,
    pub site_status: String,
    pub amount_usdt: rust_decimal::Decimal,
    pub payment_chain: Option<String>,
    pub requested_domain: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PaymentOrderContext {
    pub order_id: Uuid,
    pub user_id: Uuid,
    pub amount_usdt: Decimal,
    pub created_at: DateTime<Utc>,
    pub payment_started_at: Option<DateTime<Utc>>,
    pub payment_expires_at: Option<DateTime<Utc>>,
    pub payment_status: String,
    pub payment_chain: PaymentChain,
    pub domain_quote_id: Option<Uuid>,
    pub website_id: Option<Uuid>,
    pub website_input: Option<WebsiteInput>,
}

#[derive(Debug, Clone)]
pub struct DomainQuoteForPurchase {
    pub domain: String,
    pub namecheap_price_usd: Decimal,
    pub quoted_amount_usdt: Decimal,
}

#[derive(Debug, Clone)]
pub struct OrderPaymentInfo {
    pub id: Uuid,
    pub user_id: Uuid,
    pub amount_usdt: Decimal,
    pub created_at: DateTime<Utc>,
    pub payment_started_at: Option<DateTime<Utc>>,
    pub payment_expires_at: Option<DateTime<Utc>>,
    pub payment_status: String,
}

fn parse_db_datetime(value: &str) -> anyhow::Result<DateTime<Utc>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Ok(parsed.with_timezone(&Utc));
    }
    let naive = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")?;
    Ok(DateTime::from_naive_utc_and_offset(naive, Utc))
}
