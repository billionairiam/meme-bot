use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use rust_decimal::Decimal;
use sqlx::{Row, SqlitePool, sqlite::SqliteRow};
use uuid::Uuid;

use crate::deployment::DeploymentResult;
use crate::models::{Order, OrderStatus, PaymentChain, TokenChain, WebsiteInput};

mod order;
mod user;
mod website;

#[cfg(test)]
mod migration_tests;

pub use order::{EDIT_PRICE_USDT, FREE_EDIT_LIMIT, OrderStore, OrderWithWebsite, PaymentWindow};
pub use user::UserStore;
pub use website::WebsiteStore;

pub const PAYMENT_WINDOW_MINUTES: i64 = 30;
pub const ASSET_COLLECTION_TIMEOUT_MINUTES: i64 = 60;

#[derive(Clone)]
pub struct DbStores {
    pub users: UserStore,
    pub orders: OrderStore,
    pub websites: WebsiteStore,
}

impl DbStores {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            users: UserStore::new(pool.clone()),
            orders: OrderStore::new(pool.clone()),
            websites: WebsiteStore::new(pool),
        }
    }
}

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

impl DbStores {
    pub async fn normalize_sqlite_uuid_storage(&self) -> anyhow::Result<()> {
        Self::normalize_sqlite_uuid_storage_with_pool(&self.users.pool).await
    }

    async fn normalize_sqlite_uuid_storage_with_pool(pool: &SqlitePool) -> anyhow::Result<()> {
        const UUID_COLUMNS: &[(&str, &str)] = &[
            ("websites", "id"),
            ("websites", "user_id"),
            ("websites", "order_id"),
            ("orders", "id"),
            ("orders", "user_id"),
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

        DbStores::new(pool.clone())
            .normalize_sqlite_uuid_storage()
            .await
            .unwrap();
        let users = UserStore::new(pool.clone());
        let returned_id = users
            .upsert(12345, Some("ma"), None, None, true)
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
        let user_id = UserStore::new(pool.clone())
            .upsert(101, Some("alice"), None, None, true)
            .await
            .unwrap();
        let order_id = OrderStore::new(pool.clone())
            .create(user_id, Decimal::new(50, 0))
            .await
            .unwrap();

        assert!(OrderStore::new(pool.clone()).has_unpaid(101).await.unwrap());

        let cancelled = OrderStore::new(pool.clone())
            .cancel_for_user(101, Some(order_id))
            .await
            .unwrap();
        assert_eq!(cancelled, 1);
        assert!(!OrderStore::new(pool.clone()).has_unpaid(101).await.unwrap());

        let active = OrderStore::new(pool.clone()).get_active(101).await.unwrap();
        assert!(active.is_empty());
    }

    #[tokio::test]
    async fn order_wizard_flow() {
        let pool = test_pool().await;
        let user_id = UserStore::new(pool.clone())
            .upsert(201, Some("bob"), None, None, true)
            .await
            .unwrap();
        let order_id = OrderStore::new(pool.clone())
            .create(user_id, Decimal::new(50, 0))
            .await
            .unwrap();

        // Simulate wizard steps
        OrderStore::new(pool.clone())
            .update_field(order_id, "logo_asset_path", "/tmp/logo.png")
            .await
            .unwrap();
        OrderStore::new(pool.clone())
            .update_status(order_id, OrderStatus::NeedTicker)
            .await
            .unwrap();
        OrderStore::new(pool.clone())
            .update_field(order_id, "token_ticker", "PEPE")
            .await
            .unwrap();
        OrderStore::new(pool.clone())
            .update_status(order_id, OrderStatus::NeedPay)
            .await
            .unwrap();

        let order = OrderStore::new(pool.clone())
            .get(order_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(order.status, OrderStatus::NeedPay);
        assert_eq!(order.logo_asset_path.as_deref(), Some("/tmp/logo.png"));
        assert_eq!(order.token_ticker.as_deref(), Some("PEPE"));
    }

    #[tokio::test]
    async fn paid_order_becomes_generating() {
        let pool = test_pool().await;
        let user_id = UserStore::new(pool.clone())
            .upsert(301, Some("charlie"), None, None, true)
            .await
            .unwrap();
        let order_id = OrderStore::new(pool.clone())
            .create(user_id, Decimal::new(50, 0))
            .await
            .unwrap();
        OrderStore::new(pool.clone())
            .update_status(order_id, OrderStatus::NeedPay)
            .await
            .unwrap();

        OrderStore::new(pool.clone())
            .mark_paid(order_id, PaymentChain::Tron, "tx301")
            .await
            .unwrap();

        let order = OrderStore::new(pool.clone())
            .get(order_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(order.status, OrderStatus::PaidAndGenerating);

        // User can now create a new order
        assert!(!OrderStore::new(pool.clone()).has_unpaid(301).await.unwrap());
    }

    #[tokio::test]
    async fn completed_orders_show_in_history() {
        let pool = test_pool().await;
        let user_id = UserStore::new(pool.clone())
            .upsert(401, Some("dave"), None, None, true)
            .await
            .unwrap();
        let order_id = OrderStore::new(pool.clone())
            .create(user_id, Decimal::new(50, 0))
            .await
            .unwrap();
        OrderStore::new(pool.clone())
            .update_status(order_id, OrderStatus::NeedPay)
            .await
            .unwrap();
        OrderStore::new(pool.clone())
            .mark_paid(order_id, PaymentChain::Erc20, "tx401")
            .await
            .unwrap();
        OrderStore::new(pool.clone())
            .mark_done(order_id)
            .await
            .unwrap();

        let active = OrderStore::new(pool.clone()).get_active(401).await.unwrap();
        assert!(active.is_empty());

        let history = OrderStore::new(pool.clone())
            .get_completed(401, 10)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].order_id, order_id);
    }
}
