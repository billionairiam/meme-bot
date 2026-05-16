use super::*;

#[derive(Clone)]
pub struct TemplateStore {
    pub pool: SqlitePool,
}

impl TemplateStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn sync_catalog(
        &self,
        preview_domain: &str,
        preview_base_url: &str,
        prices: &[Decimal],
    ) -> anyhow::Result<u64> {
        let mut tx = self.pool.begin().await?;
        let mut changed = 0;

        for (index, &price) in prices.iter().enumerate() {
            let number = index + 1;
            let number = Self::format_template_number(number);
            let template_id = format!("template_{number}");
            let name = format!("{number}.{preview_domain}");
            let local_path = template_id.clone();
            let preview_url = format!("{}/{}", preview_base_url.trim_end_matches('/'), number);

            let result = sqlx::query(
                r#"
            INSERT INTO templates (
                id, template_id, name, local_path, preview_url, price_tier, enabled, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, 1, datetime('now'))
            ON CONFLICT(template_id) DO UPDATE SET
                name = EXCLUDED.name,
                local_path = EXCLUDED.local_path,
                preview_url = EXCLUDED.preview_url,
                price_tier = EXCLUDED.price_tier,
                enabled = 1,
                updated_at = datetime('now')
            "#,
            )
            .bind(uuid_value(Uuid::new_v4()))
            .bind(&template_id)
            .bind(&name)
            .bind(&local_path)
            .bind(&preview_url)
            .bind(price.to_string())
            .execute(&mut *tx)
            .await?;
            changed += result.rows_affected();
        }

        tx.commit().await?;
        Ok(changed)
    }

    fn format_template_number(template_number: usize) -> String {
        if template_number < 100 {
            format!("{template_number:02}")
        } else {
            template_number.to_string()
        }
    }
}
