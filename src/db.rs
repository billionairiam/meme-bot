use chrono::{Duration, Utc};
use rust_decimal::Decimal;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::models::{BuildMode, OrderType, PaymentChain, WebsiteInput};

pub async fn upsert_user(
    pool: &PgPool,
    telegram_user_id: i64,
    username: Option<&str>,
    first_name: Option<&str>,
    last_name: Option<&str>,
    is_group_member: bool,
) -> anyhow::Result<Uuid> {
    let row = sqlx::query(
        r#"
        INSERT INTO users (
            telegram_user_id, telegram_username, first_name, last_name,
            is_group_member, last_membership_checked_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, now(), now())
        ON CONFLICT (telegram_user_id) DO UPDATE SET
            telegram_username = EXCLUDED.telegram_username,
            first_name = EXCLUDED.first_name,
            last_name = EXCLUDED.last_name,
            is_group_member = EXCLUDED.is_group_member,
            last_membership_checked_at = now(),
            updated_at = now()
        RETURNING id
        "#,
    )
    .bind(telegram_user_id)
    .bind(username)
    .bind(first_name)
    .bind(last_name)
    .bind(is_group_member)
    .fetch_one(pool)
    .await?;

    Ok(row.get("id"))
}

pub async fn create_site_order(
    pool: &PgPool,
    user_id: Uuid,
    build_mode: BuildMode,
    template_id: Option<&str>,
    amount_usdt: Decimal,
) -> anyhow::Result<Uuid> {
    let expires_at = Utc::now() + Duration::minutes(30);
    let row = sqlx::query(
        r#"
        INSERT INTO orders (
            user_id, order_type, build_mode, template_id, amount_usdt,
            payment_status, site_status, payment_expires_at
        )
        VALUES ($1, $2, $3, $4, $5, 'pending', 'collecting_assets', $6)
        RETURNING id
        "#,
    )
    .bind(user_id)
    .bind(OrderType::SiteFee.as_str())
    .bind(build_mode.as_str())
    .bind(template_id)
    .bind(amount_usdt)
    .bind(expires_at)
    .fetch_one(pool)
    .await?;

    Ok(row.get("id"))
}

pub async fn attach_payment_chain(
    pool: &PgPool,
    order_id: Uuid,
    chain: PaymentChain,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE orders SET payment_chain = $1, updated_at = now() WHERE id = $2")
        .bind(chain.as_str())
        .bind(order_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn mark_order_paid(
    pool: &PgPool,
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
            payment_chain = $1,
            tx_hash = $2,
            paid_at = now(),
            updated_at = now()
        WHERE id = $3 AND payment_status <> 'paid'
        "#,
    )
    .bind(chain.as_str())
    .bind(tx_hash)
    .bind(order_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn create_website(
    pool: &PgPool,
    user_id: Uuid,
    order_id: Uuid,
    input: &WebsiteInput,
) -> anyhow::Result<Uuid> {
    let row = sqlx::query(
        r#"
        INSERT INTO websites (
            user_id, order_id, token_ticker, token_name, contract_address,
            x_url, telegram_url, logo_asset_path, requested_domain
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        RETURNING id
        "#,
    )
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

    Ok(row.get("id"))
}

pub async fn enqueue_generation_job(
    pool: &PgPool,
    order_id: Uuid,
    website_id: Uuid,
) -> anyhow::Result<Uuid> {
    let row = sqlx::query(
        r#"
        INSERT INTO generation_jobs (order_id, website_id, status)
        VALUES ($1, $2, 'queued')
        ON CONFLICT (order_id) DO UPDATE SET order_id = EXCLUDED.order_id
        RETURNING id
        "#,
    )
    .bind(order_id)
    .bind(website_id)
    .fetch_one(pool)
    .await?;

    Ok(row.get("id"))
}

pub async fn queue_position(pool: &PgPool, job_id: Uuid) -> anyhow::Result<i64> {
    let row = sqlx::query(
        r#"
        SELECT COUNT(*)::BIGINT AS position
        FROM generation_jobs earlier
        JOIN generation_jobs current ON current.id = $1
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
    pool: &PgPool,
    telegram_user_id: i64,
) -> anyhow::Result<Option<UserOrderStatus>> {
    let row = sqlx::query(
        r#"
        SELECT o.payment_status, o.site_status, o.amount_usdt, o.payment_chain,
               w.requested_domain
        FROM orders o
        JOIN users u ON o.user_id = u.id
        LEFT JOIN websites w ON w.order_id = o.id
        WHERE u.telegram_user_id = $1
        ORDER BY o.created_at DESC
        LIMIT 1
        "#,
    )
    .bind(telegram_user_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| {
        use sqlx::Row;
        UserOrderStatus {
            payment_status: r.get("payment_status"),
            site_status: r.get("site_status"),
            amount_usdt: r.get("amount_usdt"),
            payment_chain: r.get("payment_chain"),
            requested_domain: r.get("requested_domain"),
        }
    }))
}

#[derive(Debug, Clone)]
pub struct UserOrderStatus {
    pub payment_status: String,
    pub site_status: String,
    pub amount_usdt: rust_decimal::Decimal,
    pub payment_chain: Option<String>,
    pub requested_domain: Option<String>,
}
