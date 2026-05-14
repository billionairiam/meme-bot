use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use rust_decimal::Decimal;
use sqlx::{Row, SqlitePool, sqlite::SqliteRow};
use uuid::Uuid;

use crate::deployment::DeploymentResult;
use crate::models::{BuildMode, Order, OrderStatus, PaymentChain, WebsiteInput};

pub const PAYMENT_WINDOW_MINUTES: i64 = 30;
pub const ASSET_COLLECTION_TIMEOUT_MINUTES: i64 = 60;

fn uuid_value(id: Uuid) -> String {
    id.to_string()
}

fn uuid_from_row(row: &SqliteRow, column: &str) -> anyhow::Result<Uuid> {
    if let Ok(value) = row.try_get::<String, _>(column) {
        return Ok(Uuid::parse_str(&value)?);
    }
    let bytes = row.try_get::<Vec<u8>, _>(column)?;
    Ok(Uuid::from_slice(&bytes)?)
}

#[allow(dead_code)]
fn optional_uuid_from_row(row: &SqliteRow, column: &str) -> anyhow::Result<Option<Uuid>> {
    if let Ok(value) = row.try_get::<Option<String>, _>(column) {
        return value
            .as_deref()
            .map(Uuid::parse_str)
            .transpose()
            .map_err(Into::into);
    }
    let bytes = row.try_get::<Option<Vec<u8>>, _>(column)?;
    bytes
        .as_deref()
        .map(Uuid::from_slice)
        .transpose()
        .map_err(Into::into)
}

pub async fn normalize_sqlite_uuid_storage(pool: &SqlitePool) -> anyhow::Result<()> {
    const UUID_COLUMNS: &[(&str, &str)] = &[
        ("websites", "id"),
        ("websites", "user_id"),
        ("websites", "order_id"),
        ("orders", "id"),
        ("orders", "user_id"),
        ("templates", "id"),
        ("users", "id"),
    ];

    let mut conn = pool.acquire().await?;
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await?;

    let result: anyhow::Result<()> = async {
        for (table, column) in UUID_COLUMNS {
            let select_sql = format!(
                "SELECT rowid, {column} AS value FROM {table} WHERE typeof({column}) = 'blob'"
            );
            let rows = sqlx::query(&select_sql).fetch_all(&mut *conn).await?;
            for row in rows {
                let rowid: i64 = row.get("rowid");
                let bytes: Vec<u8> = row.get("value");
                if bytes.len() != 16 {
                    anyhow::bail!(
                        "invalid UUID blob in {table}.{column} rowid={rowid}, len={}",
                        bytes.len()
                    );
                }
                let uuid = Uuid::from_slice(&bytes)?;
                let update_sql = format!("UPDATE {table} SET {column} = ? WHERE rowid = ?");
                sqlx::query(&update_sql)
                    .bind(uuid_value(uuid))
                    .bind(rowid)
                    .execute(&mut *conn)
                    .await?;
            }
        }
        Ok(())
    }
    .await;

    let fk_on_result = sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await;
    fk_on_result?;
    result?;

    let violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut *conn)
        .await?;
    if !violations.is_empty() {
        anyhow::bail!(
            "foreign key check failed after UUID normalization: {} violations",
            violations.len()
        );
    }

    Ok(())
}

// ============================================================
// Users
// ============================================================

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
    .bind(uuid_value(id))
    .bind(telegram_user_id)
    .bind(username)
    .bind(first_name)
    .bind(last_name)
    .bind(is_group_member)
    .fetch_one(pool)
    .await?;

    uuid_from_row(&row, "id")
}

// ============================================================
// Orders
// ============================================================

/// Create a new order in need_logo status. Fails if user already has an
/// unpaid order (need_logo ~ need_pay).
pub async fn create_order(
    pool: &SqlitePool,
    user_id: Uuid,
    build_mode: BuildMode,
    template_id: Option<&str>,
    site_price_usdt: Decimal,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    let row = sqlx::query(
        r#"
        INSERT INTO orders (
            id, user_id, build_mode, template_id, status,
            site_price_usdt, total_usdt
        )
        VALUES (?, ?, ?, ?, 'need_logo', ?, ?)
        RETURNING id
        "#,
    )
    .bind(uuid_value(id))
    .bind(uuid_value(user_id))
    .bind(build_mode.as_str())
    .bind(template_id)
    .bind(site_price_usdt.to_string())
    .bind(site_price_usdt.to_string())
    .fetch_one(pool)
    .await?;

    uuid_from_row(&row, "id")
}

/// Check if user has an unpaid (wizard/payment) order.
pub async fn has_unpaid_order(pool: &SqlitePool, telegram_user_id: i64) -> anyhow::Result<bool> {
    let row = sqlx::query(
        r#"
        SELECT 1
        FROM orders o
        JOIN users u ON o.user_id = u.id
        WHERE u.telegram_user_id = ?
          AND o.status IN ('need_logo', 'need_ticker', 'need_name', 'need_contract',
                           'need_x_link', 'need_tme', 'need_domain', 'need_pay')
        LIMIT 1
        "#,
    )
    .bind(telegram_user_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

/// Get the user's current unpaid order (wizard or payment step).
pub async fn get_unpaid_order(
    pool: &SqlitePool,
    telegram_user_id: i64,
) -> anyhow::Result<Option<Order>> {
    let row = sqlx::query(
        r#"
        SELECT o.*
        FROM orders o
        JOIN users u ON o.user_id = u.id
        WHERE u.telegram_user_id = ?
          AND o.status IN ('need_logo', 'need_ticker', 'need_name', 'need_contract',
                           'need_x_link', 'need_tme', 'need_domain', 'need_pay')
        ORDER BY o.created_at DESC
        LIMIT 1
        "#,
    )
    .bind(telegram_user_id)
    .fetch_optional(pool)
    .await?;

    row.as_ref().map(order_from_row).transpose()
}

/// Get a specific order by ID.
pub async fn get_order(pool: &SqlitePool, order_id: Uuid) -> anyhow::Result<Option<Order>> {
    let row = sqlx::query("SELECT * FROM orders WHERE id = ?")
        .bind(uuid_value(order_id))
        .fetch_optional(pool)
        .await?;

    row.as_ref().map(order_from_row).transpose()
}

/// Update the order status (wizard step transitions).
pub async fn update_order_status(
    pool: &SqlitePool,
    order_id: Uuid,
    status: OrderStatus,
) -> anyhow::Result<()> {
    let result =
        sqlx::query("UPDATE orders SET status = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(status.as_str())
            .bind(uuid_value(order_id))
            .execute(pool)
            .await?;
    if result.rows_affected() != 1 {
        anyhow::bail!("order does not exist: {order_id}");
    }
    Ok(())
}

/// Update a wizard field on the order.
pub async fn update_order_field(
    pool: &SqlitePool,
    order_id: Uuid,
    field: &str,
    value: &str,
) -> anyhow::Result<()> {
    let allowed = [
        "token_ticker",
        "token_name",
        "contract_address",
        "x_url",
        "telegram_url",
        "logo_asset_path",
        "requested_domain",
    ];
    if !allowed.contains(&field) {
        anyhow::bail!("disallowed field: {field}");
    }
    let sql = format!("UPDATE orders SET {field} = ?, updated_at = datetime('now') WHERE id = ?");
    sqlx::query(&sql)
        .bind(value)
        .bind(uuid_value(order_id))
        .execute(pool)
        .await?;
    Ok(())
}

/// Set domain pricing on an order.
pub async fn set_order_domain_pricing(
    pool: &SqlitePool,
    order_id: Uuid,
    domain_price_usdt: Decimal,
    total_usdt: Decimal,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE orders
        SET domain_price_usdt = ?, total_usdt = ?, updated_at = datetime('now')
        WHERE id = ?
        "#,
    )
    .bind(domain_price_usdt.to_string())
    .bind(total_usdt.to_string())
    .bind(uuid_value(order_id))
    .execute(pool)
    .await?;
    Ok(())
}

/// Cancel the user's unpaid order.
pub async fn cancel_order_for_user(
    pool: &SqlitePool,
    telegram_user_id: i64,
    order_id: Option<Uuid>,
) -> anyhow::Result<u64> {
    let mut sql = r#"
        UPDATE orders
        SET status = 'cancelled', updated_at = datetime('now')
        WHERE user_id IN (SELECT id FROM users WHERE telegram_user_id = ?)
          AND status IN ('need_logo', 'need_ticker', 'need_name', 'need_contract',
                         'need_x_link', 'need_tme', 'need_domain', 'need_pay')
    "#
    .to_string();
    if order_id.is_some() {
        sql.push_str(" AND id = ?");
    }
    let mut query = sqlx::query(&sql).bind(telegram_user_id);
    if let Some(oid) = order_id {
        query = query.bind(uuid_value(oid));
    }
    let result = query.execute(pool).await?;
    Ok(result.rows_affected())
}

/// Expire stale pending orders (asset collection timeout or payment window expired).
pub async fn expire_stale_pending_orders_for_user(
    pool: &SqlitePool,
    telegram_user_id: i64,
) -> anyhow::Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE orders
        SET status = 'cancelled', updated_at = datetime('now')
        WHERE user_id IN (SELECT id FROM users WHERE telegram_user_id = ?)
          AND status IN ('need_logo', 'need_ticker', 'need_name', 'need_contract',
                         'need_x_link', 'need_tme', 'need_domain', 'need_pay')
          AND (
            datetime(updated_at) < datetime('now', ?)
            OR (payment_expires_at IS NOT NULL AND datetime(payment_expires_at) < datetime('now'))
          )
        "#,
    )
    .bind(telegram_user_id)
    .bind(format!("-{ASSET_COLLECTION_TIMEOUT_MINUTES} minutes"))
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

// ============================================================
// Payment
// ============================================================

#[derive(Debug, Clone)]
pub struct PaymentWindow {
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Attach payment chain and start payment window.
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
            payment_expires_at = COALESCE(payment_expires_at, ?),
            updated_at = datetime('now')
        WHERE id = ?
          AND status = 'need_pay'
        RETURNING payment_expires_at
        "#,
    )
    .bind(chain.as_str())
    .bind(expires_at.to_rfc3339())
    .bind(uuid_value(order_id))
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        anyhow::bail!("order is not in need_pay status or does not exist");
    };
    Ok(PaymentWindow {
        started_at: now,
        expires_at: parse_db_datetime(row.get::<&str, _>("payment_expires_at"))?,
    })
}

/// Mark order as paid and transition to paid_and_generating.
pub async fn mark_order_paid(
    pool: &SqlitePool,
    order_id: Uuid,
    chain: PaymentChain,
    tx_hash: &str,
) -> anyhow::Result<()> {
    let canonical = canonical_tx_hash(chain, tx_hash);
    let result = sqlx::query(
        r#"
        UPDATE orders
        SET status = 'paid_and_generating',
            payment_chain = ?,
            tx_hash = ?,
            paid_at = datetime('now'),
            updated_at = datetime('now')
        WHERE id = ? AND status = 'need_pay'
        "#,
    )
    .bind(chain.as_str())
    .bind(&canonical)
    .bind(uuid_value(order_id))
    .execute(pool)
    .await?;
    if result.rows_affected() != 1 {
        anyhow::bail!("order is not in need_pay status or does not exist");
    }
    Ok(())
}

/// Check if a TX hash has already been used by any paid order.
pub async fn tx_hash_already_used(
    pool: &SqlitePool,
    chain: PaymentChain,
    tx_hash: &str,
) -> anyhow::Result<bool> {
    let canonical = canonical_tx_hash(chain, tx_hash);
    let row = sqlx::query(
        "SELECT COUNT(*) AS count FROM orders WHERE tx_hash = ? AND status NOT IN ('cancelled')",
    )
    .bind(&canonical)
    .fetch_one(pool)
    .await?;
    Ok(row.get::<i64, _>("count") > 0)
}

fn canonical_tx_hash(chain: PaymentChain, tx_hash: &str) -> String {
    let trimmed = tx_hash.trim();
    match chain {
        PaymentChain::Erc20 | PaymentChain::Tron => trimmed.to_ascii_lowercase(),
        PaymentChain::Sol => trimmed.to_string(),
    }
}

// ============================================================
// Order status query (for /status)
// ============================================================

/// Get all active orders for a user (not done/cancelled).
pub async fn get_active_orders(
    pool: &SqlitePool,
    telegram_user_id: i64,
) -> anyhow::Result<Vec<Order>> {
    let rows = sqlx::query(
        r#"
        SELECT o.*
        FROM orders o
        JOIN users u ON o.user_id = u.id
        WHERE u.telegram_user_id = ?
          AND o.status NOT IN ('done', 'cancelled')
        ORDER BY o.created_at DESC
        "#,
    )
    .bind(telegram_user_id)
    .fetch_all(pool)
    .await?;

    rows.iter().map(order_from_row).collect()
}

/// Get completed orders for /history.
pub async fn get_completed_orders(
    pool: &SqlitePool,
    telegram_user_id: i64,
    limit: i64,
) -> anyhow::Result<Vec<OrderWithWebsite>> {
    let rows = sqlx::query(
        r#"
        SELECT o.id AS order_id, o.token_ticker, o.token_name, o.requested_domain,
               o.total_usdt, o.payment_chain, o.paid_at,
               w.cloudflare_pages_url, w.custom_domain, w.github_repo_url
        FROM orders o
        JOIN users u ON o.user_id = u.id
        LEFT JOIN websites w ON w.order_id = o.id
        WHERE u.telegram_user_id = ?
          AND o.status = 'done'
        ORDER BY o.paid_at DESC
        LIMIT ?
        "#,
    )
    .bind(telegram_user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    rows.iter()
        .map(|row| {
            Ok(OrderWithWebsite {
                order_id: uuid_from_row(row, "order_id")?,
                token_ticker: row.get("token_ticker"),
                token_name: row.get("token_name"),
                requested_domain: row.get("requested_domain"),
                total_usdt: Decimal::from_str_exact(row.get::<&str, _>("total_usdt"))?,
                payment_chain: row.get("payment_chain"),
                paid_at: row
                    .get::<Option<String>, _>("paid_at")
                    .as_deref()
                    .map(parse_db_datetime)
                    .transpose()?,
                cloudflare_pages_url: row.get("cloudflare_pages_url"),
                custom_domain: row.get("custom_domain"),
                github_repo_url: row.get("github_repo_url"),
            })
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct OrderWithWebsite {
    pub order_id: Uuid,
    pub token_ticker: Option<String>,
    pub token_name: Option<String>,
    pub requested_domain: Option<String>,
    pub total_usdt: Decimal,
    pub payment_chain: Option<String>,
    pub paid_at: Option<DateTime<Utc>>,
    pub cloudflare_pages_url: Option<String>,
    pub custom_domain: Option<String>,
    pub github_repo_url: Option<String>,
}

// ============================================================
// Generation queue (operates directly on orders table)
// ============================================================

/// Lock the next `paid_and_generating` order for generation processing.
/// Returns the order with all fields needed for generation.
pub async fn lock_next_generation_order(
    pool: &SqlitePool,
    worker_id: &str,
    max_attempts: i64,
) -> anyhow::Result<Option<Order>> {
    let row = sqlx::query(
        r#"
        SELECT *
        FROM orders
        WHERE status = 'paid_and_generating'
          AND (locked_by IS NULL OR locked_by = ?)
          AND attempt_count < ?
        ORDER BY created_at ASC
        LIMIT 1
        "#,
    )
    .bind(worker_id)
    .bind(max_attempts)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let order = order_from_row(&row)?;

    let update = sqlx::query(
        r#"
        UPDATE orders
        SET locked_by = ?,
            locked_at = datetime('now'),
            attempt_count = attempt_count + 1,
            updated_at = datetime('now')
        WHERE id = ?
          AND status = 'paid_and_generating'
          AND (locked_by IS NULL OR locked_by = ?)
        "#,
    )
    .bind(worker_id)
    .bind(uuid_value(order.id))
    .bind(worker_id)
    .execute(pool)
    .await?;

    if update.rows_affected() != 1 {
        return Ok(None);
    }

    // Re-read to get updated attempt_count
    get_order(pool, order.id).await
}

/// Reclaim stale running orders that have been locked too long.
pub async fn reclaim_stale_locked_orders(
    pool: &SqlitePool,
    stale_minutes: i64,
    max_attempts: i64,
) -> anyhow::Result<Vec<(Uuid, bool)>> {
    // Requeue orders that still have retries left
    let requeue_rows = sqlx::query(
        r#"
        UPDATE orders
        SET locked_by = NULL,
            locked_at = NULL,
            updated_at = datetime('now')
        WHERE status = 'paid_and_generating'
          AND locked_at IS NOT NULL
          AND locked_at < datetime('now', ?)
          AND attempt_count < ?
        RETURNING id
        "#,
    )
    .bind(format!("-{stale_minutes} minutes"))
    .bind(max_attempts)
    .fetch_all(pool)
    .await?;

    let mut results: Vec<(Uuid, bool)> = Vec::new();
    for row in &requeue_rows {
        results.push((uuid_from_row(row, "id")?, false));
    }

    // Mark as need_manual_review if max attempts reached
    let review_rows = sqlx::query(
        r#"
        UPDATE orders
        SET status = 'need_manual_review',
            locked_by = NULL,
            locked_at = NULL,
            updated_at = datetime('now')
        WHERE status = 'paid_and_generating'
          AND locked_at IS NOT NULL
          AND locked_at < datetime('now', ?)
          AND attempt_count >= ?
        RETURNING id
        "#,
    )
    .bind(format!("-{stale_minutes} minutes"))
    .bind(max_attempts)
    .fetch_all(pool)
    .await?;

    for row in &review_rows {
        results.push((uuid_from_row(row, "id")?, true));
    }

    Ok(results)
}

/// Mark order generation as succeeded — unlock and prepare for deployment.
pub async fn mark_generation_succeeded(pool: &SqlitePool, order_id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE orders
        SET locked_by = NULL,
            locked_at = NULL,
            updated_at = datetime('now')
        WHERE id = ?
        "#,
    )
    .bind(uuid_value(order_id))
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark order as done (generation + deployment complete).
pub async fn mark_order_done(pool: &SqlitePool, order_id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE orders
        SET status = 'done',
            locked_by = NULL,
            locked_at = NULL,
            updated_at = datetime('now')
        WHERE id = ?
        "#,
    )
    .bind(uuid_value(order_id))
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark order as needing manual review (generation or deployment failed).
pub async fn mark_order_manual_review(pool: &SqlitePool, order_id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE orders
        SET status = 'need_manual_review',
            locked_by = NULL,
            locked_at = NULL,
            updated_at = datetime('now')
        WHERE id = ?
        "#,
    )
    .bind(uuid_value(order_id))
    .execute(pool)
    .await?;
    Ok(())
}

/// Requeue a failed order for retry (unlock, keep status as paid_and_generating).
pub async fn requeue_order_for_retry(pool: &SqlitePool, order_id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE orders
        SET locked_by = NULL,
            locked_at = NULL,
            updated_at = datetime('now')
        WHERE id = ? AND status = 'paid_and_generating'
        "#,
    )
    .bind(uuid_value(order_id))
    .execute(pool)
    .await?;
    Ok(())
}

/// Reset stuck paid_and_generating orders on startup (clear locks so they get retried).
pub async fn reset_generating_orders_on_startup(pool: &SqlitePool) -> anyhow::Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE orders
        SET locked_by = NULL,
            locked_at = NULL,
            updated_at = datetime('now')
        WHERE status = 'paid_and_generating'
          AND locked_by IS NOT NULL
        "#,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Get queue position for a paid_and_generating order.
pub async fn queue_position(pool: &SqlitePool, order_id: Uuid) -> anyhow::Result<i64> {
    let current = sqlx::query(
        "SELECT created_at FROM orders WHERE id = ? AND status = 'paid_and_generating'",
    )
    .bind(uuid_value(order_id))
    .fetch_optional(pool)
    .await?;

    let Some(current) = current else {
        anyhow::bail!("order is not in paid_and_generating status");
    };
    let created_at: String = current.get("created_at");

    let row = sqlx::query(
        r#"
        SELECT COUNT(*) AS ahead
        FROM orders
        WHERE status = 'paid_and_generating'
          AND locked_by IS NULL
          AND created_at < ?
        "#,
    )
    .bind(created_at)
    .fetch_one(pool)
    .await?;

    Ok(row.get::<i64, _>("ahead") + 1)
}

// ============================================================
// Websites
// ============================================================

/// Create a website record when deployment succeeds.
pub async fn create_website(
    pool: &SqlitePool,
    user_id: Uuid,
    order_id: Uuid,
    input: &WebsiteInput,
    generation_provider: Option<&str>,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    let row = sqlx::query(
        r#"
        INSERT INTO websites (
            id, user_id, order_id, token_ticker, token_name, contract_address,
            x_url, telegram_url, logo_asset_path, requested_domain,
            generation_provider_used
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(order_id) DO UPDATE SET
            generation_provider_used = EXCLUDED.generation_provider_used,
            updated_at = datetime('now')
        RETURNING id
        "#,
    )
    .bind(uuid_value(id))
    .bind(uuid_value(user_id))
    .bind(uuid_value(order_id))
    .bind(&input.token_ticker)
    .bind(&input.token_name)
    .bind(&input.contract_address)
    .bind(&input.x_url)
    .bind(&input.telegram_url)
    .bind(&input.logo_asset_path)
    .bind(&input.requested_domain)
    .bind(generation_provider)
    .fetch_one(pool)
    .await?;

    uuid_from_row(&row, "id")
}

/// Update website with deployment results and mark order as done.
pub async fn update_website_deployment_success(
    pool: &SqlitePool,
    order_id: Uuid,
    deployment: &DeploymentResult,
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        r#"
        UPDATE websites
        SET github_repo_url = ?,
            cloudflare_pages_url = ?,
            custom_domain = ?,
            updated_at = datetime('now')
        WHERE order_id = ?
        "#,
    )
    .bind(&deployment.github_repo_url)
    .bind(&deployment.cloudflare_pages_url)
    .bind(&deployment.custom_domain)
    .bind(uuid_value(order_id))
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"
        UPDATE orders
        SET status = 'done',
            locked_by = NULL,
            locked_at = NULL,
            updated_at = datetime('now')
        WHERE id = ?
        "#,
    )
    .bind(uuid_value(order_id))
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

// ============================================================
// Helpers
// ============================================================

fn order_from_row(row: &SqliteRow) -> anyhow::Result<Order> {
    let status_str: String = row.get("status");
    let status = OrderStatus::from_db(&status_str)
        .ok_or_else(|| anyhow::anyhow!("unknown order status: {status_str}"))?;

    let build_mode = row
        .get::<Option<String>, _>("build_mode")
        .as_deref()
        .and_then(BuildMode::from_db);

    Ok(Order {
        id: uuid_from_row(row, "id")?,
        user_id: uuid_from_row(row, "user_id")?,
        status,
        build_mode,
        template_id: row.get("template_id"),
        token_ticker: row.get("token_ticker"),
        token_name: row.get("token_name"),
        contract_address: row.get("contract_address"),
        x_url: row.get("x_url"),
        telegram_url: row.get("telegram_url"),
        logo_asset_path: row.get("logo_asset_path"),
        requested_domain: row.get("requested_domain"),
        site_price_usdt: parse_decimal_or_zero(row.get::<&str, _>("site_price_usdt")),
        domain_price_usdt: parse_decimal_or_zero(row.get::<&str, _>("domain_price_usdt")),
        total_usdt: parse_decimal_or_zero(row.get::<&str, _>("total_usdt")),
        payment_chain: row
            .get::<Option<String>, _>("payment_chain")
            .as_deref()
            .and_then(PaymentChain::from_db),
        tx_hash: row.get("tx_hash"),
        payment_expires_at: row
            .get::<Option<String>, _>("payment_expires_at")
            .as_deref()
            .map(parse_db_datetime)
            .transpose()?,
        paid_at: row
            .get::<Option<String>, _>("paid_at")
            .as_deref()
            .map(parse_db_datetime)
            .transpose()?,
        attempt_count: row.get("attempt_count"),
        created_at: parse_db_datetime(row.get::<&str, _>("created_at"))?,
        updated_at: parse_db_datetime(row.get::<&str, _>("updated_at"))?,
    })
}

fn parse_db_datetime(value: &str) -> anyhow::Result<DateTime<Utc>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Ok(parsed.with_timezone(&Utc));
    }
    let naive = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")?;
    Ok(DateTime::from_naive_utc_and_offset(naive, Utc))
}

fn parse_decimal_or_zero(value: &str) -> Decimal {
    Decimal::from_str_exact(value).unwrap_or_default()
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn normalizes_legacy_blob_user_ids_before_upsert() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        let id = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO users (
                id, telegram_user_id, is_group_member, last_membership_checked_at
            )
            VALUES (?, ?, 0, datetime('now'))
            "#,
        )
        .bind(id)
        .bind(12345_i64)
        .execute(&pool)
        .await
        .unwrap();

        normalize_sqlite_uuid_storage(&pool).await.unwrap();
        let returned_id = upsert_user(&pool, 12345, Some("ma"), None, None, true)
            .await
            .unwrap();
        let row = sqlx::query("SELECT typeof(id) AS id_type FROM users WHERE telegram_user_id = ?")
            .bind(12345_i64)
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(returned_id, id);
        assert_eq!(row.get::<String, _>("id_type"), "text");
    }

    #[tokio::test]
    async fn create_and_cancel_order() {
        let pool = test_pool().await;
        let user_id = upsert_user(&pool, 101, Some("alice"), None, None, true)
            .await
            .unwrap();
        let order_id = create_order(&pool, user_id, BuildMode::AiFree, None, Decimal::new(50, 0))
            .await
            .unwrap();

        assert!(has_unpaid_order(&pool, 101).await.unwrap());

        let cancelled = cancel_order_for_user(&pool, 101, Some(order_id))
            .await
            .unwrap();
        assert_eq!(cancelled, 1);
        assert!(!has_unpaid_order(&pool, 101).await.unwrap());

        let active = get_active_orders(&pool, 101).await.unwrap();
        assert!(active.is_empty());
    }

    #[tokio::test]
    async fn order_wizard_flow() {
        let pool = test_pool().await;
        let user_id = upsert_user(&pool, 201, Some("bob"), None, None, true)
            .await
            .unwrap();
        let order_id = create_order(&pool, user_id, BuildMode::AiFree, None, Decimal::new(50, 0))
            .await
            .unwrap();

        // Simulate wizard steps
        update_order_field(&pool, order_id, "logo_asset_path", "/tmp/logo.png")
            .await
            .unwrap();
        update_order_status(&pool, order_id, OrderStatus::NeedTicker)
            .await
            .unwrap();
        update_order_field(&pool, order_id, "token_ticker", "PEPE")
            .await
            .unwrap();
        update_order_status(&pool, order_id, OrderStatus::NeedPay)
            .await
            .unwrap();

        let order = get_order(&pool, order_id).await.unwrap().unwrap();
        assert_eq!(order.status, OrderStatus::NeedPay);
        assert_eq!(order.logo_asset_path.as_deref(), Some("/tmp/logo.png"));
        assert_eq!(order.token_ticker.as_deref(), Some("PEPE"));
    }

    #[tokio::test]
    async fn paid_order_becomes_generating() {
        let pool = test_pool().await;
        let user_id = upsert_user(&pool, 301, Some("charlie"), None, None, true)
            .await
            .unwrap();
        let order_id = create_order(&pool, user_id, BuildMode::AiFree, None, Decimal::new(50, 0))
            .await
            .unwrap();
        update_order_status(&pool, order_id, OrderStatus::NeedPay)
            .await
            .unwrap();

        mark_order_paid(&pool, order_id, PaymentChain::Tron, "tx301")
            .await
            .unwrap();

        let order = get_order(&pool, order_id).await.unwrap().unwrap();
        assert_eq!(order.status, OrderStatus::PaidAndGenerating);

        // User can now create a new order
        assert!(!has_unpaid_order(&pool, 301).await.unwrap());
    }

    #[tokio::test]
    async fn completed_orders_show_in_history() {
        let pool = test_pool().await;
        let user_id = upsert_user(&pool, 401, Some("dave"), None, None, true)
            .await
            .unwrap();
        let order_id = create_order(&pool, user_id, BuildMode::AiFree, None, Decimal::new(50, 0))
            .await
            .unwrap();
        update_order_status(&pool, order_id, OrderStatus::NeedPay)
            .await
            .unwrap();
        mark_order_paid(&pool, order_id, PaymentChain::Erc20, "tx401")
            .await
            .unwrap();
        mark_order_done(&pool, order_id).await.unwrap();

        let active = get_active_orders(&pool, 401).await.unwrap();
        assert!(active.is_empty());

        let history = get_completed_orders(&pool, 401, 10).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].order_id, order_id);
    }
}
