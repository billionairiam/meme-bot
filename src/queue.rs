use std::sync::Arc;

use sqlx::{PgPool, Row};
use tokio::sync::Semaphore;
use tracing::{error, info};
use uuid::Uuid;

use crate::{
    deployment::{DeploymentRequest, SiteDeployer},
    generation::{GenerationError, GenerationRouter, SiteGenerationRequest},
};

pub struct GenerationQueue {
    pool: PgPool,
    router: Arc<GenerationRouter>,
    deployer: Option<Arc<dyn SiteDeployer>>,
    semaphore: Arc<Semaphore>,
    worker_id: String,
}

impl GenerationQueue {
    pub fn new(
        pool: PgPool,
        router: Arc<GenerationRouter>,
        deployer: Option<Arc<dyn SiteDeployer>>,
        max_concurrency: usize,
        worker_id: String,
    ) -> Self {
        Self {
            pool,
            router,
            deployer,
            semaphore: Arc::new(Semaphore::new(max_concurrency)),
            worker_id,
        }
    }

    pub async fn tick(&self) -> anyhow::Result<()> {
        let _permit = self.semaphore.acquire().await?;
        let Some(job) = self.lock_next_job().await? else {
            return Ok(());
        };

        info!(job_id = %job.job_id, order_id = %job.order_id, "running generation job");
        let job_id = job.job_id;
        let request = job.into_generation_request();
        sqlx::query(
            "UPDATE orders SET site_status = 'generating', updated_at = now() WHERE id = $1",
        )
        .bind(request.order_id)
        .execute(&self.pool)
        .await?;

        match self.router.generate(&request).await {
            Ok(result) => {
                sqlx::query(
                    r#"
                    UPDATE generation_jobs
                    SET status = 'succeeded', finished_at = now(), locked_by = NULL, locked_at = NULL
                    WHERE id = $1
                    "#,
                )
                .bind(job_id)
                .execute(&self.pool)
                .await?;

                sqlx::query(
                    r#"
                    UPDATE websites
                    SET generation_provider_used = $1, deployment_status = 'generated', updated_at = now()
                    WHERE order_id = $2
                    "#,
                )
                .bind(result.provider_name)
                .bind(request.order_id)
                .execute(&self.pool)
                .await?;

                if let Some(deployer) = &self.deployer {
                    sqlx::query(
                        "UPDATE orders SET site_status = 'deploying', updated_at = now() WHERE id = $1",
                    )
                    .bind(request.order_id)
                    .execute(&self.pool)
                    .await?;
                    let deployment = deployer.deploy(&request.deployment_request()).await;
                    match deployment {
                        Ok(deployment) => {
                            sqlx::query(
                                r#"
                                UPDATE websites
                                SET cloudflare_pages_url = $1,
                                    custom_domain = $2,
                                    deployment_status = 'live',
                                    updated_at = now()
                                WHERE order_id = $3
                                "#,
                            )
                            .bind(deployment.cloudflare_pages_url)
                            .bind(deployment.custom_domain)
                            .bind(request.order_id)
                            .execute(&self.pool)
                            .await?;
                            sqlx::query(
                                "UPDATE orders SET site_status = 'live', updated_at = now() WHERE id = $1",
                            )
                            .bind(request.order_id)
                            .execute(&self.pool)
                            .await?;
                        }
                        Err(error) => {
                            error!(order_id = %request.order_id, error = %error, "deployment failed");
                            sqlx::query(
                                r#"
                                UPDATE websites
                                SET deployment_status = 'deployment_failed', updated_at = now()
                                WHERE order_id = $1
                                "#,
                            )
                            .bind(request.order_id)
                            .execute(&self.pool)
                            .await?;
                            self.mark_failed(job_id, request.order_id).await?;
                        }
                    }
                } else {
                    sqlx::query(
                        "UPDATE orders SET site_status = 'deploying', updated_at = now() WHERE id = $1",
                    )
                    .bind(request.order_id)
                    .execute(&self.pool)
                    .await?;
                }
            }
            Err(GenerationError::QuotaExhausted | GenerationError::ProviderUnavailable) => {
                self.mark_manual_review(job_id, request.order_id).await?;
            }
            Err(error) => {
                error!(order_id = %request.order_id, error = %error, "generation failed");
                self.mark_failed(job_id, request.order_id).await?;
            }
        }
        Ok(())
    }

    async fn lock_next_job(&self) -> anyhow::Result<Option<GenerationJobRow>> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            r#"
            SELECT gj.id AS job_id, gj.order_id, gj.website_id,
                   o.build_mode, o.template_id,
                   w.token_ticker, w.token_name, w.contract_address,
                   w.x_url, w.telegram_url, w.logo_asset_path, w.requested_domain
            FROM generation_jobs gj
            JOIN orders o ON o.id = gj.order_id
            JOIN websites w ON w.id = gj.website_id
            WHERE gj.status = 'queued'
              AND o.payment_status = 'paid'
            ORDER BY gj.priority DESC, gj.queued_at ASC
            FOR UPDATE SKIP LOCKED
            LIMIT 1
            "#,
        )
        .fetch_optional(&mut *tx)
        .await?;

        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };

        let job_id: Uuid = row.get("job_id");
        sqlx::query(
            r#"
            UPDATE generation_jobs
            SET status = 'running', started_at = COALESCE(started_at, now()),
                locked_by = $1, locked_at = now(), attempt_count = attempt_count + 1
            WHERE id = $2
            "#,
        )
        .bind(&self.worker_id)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(Some(GenerationJobRow {
            job_id,
            order_id: row.get("order_id"),
            website_id: row.get("website_id"),
            build_mode: row
                .get::<Option<String>, _>("build_mode")
                .unwrap_or_default(),
            template_id: row.get("template_id"),
            token_ticker: row.get("token_ticker"),
            token_name: row.get("token_name"),
            contract_address: row.get("contract_address"),
            x_url: row.get("x_url"),
            telegram_url: row.get("telegram_url"),
            logo_asset_path: row.get("logo_asset_path"),
            requested_domain: row.get("requested_domain"),
        }))
    }

    async fn mark_manual_review(&self, job_id: Uuid, order_id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE orders SET site_status = 'needs_manual_review', updated_at = now() WHERE id = $1",
        )
        .bind(order_id)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            r#"
            UPDATE generation_jobs
            SET status = 'needs_manual_review', finished_at = now(), locked_by = NULL, locked_at = NULL
            WHERE id = $1
            "#,
        )
        .bind(job_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn mark_failed(&self, job_id: Uuid, order_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("UPDATE orders SET site_status = 'failed', updated_at = now() WHERE id = $1")
            .bind(order_id)
            .execute(&self.pool)
            .await?;
        sqlx::query(
            r#"
            UPDATE generation_jobs
            SET status = 'failed', finished_at = now(), locked_by = NULL, locked_at = NULL
            WHERE id = $1
            "#,
        )
        .bind(job_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

impl SiteGenerationRequest {
    fn deployment_request(&self) -> DeploymentRequest {
        let domain = self.input.requested_domain.clone();
        let project_name = domain
            .as_deref()
            .map(|domain| project_name_from_domain(domain, self.order_id))
            .unwrap_or_else(|| format!("site-{}", &self.order_id.simple().to_string()[..8]));

        DeploymentRequest {
            repo_owner: "direct-upload".to_string(),
            repo_name: project_name.clone(),
            github_token: String::new(),
            site_dir: self.output_dir.display().to_string(),
            cloudflare_project_name: project_name,
            production_branch: "main".to_string(),
            custom_domain: domain,
        }
    }
}

fn project_name_from_domain(domain: &str, order_id: Uuid) -> String {
    let base = domain
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    let suffix = order_id.simple().to_string();
    format!("{}-{}", base, &suffix[..8])
        .chars()
        .take(58)
        .collect()
}

struct GenerationJobRow {
    job_id: Uuid,
    order_id: Uuid,
    website_id: Uuid,
    build_mode: String,
    template_id: Option<String>,
    token_ticker: String,
    token_name: String,
    contract_address: String,
    x_url: String,
    telegram_url: String,
    logo_asset_path: String,
    requested_domain: Option<String>,
}

impl GenerationJobRow {
    fn into_generation_request(self) -> SiteGenerationRequest {
        SiteGenerationRequest {
            order_id: self.order_id,
            build_mode: self.build_mode,
            template_local_path: self.template_id,
            input: crate::models::WebsiteInput {
                token_ticker: self.token_ticker,
                token_name: self.token_name,
                contract_address: self.contract_address,
                x_url: self.x_url,
                telegram_url: self.telegram_url,
                logo_asset_path: self.logo_asset_path,
                requested_domain: self.requested_domain,
            },
            output_dir: format!("./generated-sites/{}", self.website_id).into(),
        }
    }
}
