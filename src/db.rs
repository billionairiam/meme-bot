use chrono::{Duration, Utc};
use rust_decimal::Decimal;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::models::{BuildMode, OrderType, PaymentChain, WebsiteInput};

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
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    let expires_at = (Utc::now() + Duration::minutes(30)).to_rfc3339();
    let row = sqlx::query(
        r#"
        INSERT INTO orders (
            id, user_id, order_type, build_mode, template_id, amount_usdt,
            payment_status, site_status, payment_expires_at
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
    .bind(expires_at)
    .fetch_one(pool)
    .await?;

    Ok(Uuid::parse_str(row.get::<&str, _>("id"))?)
}

pub async fn attach_payment_chain(
    pool: &SqlitePool,
    order_id: Uuid,
    chain: PaymentChain,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE orders SET payment_chain = ?, updated_at = datetime('now') WHERE id = ?")
        .bind(chain.as_str())
        .bind(order_id)
        .execute(pool)
        .await?;
    Ok(())
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
        amount_usdt: Decimal::from_str_exact(r.get::<&str, _>("amount_usdt"))
            .unwrap_or_default(),
        payment_chain: r.get("payment_chain"),
        requested_domain: r.get("requested_domain"),
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
