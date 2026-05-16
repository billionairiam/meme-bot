use super::*;

#[derive(Clone)]
pub struct WebsiteStore {
    pub pool: SqlitePool,
}

impl WebsiteStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Create a website record when deployment succeeds.
    pub async fn create(
        &self,
        user_id: Uuid,
        order_id: Uuid,
        input: &WebsiteInput,
        generation_provider: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        let row = sqlx::query(
            r#"
        INSERT INTO websites (
            id, user_id, order_id, token_chain, token_ticker, token_name, contract_address,
            x_url, telegram_url, logo_asset_path, requested_domain,
            generation_provider_used
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(order_id) DO UPDATE SET
            generation_provider_used = EXCLUDED.generation_provider_used,
            updated_at = datetime('now')
        RETURNING id
        "#,
        )
        .bind(uuid_value(id))
        .bind(uuid_value(user_id))
        .bind(uuid_value(order_id))
        .bind(input.token_chain.as_str())
        .bind(&input.token_ticker)
        .bind(&input.token_name)
        .bind(&input.contract_address)
        .bind(&input.x_url)
        .bind(&input.telegram_url)
        .bind(&input.logo_asset_path)
        .bind(&input.requested_domain)
        .bind(generation_provider)
        .fetch_one(&self.pool)
        .await?;

        uuid_from_row(&row, "id")
    }

    /// Update website with deployment results and mark order as done.
    pub async fn update_deployment_success(
        &self,
        order_id: Uuid,
        deployment: &DeploymentResult,
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;

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
}
