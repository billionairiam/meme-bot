# Repository Ablation and Legacy Template Removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the obsolete internal template subsystem and database schema, then keep only evidence-backed simplifications that reduce repository complexity without changing supported production behavior.

**Architecture:** Preserve the `meme-bot`/`api-server` process boundary, database stores, and external-integration traits. Apply one atomic SQLite migration, simplify the AI-only automated order path, then evaluate each remaining wrapper independently and record before/after metrics so unsuccessful ablations can be reverted in isolation.

**Tech Stack:** Rust 2024, Tokio, SQLx 0.8 with SQLite, Teloxide, Axum, `rust_decimal`, Cargo tests/Clippy.

**Spec:** `docs/superpowers/specs/2026-09-19-repository-ablation-design.md`

## Global Constraints

- Keep `meme-bot` and `api-server` as separate processes.
- Preserve REST API paths and externally observable behavior.
- Preserve AI creation, payment, domain, deployment, website-editing, and stored-history behavior.
- Keep payment, generation, deployment, domain-registration, and DNS integrations behind traits with credential-free test doubles.
- Keep SQLite configured with one pooled connection and preserve the queue-locking model.
- Keep all monetary values as `rust_decimal::Decimal` or their existing exact SQLite text representation; never use `f64`.
- Do not modify `migrations/0001_init.sql`; create `migrations/0002_remove_template_system.sql`.
- Preserve the manual template offer at exactly 200 USDT, `https://memewebs.store`, and the configured `ADMIN_CONTACT`; it must not create an order.
- Old `tpl_page:*`, `select_tpl:*`, `confirm_tpl:*`, and `/start tpl_*` interactions are unsupported after removal.
- Keep `anyhow` context, `thiserror` domain errors, provider retry policy, and recoverable/non-recoverable error classification.
- Before production deployment, back up the SQLite database. Rollback restores that backup before starting old binaries.
- Use `apply_patch` for repository file edits and preserve unrelated user changes.
- Accept an ablation only when at least one measured complexity dimension decreases, no unexplained dimension increases, and all required verification passes.

## Review Focus

- An unfinished legacy template order, including one with a queue lock, must migrate to `need_manual_review` with both lock fields cleared; Task 2 owns this migration test.
- Completed and cancelled legacy template orders, exact money/payment/edit values, and an associated website foreign key must survive migration; Task 2 owns these preservation assertions.
- Migration DDL must roll back as one transaction and leave the `0001` schema usable if not committed; Task 2 owns the rollback test.
- The supported manual template entry must still show 200 USDT, link to `memewebs.store`, expose admin contact, and contain no checkout callback; Task 2 retains the keyboard/screen tests.
- A retryable domain quote failure must retry, a non-retryable failure must stop, and alternatives must exclude the requested TLD and sort by price; Task 5 adds characterization tests.

---

## File Map

**Create**

- `migrations/0002_remove_template_system.sql` — atomic state/data migration and removal of obsolete schema.
- `src/db/migration_tests.rs` — migration preservation, schema, foreign-key, and rollback coverage.
- `docs/superpowers/reports/2026-09-19-repository-ablation-results.md` — baseline, per-batch measurements, retained/reverted experiments, and deployment note.

**Delete**

- `src/db/template.rs` — retired internal template catalog store.
- `src/app.rs` — one-use queue-construction wrapper, if Task 3 metrics confirm removal.

**Modify for template removal**

- `src/models.rs` — remove `BuildMode`, `Template`, and template fields on `Order`.
- `src/db/mod.rs` — remove `TemplateStore`, register migration tests, and stop normalizing the deleted table.
- `src/db/order.rs` — make order creation AI-only and stop mapping deleted columns.
- `src/generation.rs` — remove template/build-mode request fields and unread generation result data.
- `src/queue.rs` — build AI generation requests without template metadata.
- `src/config.rs` — remove retired template configuration and later make `database_url` non-optional.
- `src/bin/meme-bot.rs` — remove catalog synchronization and later wire the queue directly.
- `src/telegram/handlers.rs` — remove legacy checkout compatibility and simplify AI order creation.
- `src/telegram/helpers.rs` — remove retired deep-link parsing and later remove confirmed dead helpers.
- `.env.meme-bot.example` — remove obsolete catalog variables.
- `README.md` — document current schema, manual template behavior, backup requirement, and actual file layout.

**Modify for focused ablations**

- `src/telegram/keyboards.rs`, `src/telegram/screens.rs`, `src/telegram/handlers.rs` — replace the zero-state `Keyboards` namespace with module functions.
- `src/telegram/domain_flow.rs`, `src/telegram/handlers.rs` — replace `DomainFlow` with functions while retaining domain behavior.
- `src/queue.rs`, `src/bin/meme-bot.rs`, `src/lib.rs` — remove `GenerationQueueConfig` and `App` forwarding layers.
- `Cargo.toml`, `Cargo.lock`, `src/lib.rs`, `src/db/mod.rs`, `src/telegram/helpers.rs` — remove unused direct dependency/export/dead-code surface.

---

### Task 1: Record the Immutable Baseline

**Files:**
- Create: `docs/superpowers/reports/2026-09-19-repository-ablation-results.md`

**Interfaces:**
- Consumes: repository state at commit `0cd5e68`.
- Produces: the fixed commands and baseline numbers used to accept or reject later ablations.

- [ ] **Step 1: Re-run the baseline measurements**

Run:

```bash
rg --files src migrations | sort | xargs wc -l | tail -1
rg -n '^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?(struct|enum|trait)[[:space:]]' src | wc -l
rg -n '^[[:space:]]*pub([[:space:]]|\([^)]*\)[[:space:]])+(mod|struct|enum|trait|fn|type|use|const|static)[[:space:]]' src | wc -l
awk '/^\[dependencies\]/{inside=1;next} /^\[/{inside=0} inside && /^[A-Za-z0-9_-]+[[:space:]]*=/{count++} END{print count}' Cargo.toml
cargo test -- --list
cargo test
```

Expected at `0cd5e68`: 11,623 Rust/migration lines, 78 declared structs/enums/traits, 200 public symbols under the fixed regex, 23 direct dependencies, 65 listed tests, and the full suite passes with 58 passed and 7 ignored.

- [ ] **Step 2: Create the baseline report**

Create the report with this initial content, using `apply_patch`:

```markdown
# Repository Ablation Results

Implementation baseline: `0cd5e68`

## Fixed Measurement Commands

The implementation plan contains the exact commands. Every batch uses those
commands unchanged so the measurements remain comparable.

## Baseline

| Metric | Baseline |
|---|---:|
| Rust and migration lines | 11,623 |
| Structs, enums, and traits | 78 |
| Public symbols | 200 |
| Direct dependencies | 23 |
| Listed tests | 65 |

## Wrapper Candidates

- Queue construction: `main -> App::build_queue -> GenerationQueue::new`; `App::run_once` has no caller.
- `GenerationQueueConfig`: one construction site and one consumer.
- `Keyboards`: zero stored state; all methods are namespace functions.
- `DomainFlow`: one `&dyn DomainRegistrar` field; two behavior methods.
- `SiteGenerationResult.output_dir`: written by generation and never read.
- `Config.database_url`: always receives either `DATABASE_URL` or a default, but is stored as `Option<String>`.

## Batch Results

Results are appended only after a batch passes its required checks. A rejected
candidate is recorded with the reason it was reverted.

## Deployment

Before deploying migration 0002, stop both binaries and copy `db.sqlite3` to
`db.sqlite3.pre-0002`. Rollback requires restoring that copy before starting
the old binaries.
```

- [ ] **Step 3: Verify and commit the baseline**

Run:

```bash
git diff --check
git add docs/superpowers/reports/2026-09-19-repository-ablation-results.md
git commit -m "Document repository ablation baseline"
```

Expected: one documentation commit; no source or schema changes.

---

### Task 2: Atomically Remove the Legacy Template Subsystem

**Files:**
- Create: `migrations/0002_remove_template_system.sql`
- Create: `src/db/migration_tests.rs`
- Delete: `src/db/template.rs`
- Modify: `src/db/mod.rs`
- Modify: `src/db/order.rs`
- Modify: `src/models.rs`
- Modify: `src/generation.rs`
- Modify: `src/queue.rs`
- Modify: `src/config.rs`
- Modify: `src/bin/meme-bot.rs`
- Modify: `src/telegram/handlers.rs`
- Modify: `src/telegram/helpers.rs`
- Modify: `.env.meme-bot.example`
- Modify: `README.md`
- Modify: `docs/superpowers/reports/2026-09-19-repository-ablation-results.md`

**Interfaces:**
- Consumes: `orders` and `websites` schema from migration 0001; current `OrderStore::create(Uuid, BuildMode, Option<&str>, Decimal)`.
- Produces: `OrderStore::create(&self, user_id: Uuid, site_price_usdt: Decimal) -> anyhow::Result<Uuid>`; `SiteGenerationRequest { order_id, input, output_dir }`; a schema with `users`, `orders`, and `websites` but no internal template table or order discriminator.

- [ ] **Step 1: Add migration tests before the migration file exists**

Add `#[cfg(test)] mod migration_tests;` beside the existing test module declaration in `src/db/mod.rs`, then create `src/db/migration_tests.rs` with the following structure and assertions:

```rust
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
        INSERT INTO users (id, telegram_user_id, is_group_member)
        VALUES ('11111111-1111-4111-8111-111111111111', 42, 1);

        INSERT INTO templates (id, template_id, name, local_path, price_tier)
        VALUES ('22222222-2222-4222-8222-222222222222', 'template_01',
                'Legacy', 'template_01', '200');

        INSERT INTO orders (
            id, user_id, status, build_mode, site_price_usdt, domain_price_usdt,
            total_usdt, locked_by, locked_at, attempt_count
        ) VALUES (
            'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
            '11111111-1111-4111-8111-111111111111',
            'paid_and_generating', 'ai_free', '50', '0', '50',
            'ai-worker', '2026-09-19 00:00:00', 1
        );

        INSERT INTO orders (
            id, user_id, status, build_mode, template_id, site_price_usdt,
            domain_price_usdt, total_usdt, payment_chain, tx_hash, locked_by,
            locked_at, attempt_count
        ) VALUES (
            'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',
            '11111111-1111-4111-8111-111111111111',
            'need_pay', 'template', 'template_01', '200', '5', '205',
            'tron', 'unfinished-tx', 'legacy-worker', '2026-09-19 01:00:00', 2
        );

        INSERT INTO orders (
            id, user_id, status, build_mode, template_id, token_chain,
            token_ticker, token_name, contract_address, x_url, telegram_url,
            logo_asset_path, requested_domain, site_price_usdt,
            domain_price_usdt, total_usdt, payment_chain, tx_hash, paid_at,
            pending_contract_address, edit_status, edit_payment_chain,
            edit_tx_hash, edit_success_count, attempt_count, created_at, updated_at
        ) VALUES (
            'cccccccc-cccc-4ccc-8ccc-cccccccccccc',
            '11111111-1111-4111-8111-111111111111',
            'done', 'template', 'template_01', 'eth', 'DONE', 'Done Token',
            '0x1111111111111111111111111111111111111111',
            'https://x.com/done', 'https://t.me/done', '/logos/done.png',
            'done.example', '200', '10', '210', 'erc20', 'done-tx',
            '2026-09-19 02:00:00', '0x2222222222222222222222222222222222222222',
            'need_pay', 'sol', 'edit-tx', 3, 2,
            '2026-09-18 22:00:00', '2026-09-19 03:00:00'
        );

        INSERT INTO orders (
            id, user_id, status, build_mode, template_id, site_price_usdt,
            domain_price_usdt, total_usdt
        ) VALUES (
            'dddddddd-dddd-4ddd-8ddd-dddddddddddd',
            '11111111-1111-4111-8111-111111111111',
            'cancelled', 'template', 'template_01', '200', '0', '200'
        );

        INSERT INTO websites (
            id, user_id, order_id, token_chain, token_ticker, token_name,
            contract_address, x_url, telegram_url, logo_asset_path,
            requested_domain, github_repo_url, cloudflare_pages_url,
            custom_domain, generation_provider_used
        ) VALUES (
            'eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee',
            '11111111-1111-4111-8111-111111111111',
            'cccccccc-cccc-4ccc-8ccc-cccccccccccc',
            'eth', 'DONE', 'Done Token',
            '0x1111111111111111111111111111111111111111',
            'https://x.com/done', 'https://t.me/done', '/logos/done.png',
            'done.example', 'https://github.com/example/done',
            'https://done.pages.dev', 'done.example', 'gemini'
        );
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
    assert_eq!(completed.get::<String, _>("created_at"), "2026-09-18 22:00:00");
    assert_eq!(completed.get::<String, _>("updated_at"), "2026-09-19 03:00:00");

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
    assert_eq!(website.get::<String, _>("generation_provider_used"), "gemini");

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

    let indexes = sqlx::query(
        "SELECT name FROM sqlite_schema WHERE type = 'index' AND tbl_name = 'orders'",
    )
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
```

- [ ] **Step 2: Run the migration test to verify the missing implementation**

Run:

```bash
cargo test db::migration_tests -- --nocapture
```

Expected: compilation fails because `migrations/0002_remove_template_system.sql` does not exist.

- [ ] **Step 3: Add the atomic migration**

Create `migrations/0002_remove_template_system.sql` with the complete schema below. SQLx SQLite applies the entire migration and migration bookkeeping in one transaction.

```sql
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
```

- [ ] **Step 4: Verify migration behavior and rollback**

Run:

```bash
cargo test db::migration_tests -- --nocapture
```

Expected: both migration tests pass, including `PRAGMA foreign_key_check` and transactional rollback.

- [ ] **Step 5: Simplify database models and order creation**

Apply these exact interface changes:

```diff
-pub enum BuildMode { Template, AiFree }
 pub struct Order {
     pub id: Uuid,
     pub user_id: Uuid,
     pub status: OrderStatus,
-    pub build_mode: Option<BuildMode>,
-    pub template_id: Option<String>,
     // keep every other current Order field unchanged
 }
```

Replace `OrderStore::create` with:

```rust
pub async fn create(
    &self,
    user_id: Uuid,
    site_price_usdt: Decimal,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    let row = sqlx::query(
        r#"
        INSERT INTO orders (id, user_id, status, site_price_usdt, total_usdt)
        VALUES (?, ?, 'need_chain', ?, ?)
        RETURNING id
        "#,
    )
    .bind(uuid_value(id))
    .bind(uuid_value(user_id))
    .bind(site_price_usdt.to_string())
    .bind(site_price_usdt.to_string())
    .fetch_one(&self.pool)
    .await?;
    uuid_from_row(&row, "id")
}
```

Also:

- remove `mod template`, `TemplateStore`, and `DbStores.templates` from `src/db/mod.rs`;
- remove `("templates", "id")` from UUID normalization;
- delete the three catalog-specific tests;
- update remaining `.create(...)` test calls to pass only `(user_id, price)`;
- remove `build_mode` and `template_id` parsing from `OrderStore::order_from_row`;
- remove the two fields from every `Order` test literal;
- delete `src/db/template.rs` with `apply_patch`.

- [ ] **Step 6: Make generation requests AI-only**

Change the interfaces and prompt payload to:

```rust
pub struct SiteGenerationRequest {
    pub order_id: Uuid,
    pub input: WebsiteInput,
    pub output_dir: PathBuf,
}

let spec = serde_json::json!({
    "order_id": request.order_id,
    "provider": provider_name,
    "input": request.input,
    "output_dir": request.output_dir,
    "done_file": request.output_dir.join(".agent_done.json"),
    "constraints": {
        "static_site_only": true,
        "no_backend": true,
        "no_database": true,
        "no_user_prompt": true,
        "do_not_ask_questions": true,
        "write_files_directly": true
    }
});
```

Remove the `build_mode` log field in `queue.rs` and remove both obsolete fields
from every existing generation and queue test request literal. The new struct
shape and JSON construction make reintroducing either field a compile-time
code change; the retirement audit in Step 10 checks that production source no
longer contains either identifier.

Run:

```bash
cargo test generation::tests -- --nocapture
cargo test queue::tests -- --nocapture
```

Expected: generation validation and queue request tests pass without template metadata.

- [ ] **Step 7: Remove retired configuration and startup synchronization**

From `Config` and `Config::from_env`, delete:

```text
template_preview_domain
template_preview_base_url
template_prices_usdt
local_templates_dir
```

Delete `DEFAULT_TEMPLATE_PRICES` and `parse_template_prices`. Remove catalog synchronization and its log from `src/bin/meme-bot.rs`. Do not remove `admin_contact`, `TEMPLATE_PRICE_USDT`, or `TEMPLATE_SITE_URL`.

Run:

```bash
cargo check --bin meme-bot
```

Expected: the bot binary compiles without internal catalog configuration.

- [ ] **Step 8: Simplify Telegram AI order creation and remove checkout compatibility**

Change the helper to:

```rust
async fn start_new_order(
    pool: &SqlitePool,
    telegram_user_id: UserId,
    username: Option<&str>,
    first_name: Option<&str>,
    last_name: Option<&str>,
    site_price_usdt: rust_decimal::Decimal,
) -> anyhow::Result<uuid::Uuid> {
    let db_user_id = users(pool)
        .upsert(
            telegram_user_id.0 as i64,
            username,
            first_name,
            last_name,
            true,
        )
        .await?;
    if orders(pool).has_unpaid(telegram_user_id.0 as i64).await? {
        anyhow::bail!("has pending order");
    }
    let order_id = orders(pool).create(db_user_id, site_price_usdt).await?;
    info!(
        user_id = telegram_user_id.0 as i64,
        order_id = %order_id,
        site_price_usdt = %site_price_usdt,
        "order created"
    );
    Ok(order_id)
}
```

Remove `BuildMode` imports and arguments, `is_legacy_template_callback`, `parse_template_deep_link`, their branches, and their tests. Keep the `template_mode` callback, manual contact screen, manual template keyboard, and their fixed-price/link/admin tests.

Run:

```bash
cargo test telegram::keyboards::tests -- --nocapture
cargo test telegram::screens::tests -- --nocapture
```

Expected: the supported manual template tests pass and no checkout callback is emitted.

- [ ] **Step 9: Update operator documentation and environment examples**

Use `apply_patch` to:

- remove `TEMPLATE_PRICES_USDT`, `TEMPLATE_PREVIEW_DOMAIN`, `TEMPLATE_PREVIEW_BASE_URL`, and `LOCAL_TEMPLATES_DIR` from `.env.meme-bot.example` and the README configuration table;
- change the architecture line to `SQLite orders/users/websites`;
- remove the `templates` row from the data-model table and say the current schema is the ordered migration set under `migrations/`;
- remove `src/app.rs` from the layout only when Task 3 deletes that file;
- add a deployment warning with the exact safe sequence: stop both binaries, copy `db.sqlite3` to `db.sqlite3.pre-0002`, deploy both new binaries, then start them; rollback restores the copy before old binaries start.

- [ ] **Step 10: Prove the retired subsystem is gone**

Run:

```bash
rg -n --glob '!src/db/migration_tests.rs' 'TemplateStore|BuildMode|template_id|build_mode|TEMPLATE_PRICES_USDT|TEMPLATE_PREVIEW_DOMAIN|TEMPLATE_PREVIEW_BASE_URL|LOCAL_TEMPLATES_DIR' src README.md .env.meme-bot.example Cargo.toml
rg -n 'TEMPLATE_PRICE_USDT|TEMPLATE_SITE_URL|memewebs\.store|admin_contact' src/telegram README.md
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --bin meme-bot
cargo build --bin api-server
```

Expected: the first `rg` has no matches outside the intentional legacy fixture
in `src/db/migration_tests.rs`; the second finds only supported manual-template
UI/docs; all verification commands pass.

- [ ] **Step 11: Measure, report, and commit Batch 1**

Re-run the fixed metrics from Task 1. Append a `Batch 1 — legacy template removal` section containing the observed integers, verification result, and `Retained: yes` to the report. Then run:

```bash
git diff --check
git add migrations/0002_remove_template_system.sql src/db/migration_tests.rs src/db/mod.rs src/db/order.rs src/db/template.rs src/models.rs src/generation.rs src/queue.rs src/config.rs src/bin/meme-bot.rs src/telegram/handlers.rs src/telegram/helpers.rs .env.meme-bot.example README.md docs/superpowers/reports/2026-09-19-repository-ablation-results.md
git commit -m "Remove legacy template subsystem"
```

Expected: migration, code, tests, docs, and report are one green, reversible commit.

---

### Task 3: Remove `App` and `GenerationQueueConfig`

**Files:**
- Delete: `src/app.rs`
- Modify: `src/lib.rs`
- Modify: `src/queue.rs`
- Modify: `src/bin/meme-bot.rs`
- Modify: `README.md`
- Modify: `docs/superpowers/reports/2026-09-19-repository-ablation-results.md`

**Interfaces:**
- Consumes: `GenerationRouter::from_config(&Config)` and `HttpClientPagesDeployer::new(String, String)`.
- Produces: `GenerationQueue::new(pool, router, deployer, Arc<Config>, String, notification_tx)` called directly once from the bot entry point.

- [ ] **Step 1: Capture the pre-ablation behavior check**

Run:

```bash
cargo test queue::tests -- --nocapture
cargo build --bin meme-bot
```

Expected: pass before structural changes.

- [ ] **Step 2: Remove the queue configuration carrier**

Delete `GenerationQueueConfig` and change `GenerationQueue::new` to:

```rust
pub fn new(
    pool: SqlitePool,
    router: Arc<GenerationRouter>,
    deployer: Option<Arc<dyn SiteDeployer>>,
    config: Arc<Config>,
    worker_id: String,
    notification_tx: Option<mpsc::UnboundedSender<UserNotification>>,
) -> Self {
    Self {
        orders: OrderStore::new(pool.clone()),
        websites: WebsiteStore::new(pool.clone()),
        pool,
        router,
        deployer,
        semaphore: Arc::new(Semaphore::new(config.generation_max_concurrency.max(1))),
        lock_mutex: Arc::new(Mutex::new(())),
        config,
        worker_id,
        notification_tx,
    }
}
```

- [ ] **Step 3: Inline the one-use app wiring into `meme-bot`**

Import `HttpClientPagesDeployer`, `SiteDeployer`, `GenerationRouter`, and `GenerationQueue` in `src/bin/meme-bot.rs`, then replace `App::new(...).build_queue(...)` with:

```rust
let router = Arc::new(GenerationRouter::from_config(&config));
let deployer: Option<Arc<dyn SiteDeployer>> = match (
    config.api_service_base_url.as_deref(),
    config.api_service_api_key.as_deref(),
) {
    (Some(base_url), Some(api_key)) => Some(Arc::new(HttpClientPagesDeployer::new(
        base_url.to_string(),
        api_key.to_string(),
    ))),
    _ => None,
};
info!(
    max_concurrency = config.generation_max_concurrency,
    primary_provider = config.generation_primary_provider.as_str(),
    "generation queue configured"
);
let queue = GenerationQueue::new(
    pool.clone(),
    router,
    deployer,
    Arc::clone(&config),
    "worker-1".to_string(),
    Some(notification_tx),
);
```

Delete `src/app.rs`, delete `pub mod app` from `src/lib.rs`, and remove `app.rs` from the README layout.

- [ ] **Step 4: Verify the ablation and keep it only if it wins**

Run:

```bash
rg -n 'struct App|GenerationQueueConfig|run_once|build_queue' src
cargo fmt --check
cargo test
cargo build --bin meme-bot
```

Expected: `rg` has no matches; all checks pass. Re-run fixed metrics and confirm at least two types and one forwarding call layer are removed without increasing source lines. If not, revert only Task 3.

- [ ] **Step 5: Report and commit the accepted candidate**

Append the measured result under `Batch 2 — forwarding abstractions`, naming both removed types and the call-depth reduction. Then run:

```bash
git add src/lib.rs src/queue.rs src/bin/meme-bot.rs README.md docs/superpowers/reports/2026-09-19-repository-ablation-results.md
git commit -m "Inline generation queue wiring"
```

---

### Task 4: Replace the `Keyboards` Namespace Type with Module Functions

**Files:**
- Modify: `src/telegram/keyboards.rs`
- Modify: `src/telegram/handlers.rs`
- Modify: `src/telegram/screens.rs`
- Modify: `docs/superpowers/reports/2026-09-19-repository-ablation-results.md`

**Interfaces:**
- Consumes: all existing `Keyboards::<name>(...) -> InlineKeyboardMarkup` behavior.
- Produces: `keyboards::<name>(...) -> InlineKeyboardMarkup` with identical callback data, labels, ordering, and limits.

- [ ] **Step 1: Run existing keyboard characterization tests**

Run:

```bash
cargo test telegram::keyboards::tests -- --nocapture
```

Expected: the three manual-template tests pass before the refactor.

- [ ] **Step 2: Convert associated functions to module functions**

Remove:

```rust
pub(crate) struct Keyboards;

impl Keyboards {
    // functions
}
```

Keep each function body unchanged and move it to module scope, for example:

```rust
pub(crate) fn main_menu() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback("🚀 Create Website", "create")],
        vec![InlineKeyboardButton::callback("✏️ Edit Website", "edit")],
        vec![InlineKeyboardButton::callback("📊 Order Status", "status")],
        vec![InlineKeyboardButton::callback("🧾 Paid History", "history")],
        vec![InlineKeyboardButton::callback("👤 Contact Admin", "contact_admin")],
    ])
}

pub(crate) fn create_mode(ai_creation_price_usdt: Decimal) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            format!("📦 Template Mode | {TEMPLATE_PRICE_USDT} USDT"),
            "template_mode",
        )],
        vec![InlineKeyboardButton::callback(
            format!("🤖 Free Creation | {} USDT", ai_creation_price_usdt),
            "ai_mode",
        )],
        vec![InlineKeyboardButton::callback("⬅ Back", "main")],
    ])
}
```

In tests, import the functions from `super` and call `create_mode(...)` and `template_contact()` directly.

- [ ] **Step 3: Update all call sites mechanically**

In `handlers.rs` and `screens.rs`, import `super::keyboards` and replace every `Keyboards::name(...)` with `keyboards::name(...)`. Do not change button text, callback values, row ordering, `.take(12)`, or `.take(10)` limits.

- [ ] **Step 4: Verify, measure, report, and commit**

Run:

```bash
rg -n 'Keyboards' src/telegram
cargo fmt --check
cargo test telegram::keyboards::tests -- --nocapture
cargo test telegram::screens::tests -- --nocapture
cargo test
```

Expected: `rg` has no matches, behavior tests pass, type count decreases by one, and source lines do not increase. Append the result to the Batch 2 report section, then run:

```bash
git add src/telegram/keyboards.rs src/telegram/handlers.rs src/telegram/screens.rs docs/superpowers/reports/2026-09-19-repository-ablation-results.md
git commit -m "Replace keyboard namespace with functions"
```

---

### Task 5: Replace `DomainFlow` with Tested Functions

**Files:**
- Modify: `src/telegram/domain_flow.rs`
- Modify: `src/telegram/handlers.rs`
- Modify: `docs/superpowers/reports/2026-09-19-repository-ablation-results.md`

**Interfaces:**
- Consumes: `DomainRegistrar::quote(&DomainQuoteRequest)` and `EXTERNAL_STEP_MAX_ATTEMPTS`.
- Produces: `quote_with_retries(&dyn DomainRegistrar, &str) -> Result<DomainQuote, DomainError>` and `check_alternatives(&dyn DomainRegistrar, &str, &str) -> Vec<DomainQuote>`.

- [ ] **Step 1: Add characterization tests to the current wrapper**

Add a `#[cfg(test)]` module in `domain_flow.rs` with this stub and quote helper:

```rust
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use crate::domain::{
    DomainPurchaseRequest, DomainPurchaseResult, SUGGESTED_TLDS,
};

use super::*;

struct StubRegistrar {
    responses: Mutex<VecDeque<Result<DomainQuote, DomainError>>>,
    calls: AtomicUsize,
}

impl StubRegistrar {
    fn new(responses: Vec<Result<DomainQuote, DomainError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            calls: AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl DomainRegistrar for StubRegistrar {
    async fn quote(&self, _request: &DomainQuoteRequest) -> Result<DomainQuote, DomainError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("stub response for every quote")
    }

    async fn purchase(
        &self,
        _request: &DomainPurchaseRequest,
    ) -> Result<DomainPurchaseResult, DomainError> {
        unreachable!("purchase is not used by domain flow")
    }
}

fn quote(domain: &str, price: i64) -> DomainQuote {
    DomainQuote {
        domain: domain.to_string(),
        tld: domain.rsplit_once('.').map(|(_, tld)| tld).unwrap_or("").to_string(),
        available: true,
        is_premium: false,
        price_usd: rust_decimal::Decimal::new(price, 0),
        years: 1,
    }
}
```

Add these tests against the current `DomainFlow` API:

```rust
#[tokio::test]
async fn retryable_quote_error_is_retried() {
    let registrar = StubRegistrar::new(vec![
        Err(DomainError::Http("temporary".to_string())),
        Ok(quote("coin.xyz", 12)),
    ]);
    let result = DomainFlow::new(&registrar)
        .quote_with_retries("coin.xyz")
        .await
        .unwrap();
    assert_eq!(result.domain, "coin.xyz");
    assert_eq!(registrar.call_count(), 2);
}

#[tokio::test]
async fn non_retryable_quote_error_stops_immediately() {
    let registrar = StubRegistrar::new(vec![Err(DomainError::UnsupportedTld)]);
    let error = DomainFlow::new(&registrar)
        .quote_with_retries("coin.invalid")
        .await
        .unwrap_err();
    assert!(matches!(error, DomainError::UnsupportedTld));
    assert_eq!(registrar.call_count(), 1);
}
```

Add the alternatives test:

```rust
#[tokio::test]
async fn alternatives_exclude_requested_tld_filter_and_sort_by_price() {
    let responses = SUGGESTED_TLDS
        .iter()
        .filter(|tld| **tld != "com")
        .map(|tld| {
            let mut candidate = match *tld {
                "xyz" => quote("coin.xyz", 20),
                "fun" => quote("coin.fun", 5),
                _ => quote(&format!("coin.{tld}"), 99),
            };
            if *tld == "site" {
                candidate.is_premium = true;
            } else if !matches!(*tld, "xyz" | "fun") {
                candidate.available = false;
            }
            Ok(candidate)
        })
        .collect();
    let registrar = StubRegistrar::new(responses);

    let alternatives = DomainFlow::new(&registrar)
        .check_alternatives("coin", "com")
        .await;

    assert_eq!(
        alternatives
            .iter()
            .map(|quote| quote.domain.as_str())
            .collect::<Vec<_>>(),
        ["coin.fun", "coin.xyz"]
    );
    assert_eq!(registrar.call_count(), SUGGESTED_TLDS.len() - 1);
}
```

- [ ] **Step 2: Run characterization tests before refactoring**

Run:

```bash
cargo test telegram::domain_flow::tests -- --nocapture
```

Expected: retry, stop, exclusion, filtering, and price-order behavior pass. The retry test takes approximately two seconds because it exercises the real backoff.

- [ ] **Step 3: Replace the wrapper with free functions**

Delete `DomainFlow` and its `impl`, preserving function bodies under these signatures:

```rust
pub(crate) async fn quote_with_retries(
    registrar: &dyn DomainRegistrar,
    domain: &str,
) -> Result<DomainQuote, DomainError> {
    let request = DomainQuoteRequest {
        domain: domain.to_string(),
    };
    let mut last_error = None;
    for attempt in 1..=EXTERNAL_STEP_MAX_ATTEMPTS {
        info!(%domain, attempt, "checking domain quote");
        match registrar.quote(&request).await {
            Ok(quote) => {
                info!(
                    %domain,
                    attempt,
                    available = quote.available,
                    premium = quote.is_premium,
                    price_usd = %quote.price_usd,
                    "domain quote check succeeded"
                );
                return Ok(quote);
            }
            Err(error)
                if retryable_domain_error(&error) && attempt < EXTERNAL_STEP_MAX_ATTEMPTS =>
            {
                warn!(%domain, attempt, error = %error, "domain quote failed; retrying");
                last_error = Some(error);
                sleep(TokioDuration::from_secs(attempt as u64 * 2)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        DomainError::Namecheap("domain quote failed after retries".to_string())
    }))
}

pub(crate) async fn check_alternatives(
    registrar: &dyn DomainRegistrar,
    base_name: &str,
    exclude_tld: &str,
) -> Vec<DomainQuote> {
    let mut alternatives = Vec::new();
    for tld in crate::domain::SUGGESTED_TLDS {
        if *tld == exclude_tld {
            continue;
        }
        let domain = format!("{base_name}.{tld}");
        let request = DomainQuoteRequest { domain };
        if let Ok(quote) = registrar.quote(&request).await
            && quote.available
            && !quote.is_premium
        {
            alternatives.push(quote);
        }
    }
    alternatives.sort_by_key(|quote| quote.price_usd);
    alternatives
}
```

Update tests to call the functions directly. In `handlers.rs`, import the module and change the two call sites to:

```rust
domain_flow::quote_with_retries(registrar.as_ref(), domain).await
domain_flow::check_alternatives(registrar.as_ref(), base_name, tld).await
```

- [ ] **Step 4: Verify, measure, report, and commit**

Run:

```bash
rg -n 'DomainFlow' src
cargo fmt --check
cargo test telegram::domain_flow::tests -- --nocapture
cargo test
```

Expected: no production `DomainFlow` matches, behavior tests pass, and one
construction/call layer is removed. The raw type count may stay flat because
the required test stub replaces the deleted production type; record that fact
instead of claiming a type-count win. Append the measured result to Batch 2 and run:

```bash
git add src/telegram/domain_flow.rs src/telegram/handlers.rs docs/superpowers/reports/2026-09-19-repository-ablation-results.md
git commit -m "Replace domain flow wrapper with functions"
```

---

### Task 6: Remove the Unread Generation Result Field

**Files:**
- Modify: `src/generation.rs`
- Modify: `docs/superpowers/reports/2026-09-19-repository-ablation-results.md`

**Interfaces:**
- Consumes: queue reads only `SiteGenerationResult.provider_name`.
- Produces: `SiteGenerationResult { provider_name: String }`.

- [ ] **Step 1: Prove the field has no consumer**

Run:

```bash
rg -n 'SiteGenerationResult|result\.output_dir|output_dir:' src/generation.rs src/queue.rs
```

Expected: `SiteGenerationResult.output_dir` is assigned in `run_cli_provider` and never read from a result.

- [ ] **Step 2: Remove the field and its write**

Change the type and successful return to:

```rust
pub struct SiteGenerationResult {
    pub provider_name: String,
}

Ok(SiteGenerationResult {
    provider_name: provider.name.to_string(),
})
```

- [ ] **Step 3: Verify, measure, report, and commit**

Run:

```bash
cargo fmt --check
cargo test generation::tests -- --nocapture
cargo test queue::tests -- --nocapture
cargo test
```

Expected: behavior remains green and at least one public field/source line is removed. Append the result to Batch 2 and run:

```bash
git add src/generation.rs docs/superpowers/reports/2026-09-19-repository-ablation-results.md
git commit -m "Remove unused generation output field"
```

---

### Task 7: Make the Always-Present Database URL Non-Optional

**Files:**
- Modify: `src/config.rs`
- Modify: `src/bin/meme-bot.rs`
- Modify: `docs/superpowers/reports/2026-09-19-repository-ablation-results.md`

**Interfaces:**
- Consumes: `DATABASE_URL`, defaulting to `sqlite://./db.sqlite3`.
- Produces: `Config.database_url: String`, with the same environment/default behavior.

- [ ] **Step 1: Simplify configuration construction**

Change the field and initializer to:

```rust
pub database_url: String,

database_url: env::var("DATABASE_URL")
    .unwrap_or_else(|_| "sqlite://./db.sqlite3".to_string()),
```

- [ ] **Step 2: Remove the impossible missing-value branch**

Replace the bot startup extraction with:

```rust
let connect_opts = SqliteConnectOptions::from_str(&config.database_url)?
    .pragma("foreign_keys", "ON")
    .create_if_missing(true);
```

Keep `.context("connect sqlite")` on pool connection and `.context("run migrations")` on migrations.

- [ ] **Step 3: Verify, measure, report, and commit**

Run:

```bash
cargo fmt --check
cargo test
cargo build --bin meme-bot
```

Expected: default and configured URLs still compile through the same startup path, while one impossible `Option` branch disappears. Append the result to Batch 2 and run:

```bash
git add src/config.rs src/bin/meme-bot.rs docs/superpowers/reports/2026-09-19-repository-ablation-results.md
git commit -m "Make database URL configuration explicit"
```

---

### Task 8: Narrow Dependencies, Root Visibility, and Confirmed Dead Code

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `src/lib.rs`
- Modify: `src/db/mod.rs`
- Modify: `src/telegram/helpers.rs`
- Modify: `docs/superpowers/reports/2026-09-19-repository-ablation-results.md`

**Interfaces:**
- Consumes: imports used by both binaries and internal modules.
- Produces: the same two binaries and REST/Telegram behavior with fewer direct dependencies and externally visible root modules.

- [ ] **Step 1: Remove confirmed dead functions**

Delete `optional_uuid_from_row` and `domain_error_for_user`, including their `#[allow(dead_code)]` attributes. Verify no caller first:

```bash
rg -n 'optional_uuid_from_row|domain_error_for_user' src
```

Expected before deletion: definitions only. Expected after deletion: no matches.

- [ ] **Step 2: Narrow the root module surface**

After Task 3 has removed `app`, keep binary-facing modules public. Keep
`models` public because public database/generation interfaces expose its
types. Narrow only the roots that do not appear in a binary-facing signature:

```rust
pub mod api_client;
pub mod config;
pub mod db;
pub mod deployment;
pub mod domain;
pub mod generation;
pub mod logging;
pub mod models;
mod payment;
pub mod queue;
pub mod telegram;
mod validation;
```

Run strict Clippy immediately after each visibility change. If either private
module creates a `private_interfaces` warning or requires a compensating
re-export, restore that module's public visibility and record the rejected
candidate; `cargo build --bins` and strict Clippy are the authority for this
boundary.

- [ ] **Step 3: Remove the unused direct `tower` dependency**

Delete this line from `Cargo.toml`:

```toml
tower = "0.5"
```

Run `cargo check --all-targets --all-features` so Cargo updates `Cargo.lock` if the resolved graph changes. It is acceptable for `tower` to remain transitive through Axum; the acceptance metric is one fewer direct dependency.

- [ ] **Step 4: Verify, measure, report, and commit Batch 3**

Run:

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --bins
```

Expected: all checks pass, direct dependencies fall from 23 to 22, public-symbol count decreases, and both binaries retain access to required library modules. Append a `Batch 3 — dependency and visibility surface` section with exact metrics and retained/reverted candidates, then run:

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/db/mod.rs src/telegram/helpers.rs docs/superpowers/reports/2026-09-19-repository-ablation-results.md
git commit -m "Reduce dependency and public API surface"
```

---

### Task 9: Final Cross-Batch Verification, Review, and Push

**Files:**
- Modify: `docs/superpowers/reports/2026-09-19-repository-ablation-results.md`

**Interfaces:**
- Consumes: all accepted commits from Tasks 1–8.
- Produces: a complete evidence report and a reviewed, pushed `main` branch.

- [ ] **Step 1: Run the full acceptance suite from a clean process environment**

Run:

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --bin meme-bot
cargo build --bin api-server
```

Expected: every command exits zero without real credentials or network calls.

- [ ] **Step 2: Run schema and retirement audits**

Run:

```bash
cargo test db::migration_tests -- --nocapture
rg -n --glob '!src/db/migration_tests.rs' 'TemplateStore|BuildMode|template_id|build_mode|TEMPLATE_PRICES_USDT|TEMPLATE_PREVIEW_DOMAIN|TEMPLATE_PREVIEW_BASE_URL|LOCAL_TEMPLATES_DIR' src README.md .env.meme-bot.example Cargo.toml
rg -n 'template_mode|TEMPLATE_PRICE_USDT|TEMPLATE_SITE_URL|memewebs\.store|admin_contact' src/telegram README.md
rg -n '^tower[[:space:]]*=' Cargo.toml
```

Expected: migration tests pass; the retired-identifier and direct-`tower` searches have no matches; supported manual-template searches show the fixed UI flow.

- [ ] **Step 3: Complete the final metric comparison**

Re-run every fixed command from Task 1. Add a final table with Baseline, Batch 1, Batch 2, Batch 3, and Final values; list each retained candidate and any candidate reverted during implementation. State explicitly that migration 0002 requires the documented pre-deployment backup.

The Final column must show a net reduction from 11,623 lines, 78 types, 200 public symbols, and 23 direct dependencies. Test count may rise because migration/domain characterization coverage is required.

- [ ] **Step 4: Commit the final measured report if it changed**

Run:

```bash
git add docs/superpowers/reports/2026-09-19-repository-ablation-results.md
git diff --cached --quiet
```

Expected: exit zero when no report delta remains, or exit one when a report
delta is staged. For exit one, run:

```bash
git commit -m "Document repository ablation results"
```

Expected: one final documentation commit.

- [ ] **Step 5: Perform final code review and resolve every finding**

Use `superpowers:requesting-code-review` for a whole-branch review against the spec and this plan. For any finding, use `superpowers:receiving-code-review`, reproduce the concern, fix it with the smallest patch, and rerun the relevant targeted test plus the full acceptance suite.

- [ ] **Step 6: Verify repository state and push**

Use `superpowers:verification-before-completion`, then run:

```bash
git status --short
git log --oneline 0cd5e68..HEAD
git push origin main
```

Expected: the worktree is clean, the log contains the design plus accepted ablation commits, and `origin/main` advances to the reviewed final commit.
