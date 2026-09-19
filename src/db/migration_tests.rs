use std::str::FromStr;

use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};

const LEGACY_SCHEMA: &str = include_str!("../../migrations/0001_init.sql");
const REMOVE_TEMPLATE_SCHEMA: &str =
    include_str!("../../migrations/0002_remove_template_system.sql");

async fn legacy_pool() -> SqlitePool {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(LEGACY_SCHEMA).execute(&pool).await.unwrap();
    pool
}

async fn seed_legacy_rows(pool: &SqlitePool) {
    sqlx::raw_sql(
        r#"
        INSERT INTO users (id, telegram_user_id, is_group_member) VALUES
            ('11111111-1111-4111-8111-111111111111', 42, 1);
        INSERT INTO templates (id, template_id, name, local_path, price_tier)
        VALUES ('22222222-2222-4222-8222-222222222222', 'template_01', 'Legacy', 'template_01', '200');
        INSERT INTO orders (id, user_id, status, build_mode, site_price_usdt,
            domain_price_usdt, total_usdt, locked_by, locked_at, attempt_count) VALUES (
            'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
            '11111111-1111-4111-8111-111111111111',
            'paid_and_generating', 'ai_free', '50', '0', '50',
            'ai-worker', '2026-09-19 00:00:00', 1);
        INSERT INTO orders (id, user_id, status, build_mode, template_id,
            site_price_usdt, domain_price_usdt, total_usdt, payment_chain,
            tx_hash, locked_by, locked_at, attempt_count) VALUES (
            'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',
            '11111111-1111-4111-8111-111111111111',
            'need_pay', 'template', 'template_01', '200', '5', '205',
            'tron', 'unfinished-tx', 'legacy-worker', '2026-09-19 01:00:00', 2);
        INSERT INTO orders (id, user_id, status, build_mode, template_id, token_chain,
            token_ticker, token_name, contract_address, x_url, telegram_url,
            logo_asset_path, requested_domain, site_price_usdt,
            domain_price_usdt, total_usdt, payment_chain, tx_hash, paid_at,
            pending_contract_address, edit_status, edit_payment_chain,
            edit_tx_hash, edit_success_count, attempt_count, created_at, updated_at) VALUES (
            'cccccccc-cccc-4ccc-8ccc-cccccccccccc',
            '11111111-1111-4111-8111-111111111111',
            'done', 'template', 'template_01', 'eth', 'DONE', 'Done Token',
            '0x1111111111111111111111111111111111111111',
            'https://x.com/done', 'https://t.me/done', '/logos/done.png',
            'done.example', '200', '10', '210', 'erc20', 'done-tx',
            '2026-09-19 02:00:00', '0x2222222222222222222222222222222222222222',
            'need_pay', 'sol', 'edit-tx', 3, 2,
            '2026-09-18 22:00:00', '2026-09-19 03:00:00');
        INSERT INTO orders (id, user_id, status, build_mode, template_id,
            site_price_usdt, domain_price_usdt, total_usdt) VALUES (
            'dddddddd-dddd-4ddd-8ddd-dddddddddddd',
            '11111111-1111-4111-8111-111111111111',
            'cancelled', 'template', 'template_01', '200', '0', '200');
        INSERT INTO websites (id, user_id, order_id, token_chain, token_ticker, token_name,
            contract_address, x_url, telegram_url, logo_asset_path,
            requested_domain, github_repo_url, cloudflare_pages_url,
            custom_domain, generation_provider_used) VALUES (
            'eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee',
            '11111111-1111-4111-8111-111111111111',
            'cccccccc-cccc-4ccc-8ccc-cccccccccccc',
            'eth', 'DONE', 'Done Token',
            '0x1111111111111111111111111111111111111111',
            'https://x.com/done', 'https://t.me/done', '/logos/done.png',
            'done.example', 'https://github.com/example/done',
            'https://done.pages.dev', 'done.example', 'gemini');
        "#,
    )
    .execute(pool)
    .await
    .unwrap();
}

async fn apply_removal(pool: &SqlitePool) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(REMOVE_TEMPLATE_SCHEMA)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn removes_template_schema_without_losing_order_or_website_history() {
    let pool = legacy_pool().await;
    seed_legacy_rows(&pool).await;
    apply_removal(&pool).await;

    let unfinished = sqlx::query(
        "SELECT status, locked_by, locked_at, site_price_usdt, domain_price_usdt, \
         total_usdt, payment_chain, tx_hash, attempt_count FROM orders WHERE id = ?",
    )
    .bind("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(unfinished.get::<String, _>("status"), "need_manual_review");
    assert_eq!(unfinished.get::<Option<String>, _>("locked_by"), None);
    assert_eq!(unfinished.get::<Option<String>, _>("locked_at"), None);
    assert_eq!(unfinished.get::<String, _>("site_price_usdt"), "200");
    assert_eq!(unfinished.get::<String, _>("domain_price_usdt"), "5");
    assert_eq!(unfinished.get::<String, _>("total_usdt"), "205");
    assert_eq!(unfinished.get::<String, _>("payment_chain"), "tron");
    assert_eq!(unfinished.get::<String, _>("tx_hash"), "unfinished-tx");
    assert_eq!(unfinished.get::<i64, _>("attempt_count"), 2);

    let ai = sqlx::query(
        "SELECT status, locked_by, locked_at, site_price_usdt, total_usdt, \
         attempt_count FROM orders WHERE id = ?",
    )
    .bind("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(ai.get::<String, _>("status"), "paid_and_generating");
    assert_eq!(ai.get::<String, _>("locked_by"), "ai-worker");
    assert_eq!(ai.get::<String, _>("locked_at"), "2026-09-19 00:00:00");
    assert_eq!(ai.get::<String, _>("site_price_usdt"), "50");
    assert_eq!(ai.get::<String, _>("total_usdt"), "50");
    assert_eq!(ai.get::<i64, _>("attempt_count"), 1);

    let completed = sqlx::query(
        "SELECT status, site_price_usdt, domain_price_usdt, total_usdt, tx_hash, \
         paid_at, pending_contract_address, edit_status, edit_payment_chain, \
         edit_tx_hash, edit_success_count, attempt_count, created_at, updated_at \
         FROM orders WHERE id = ?",
    )
    .bind("cccccccc-cccc-4ccc-8ccc-cccccccccccc")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(completed.get::<String, _>("status"), "done");
    assert_eq!(completed.get::<String, _>("site_price_usdt"), "200");
    assert_eq!(completed.get::<String, _>("domain_price_usdt"), "10");
    assert_eq!(completed.get::<String, _>("total_usdt"), "210");
    assert_eq!(completed.get::<String, _>("tx_hash"), "done-tx");
    assert_eq!(completed.get::<String, _>("paid_at"), "2026-09-19 02:00:00");
    assert_eq!(
        completed.get::<String, _>("pending_contract_address"),
        "0x2222222222222222222222222222222222222222"
    );
    assert_eq!(completed.get::<String, _>("edit_status"), "need_pay");
    assert_eq!(completed.get::<String, _>("edit_payment_chain"), "sol");
    assert_eq!(completed.get::<String, _>("edit_tx_hash"), "edit-tx");
    assert_eq!(completed.get::<i64, _>("edit_success_count"), 3);
    assert_eq!(completed.get::<i64, _>("attempt_count"), 2);
    assert_eq!(
        completed.get::<String, _>("created_at"),
        "2026-09-18 22:00:00"
    );
    assert_eq!(
        completed.get::<String, _>("updated_at"),
        "2026-09-19 03:00:00"
    );

    let cancelled_status: String = sqlx::query_scalar("SELECT status FROM orders WHERE id = ?")
        .bind("dddddddd-dddd-4ddd-8ddd-dddddddddddd")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(cancelled_status, "cancelled");

    let website = sqlx::query(
        "SELECT order_id, github_repo_url, cloudflare_pages_url, custom_domain, \
         generation_provider_used FROM websites WHERE id = ?",
    )
    .bind("eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        website.get::<String, _>("order_id"),
        "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
    );
    assert_eq!(
        website.get::<String, _>("github_repo_url"),
        "https://github.com/example/done"
    );
    assert_eq!(
        website.get::<String, _>("cloudflare_pages_url"),
        "https://done.pages.dev"
    );
    assert_eq!(website.get::<String, _>("custom_domain"), "done.example");
    assert_eq!(
        website.get::<String, _>("generation_provider_used"),
        "gemini"
    );

    let template_tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = 'templates'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(template_tables, 0);

    let columns = sqlx::query("PRAGMA table_info(orders)")
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect::<Vec<_>>();
    assert!(!columns.iter().any(|name| name == "build_mode"));
    assert!(!columns.iter().any(|name| name == "template_id"));

    let indexes =
        sqlx::query("SELECT name FROM sqlite_schema WHERE type = 'index' AND tbl_name = 'orders'")
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get::<String, _>("name"))
            .collect::<Vec<_>>();
    for expected in [
        "orders_user_id_idx",
        "orders_status_idx",
        "orders_status_generating_idx",
    ] {
        assert!(indexes.iter().any(|name| name == expected));
    }

    let website_fk_target = sqlx::query("PRAGMA foreign_key_list(websites)")
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.get::<String, _>("from") == "order_id")
        .map(|row| row.get::<String, _>("table"));
    assert_eq!(website_fk_target.as_deref(), Some("orders"));
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn migration_ddl_rolls_back_as_one_transaction() {
    let pool = legacy_pool().await;
    seed_legacy_rows(&pool).await;

    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(REMOVE_TEMPLATE_SCHEMA)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();

    let template_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM templates")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(template_count, 1);
    let columns = sqlx::query("PRAGMA table_info(orders)")
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect::<Vec<_>>();
    assert!(columns.iter().any(|name| name == "build_mode"));
    assert!(columns.iter().any(|name| name == "template_id"));
}
