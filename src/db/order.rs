use super::*;

#[derive(Clone)]
pub struct OrderStore {
    pub pool: SqlitePool,
}

impl OrderStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
    /// Create a new order in need_chain status. Fails if user already has an
    /// unpaid order (need_logo ~ need_pay).
    pub async fn create(
        &self,
        user_id: Uuid,
        build_mode: BuildMode,
        template_id: Option<&str>,
        site_price_usdt: Decimal,
    ) -> anyhow::Result<Uuid> {
        let mut tx = self.pool.begin().await?;

        if let Some(template_id) = template_id {
            sqlx::query(
                r#"
            INSERT INTO templates (
                id, template_id, name, local_path, price_tier, updated_at
            )
            VALUES (?, ?, ?, ?, ?, datetime('now'))
            ON CONFLICT(template_id) DO UPDATE SET
                price_tier = EXCLUDED.price_tier,
                updated_at = datetime('now')
            "#,
            )
            .bind(uuid_value(Uuid::new_v4()))
            .bind(template_id)
            .bind(template_id)
            .bind(template_id)
            .bind(site_price_usdt.to_string())
            .execute(&mut *tx)
            .await?;
        }

        let id = Uuid::new_v4();
        let row = sqlx::query(
            r#"
        INSERT INTO orders (
            id, user_id, build_mode, template_id, status,
            site_price_usdt, total_usdt
        )
        VALUES (?, ?, ?, ?, 'need_chain', ?, ?)
        RETURNING id
        "#,
        )
        .bind(uuid_value(id))
        .bind(uuid_value(user_id))
        .bind(build_mode.as_str())
        .bind(template_id)
        .bind(site_price_usdt.to_string())
        .bind(site_price_usdt.to_string())
        .fetch_one(&mut *tx)
        .await?;

        let id = uuid_from_row(&row, "id")?;
        tx.commit().await?;
        Ok(id)
    }
    /// Check if user has an unpaid (wizard/payment) order.
    pub async fn has_unpaid(&self, telegram_user_id: i64) -> anyhow::Result<bool> {
        let row = sqlx::query(
            r#"
        SELECT 1
        FROM orders o
        JOIN users u ON o.user_id = u.id
        WHERE u.telegram_user_id = ?
          AND o.status IN ('need_chain', 'need_logo', 'need_ticker', 'need_name', 'need_contract',
                           'need_x_link', 'need_tme', 'need_domain', 'need_pay')
        LIMIT 1
        "#,
        )
        .bind(telegram_user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    /// Get the user's current unpaid order (wizard or payment step).
    pub async fn get_unpaid(&self, telegram_user_id: i64) -> anyhow::Result<Option<Order>> {
        let row = sqlx::query(
            r#"
        SELECT o.*
        FROM orders o
        JOIN users u ON o.user_id = u.id
        WHERE u.telegram_user_id = ?
          AND o.status IN ('need_chain', 'need_logo', 'need_ticker', 'need_name', 'need_contract',
                           'need_x_link', 'need_tme', 'need_domain', 'need_pay')
        ORDER BY o.created_at DESC
        LIMIT 1
        "#,
        )
        .bind(telegram_user_id)
        .fetch_optional(&self.pool)
        .await?;

        row.as_ref().map(Self::order_from_row).transpose()
    }

    /// Get a specific order by ID.
    pub async fn get(&self, order_id: Uuid) -> anyhow::Result<Option<Order>> {
        let row = sqlx::query("SELECT * FROM orders WHERE id = ?")
            .bind(uuid_value(order_id))
            .fetch_optional(&self.pool)
            .await?;

        row.as_ref().map(Self::order_from_row).transpose()
    }

    /// Update the order status (wizard step transitions).
    pub async fn update_status(&self, order_id: Uuid, status: OrderStatus) -> anyhow::Result<()> {
        let result =
            sqlx::query("UPDATE orders SET status = ?, updated_at = datetime('now') WHERE id = ?")
                .bind(status.as_str())
                .bind(uuid_value(order_id))
                .execute(&self.pool)
                .await?;
        if result.rows_affected() != 1 {
            anyhow::bail!("order does not exist: {order_id}");
        }
        Ok(())
    }

    /// Update a wizard field on the order.
    pub async fn update_field(
        &self,
        order_id: Uuid,
        field: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        let allowed = [
            "token_ticker",
            "token_chain",
            "token_name",
            "contract_address",
            "x_url",
            "telegram_url",
            "logo_asset_path",
            "requested_domain",
            "alternative_domains_json",
        ];
        if !allowed.contains(&field) {
            anyhow::bail!("disallowed field: {field}");
        }
        let sql =
            format!("UPDATE orders SET {field} = ?, updated_at = datetime('now') WHERE id = ?");
        sqlx::query(&sql)
            .bind(value)
            .bind(uuid_value(order_id))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Update a wizard field and advance order status in one query.
    pub async fn update_field_and_status(
        &self,
        order_id: Uuid,
        field: &str,
        value: &str,
        next_status: OrderStatus,
    ) -> anyhow::Result<()> {
        let allowed = [
            "token_ticker",
            "token_chain",
            "token_name",
            "contract_address",
            "x_url",
            "telegram_url",
            "logo_asset_path",
            "requested_domain",
            "alternative_domains_json",
        ];
        if !allowed.contains(&field) {
            anyhow::bail!("disallowed field: {field}");
        }
        let sql = format!(
            "UPDATE orders SET {field} = ?, status = ?, updated_at = datetime('now') WHERE id = ?"
        );
        sqlx::query(&sql)
            .bind(value)
            .bind(next_status.as_str())
            .bind(uuid_value(order_id))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Clear a nullable field on an order (set to NULL).
    pub async fn clear_field(&self, order_id: Uuid, field: &str) -> anyhow::Result<()> {
        let allowed = ["alternative_domains_json"];
        if !allowed.contains(&field) {
            anyhow::bail!("disallowed clear field: {field}");
        }
        let sql =
            format!("UPDATE orders SET {field} = NULL, updated_at = datetime('now') WHERE id = ?");
        sqlx::query(&sql)
            .bind(uuid_value(order_id))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Set domain pricing on an order.
    pub async fn set_domain_pricing(
        &self,
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
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Cancel the user's unpaid order.
    pub async fn cancel_for_user(
        &self,
        telegram_user_id: i64,
        order_id: Option<Uuid>,
    ) -> anyhow::Result<u64> {
        let mut sql = r#"
        UPDATE orders
        SET status = 'cancelled', updated_at = datetime('now')
        WHERE user_id IN (SELECT id FROM users WHERE telegram_user_id = ?)
          AND status IN ('need_chain', 'need_logo', 'need_ticker', 'need_name', 'need_contract',
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
        let result = query.execute(&self.pool).await?;
        Ok(result.rows_affected())
    }

    /// Expire stale pending orders (asset collection timeout or payment window expired).
    pub async fn expire_stale_pending_for_user(
        &self,
        telegram_user_id: i64,
    ) -> anyhow::Result<u64> {
        let result = sqlx::query(
            r#"
        UPDATE orders
        SET status = 'cancelled', updated_at = datetime('now')
        WHERE user_id IN (SELECT id FROM users WHERE telegram_user_id = ?)
          AND status IN ('need_chain', 'need_logo', 'need_ticker', 'need_name', 'need_contract',
                         'need_x_link', 'need_tme', 'need_domain', 'need_pay')
          AND (
            datetime(updated_at) < datetime('now', ?)
            OR (payment_expires_at IS NOT NULL AND datetime(payment_expires_at) < datetime('now'))
          )
        "#,
        )
        .bind(telegram_user_id)
        .bind(format!("-{ASSET_COLLECTION_TIMEOUT_MINUTES} minutes"))
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
    pub async fn attach_payment_chain(
        &self,
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
        .fetch_optional(&self.pool)
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
    pub async fn mark_paid(
        &self,
        order_id: Uuid,
        chain: PaymentChain,
        tx_hash: &str,
    ) -> anyhow::Result<()> {
        let canonical = Self::canonical_tx_hash(chain, tx_hash);
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
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            anyhow::bail!("order is not in need_pay status or does not exist");
        }
        Ok(())
    }

    /// Check if a TX hash has already been used by any paid order.
    pub async fn tx_hash_already_used(
        &self,
        chain: PaymentChain,
        tx_hash: &str,
    ) -> anyhow::Result<bool> {
        let canonical = Self::canonical_tx_hash(chain, tx_hash);
        let row = sqlx::query(
            r#"
        SELECT COUNT(*) AS count
        FROM orders
        WHERE (tx_hash = ? OR edit_tx_hash = ?)
          AND status NOT IN ('cancelled')
        "#,
        )
        .bind(&canonical)
        .bind(&canonical)
        .fetch_one(&self.pool)
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
    pub async fn get_active(&self, telegram_user_id: i64) -> anyhow::Result<Vec<Order>> {
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
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(Self::order_from_row).collect()
    }

    /// Get completed orders for /history.
    pub async fn get_completed(
        &self,
        telegram_user_id: i64,
        limit: i64,
    ) -> anyhow::Result<Vec<OrderWithWebsite>> {
        let rows = sqlx::query(
            r#"
        SELECT o.id AS order_id, o.token_chain, o.token_ticker, o.token_name,
               o.contract_address, o.x_url, o.telegram_url, o.requested_domain,
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
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|row| {
                Ok(OrderWithWebsite {
                    order_id: uuid_from_row(row, "order_id")?,
                    token_chain: row
                        .get::<Option<String>, _>("token_chain")
                        .as_deref()
                        .and_then(TokenChain::from_db),
                    token_ticker: row.get("token_ticker"),
                    token_name: row.get("token_name"),
                    contract_address: row.get("contract_address"),
                    x_url: row.get("x_url"),
                    telegram_url: row.get("telegram_url"),
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
    pub async fn get_completed_for_user(
        &self,
        telegram_user_id: i64,
        order_id: Uuid,
    ) -> anyhow::Result<Option<Order>> {
        let row = sqlx::query(
            r#"
        SELECT o.*
        FROM orders o
        JOIN users u ON o.user_id = u.id
        WHERE u.telegram_user_id = ?
          AND o.id = ?
          AND o.status = 'done'
        LIMIT 1
        "#,
        )
        .bind(telegram_user_id)
        .bind(uuid_value(order_id))
        .fetch_optional(&self.pool)
        .await?;

        row.as_ref().map(Self::order_from_row).transpose()
    }

    pub async fn get_active_edit(&self, telegram_user_id: i64) -> anyhow::Result<Option<Order>> {
        let row = sqlx::query(
            r#"
        SELECT o.*
        FROM orders o
        JOIN users u ON o.user_id = u.id
        WHERE u.telegram_user_id = ?
          AND o.status = 'done'
          AND o.edit_status IN ('draft', 'need_pay', 'applying')
        ORDER BY o.updated_at DESC
        LIMIT 1
        "#,
        )
        .bind(telegram_user_id)
        .fetch_optional(&self.pool)
        .await?;

        row.as_ref().map(Self::order_from_row).transpose()
    }

    pub async fn start_edit_draft(&self, order_id: Uuid) -> anyhow::Result<()> {
        let result = sqlx::query(
            r#"
        UPDATE orders
        SET pending_contract_address = contract_address,
            pending_x_url = x_url,
            pending_telegram_url = telegram_url,
            pending_edit_field = NULL,
            edit_status = 'draft',
            edit_payment_chain = NULL,
            edit_tx_hash = NULL,
            edit_payment_expires_at = NULL,
            edit_paid_at = NULL,
            updated_at = datetime('now')
        WHERE id = ? AND status = 'done'
        "#,
        )
        .bind(uuid_value(order_id))
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            anyhow::bail!("completed order does not exist: {order_id}");
        }
        Ok(())
    }

    pub async fn set_pending_edit_field(
        &self,
        order_id: Uuid,
        field: Option<&str>,
    ) -> anyhow::Result<()> {
        if let Some(field) = field
            && !matches!(field, "contract" | "x" | "telegram")
        {
            anyhow::bail!("disallowed edit field: {field}");
        }
        sqlx::query(
            r#"
        UPDATE orders
        SET pending_edit_field = ?, edit_status = 'draft', updated_at = datetime('now')
        WHERE id = ? AND status = 'done'
        "#,
        )
        .bind(field)
        .bind(uuid_value(order_id))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_pending_edit_value(
        &self,
        order_id: Uuid,
        field: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        let column = match field {
            "contract" => "pending_contract_address",
            "x" => "pending_x_url",
            "telegram" => "pending_telegram_url",
            other => anyhow::bail!("disallowed edit field: {other}"),
        };
        let sql = format!(
            "UPDATE orders SET {column} = ?, pending_edit_field = NULL, edit_status = 'draft', updated_at = datetime('now') WHERE id = ? AND status = 'done'"
        );
        sqlx::query(&sql)
            .bind(value)
            .bind(uuid_value(order_id))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn clear_edit_draft(&self, order_id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            r#"
        UPDATE orders
        SET pending_contract_address = NULL,
            pending_x_url = NULL,
            pending_telegram_url = NULL,
            pending_edit_field = NULL,
            edit_status = 'none',
            edit_payment_chain = NULL,
            edit_tx_hash = NULL,
            edit_payment_expires_at = NULL,
            edit_paid_at = NULL,
            updated_at = datetime('now')
        WHERE id = ?
        "#,
        )
        .bind(uuid_value(order_id))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn user_edit_success_count(&self, telegram_user_id: i64) -> anyhow::Result<i64> {
        let row = sqlx::query(
            r#"
        SELECT COALESCE(SUM(o.edit_success_count), 0) AS count
        FROM orders o
        JOIN users u ON o.user_id = u.id
        WHERE u.telegram_user_id = ?
        "#,
        )
        .bind(telegram_user_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get("count"))
    }

    pub async fn mark_edit_need_pay(&self, order_id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            r#"
        UPDATE orders
        SET edit_status = 'need_pay',
            edit_payment_chain = NULL,
            edit_tx_hash = NULL,
            edit_payment_expires_at = NULL,
            updated_at = datetime('now')
        WHERE id = ? AND status = 'done'
        "#,
        )
        .bind(uuid_value(order_id))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn attach_edit_payment_chain(
        &self,
        order_id: Uuid,
        chain: PaymentChain,
    ) -> anyhow::Result<PaymentWindow> {
        let now = Utc::now();
        let expires_at = now + Duration::minutes(PAYMENT_WINDOW_MINUTES);
        let row = sqlx::query(
            r#"
        UPDATE orders
        SET edit_payment_chain = ?,
            edit_payment_expires_at = COALESCE(edit_payment_expires_at, ?),
            updated_at = datetime('now')
        WHERE id = ?
          AND status = 'done'
          AND edit_status = 'need_pay'
        RETURNING edit_payment_expires_at
        "#,
        )
        .bind(chain.as_str())
        .bind(expires_at.to_rfc3339())
        .bind(uuid_value(order_id))
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            anyhow::bail!("edit is not waiting for payment or does not exist");
        };
        Ok(PaymentWindow {
            started_at: now,
            expires_at: parse_db_datetime(row.get::<&str, _>("edit_payment_expires_at"))?,
        })
    }

    pub async fn mark_edit_paid(
        &self,
        order_id: Uuid,
        chain: PaymentChain,
        tx_hash: &str,
    ) -> anyhow::Result<()> {
        let canonical = Self::canonical_tx_hash(chain, tx_hash);
        let result = sqlx::query(
            r#"
        UPDATE orders
        SET edit_payment_chain = ?,
            edit_tx_hash = ?,
            edit_paid_at = datetime('now'),
            edit_status = 'applying',
            updated_at = datetime('now')
        WHERE id = ? AND status = 'done' AND edit_status = 'need_pay'
        "#,
        )
        .bind(chain.as_str())
        .bind(&canonical)
        .bind(uuid_value(order_id))
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            anyhow::bail!("edit is not waiting for payment or does not exist");
        }
        Ok(())
    }

    pub async fn mark_edit_applying(&self, order_id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
        "UPDATE orders SET edit_status = 'applying', updated_at = datetime('now') WHERE id = ? AND status = 'done'",
    )
    .bind(uuid_value(order_id))
    .execute(&self.pool)
    .await?;
        Ok(())
    }

    pub async fn mark_edit_success_and_update_fields(
        &self,
        order_id: Uuid,
        contract_address: &str,
        x_url: &str,
        telegram_url: &str,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r#"
        UPDATE orders
        SET contract_address = ?,
            x_url = ?,
            telegram_url = ?,
            pending_contract_address = NULL,
            pending_x_url = NULL,
            pending_telegram_url = NULL,
            pending_edit_field = NULL,
            edit_status = 'none',
            edit_payment_chain = NULL,
            edit_tx_hash = NULL,
            edit_payment_expires_at = NULL,
            edit_paid_at = NULL,
            edit_success_count = edit_success_count + 1,
            updated_at = datetime('now')
        WHERE id = ? AND status = 'done'
        "#,
        )
        .bind(contract_address)
        .bind(x_url)
        .bind(telegram_url)
        .bind(uuid_value(order_id))
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
        UPDATE websites
        SET contract_address = ?,
            x_url = ?,
            telegram_url = ?,
            updated_at = datetime('now')
        WHERE order_id = ?
        "#,
        )
        .bind(contract_address)
        .bind(x_url)
        .bind(telegram_url)
        .bind(uuid_value(order_id))
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    pub async fn mark_edit_manual_review(&self, order_id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
        "UPDATE orders SET edit_status = 'need_manual_review', updated_at = datetime('now') WHERE id = ?",
    )
    .bind(uuid_value(order_id))
    .execute(&self.pool)
    .await?;
        Ok(())
    }

    // ============================================================
    // Generation queue (operates directly on orders table)
    // ============================================================

    /// Lock the next `paid_and_generating` order for generation processing.
    /// Returns the order with all fields needed for generation.
    pub async fn lock_next_generation(
        &self,
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
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let order = Self::order_from_row(&row)?;

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
        .execute(&self.pool)
        .await?;

        if update.rows_affected() != 1 {
            return Ok(None);
        }

        // Re-read to get updated attempt_count
        self.get(order.id).await
    }

    /// Reclaim stale running orders that have been locked too long.
    pub async fn reclaim_stale_locked(
        &self,
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
        .fetch_all(&self.pool)
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
        .fetch_all(&self.pool)
        .await?;

        for row in &review_rows {
            results.push((uuid_from_row(row, "id")?, true));
        }

        Ok(results)
    }

    /// Mark order as done (generation + deployment complete).
    pub async fn mark_done(&self, order_id: Uuid) -> anyhow::Result<()> {
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
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark order as needing manual review (generation or deployment failed).
    pub async fn mark_manual_review(&self, order_id: Uuid) -> anyhow::Result<()> {
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
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Requeue a failed order for retry (unlock, keep status as paid_and_generating).
    pub async fn requeue_for_retry(&self, order_id: Uuid) -> anyhow::Result<()> {
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
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Reset stuck paid_and_generating orders on startup (clear locks so they get retried).
    pub async fn reset_generating_on_startup(&self) -> anyhow::Result<u64> {
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
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

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
            token_chain: row
                .get::<Option<String>, _>("token_chain")
                .as_deref()
                .and_then(TokenChain::from_db),
            token_ticker: row.get("token_ticker"),
            token_name: row.get("token_name"),
            contract_address: row.get("contract_address"),
            x_url: row.get("x_url"),
            telegram_url: row.get("telegram_url"),
            logo_asset_path: row.get("logo_asset_path"),
            requested_domain: row.get("requested_domain"),
            alternative_domains_json: row.get("alternative_domains_json"),
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
            pending_contract_address: row.get("pending_contract_address"),
            pending_x_url: row.get("pending_x_url"),
            pending_telegram_url: row.get("pending_telegram_url"),
            pending_edit_field: row.get("pending_edit_field"),
            edit_status: row.get("edit_status"),
            edit_payment_chain: row
                .get::<Option<String>, _>("edit_payment_chain")
                .as_deref()
                .and_then(PaymentChain::from_db),
            edit_tx_hash: row.get("edit_tx_hash"),
            edit_payment_expires_at: row
                .get::<Option<String>, _>("edit_payment_expires_at")
                .as_deref()
                .map(parse_db_datetime)
                .transpose()?,
            edit_paid_at: row
                .get::<Option<String>, _>("edit_paid_at")
                .as_deref()
                .map(parse_db_datetime)
                .transpose()?,
            edit_success_count: row.get("edit_success_count"),
            attempt_count: row.get("attempt_count"),
            created_at: parse_db_datetime(row.get::<&str, _>("created_at"))?,
            updated_at: parse_db_datetime(row.get::<&str, _>("updated_at"))?,
        })
    }
}

pub struct PaymentWindow {
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}
pub struct OrderWithWebsite {
    pub order_id: Uuid,
    pub token_chain: Option<TokenChain>,
    pub token_ticker: Option<String>,
    pub token_name: Option<String>,
    pub contract_address: Option<String>,
    pub x_url: Option<String>,
    pub telegram_url: Option<String>,
    pub requested_domain: Option<String>,
    pub total_usdt: Decimal,
    pub payment_chain: Option<String>,
    pub paid_at: Option<DateTime<Utc>>,
    pub cloudflare_pages_url: Option<String>,
    pub custom_domain: Option<String>,
    pub github_repo_url: Option<String>,
}
pub const FREE_EDIT_LIMIT: i64 = 3;
pub const EDIT_PRICE_USDT: i64 = 10;
