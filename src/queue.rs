use std::sync::Arc;

use sqlx::{Row, SqlitePool};
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio::time::{Duration, sleep};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    db,
    deployment::{DeploymentRequest, SiteDeployer},
    generation::{GenerationError, GenerationRouter, SiteGenerationRequest},
};

const MAX_JOB_ATTEMPTS: i64 = 3;
const STALE_RUNNING_JOB_MINUTES: i64 = 120;

#[derive(Clone, Debug)]
pub struct UserNotification {
    pub telegram_user_id: i64,
    pub message: String,
}

pub struct GenerationQueueConfig {
    pub max_concurrency: usize,
    pub worker_id: String,
    pub repo_owner: String,
    pub github_token: String,
}

pub struct GenerationQueue {
    pool: SqlitePool,
    router: Arc<GenerationRouter>,
    deployer: Option<Arc<dyn SiteDeployer>>,
    repo_owner: String,
    github_token: String,
    semaphore: Arc<Semaphore>,
    lock_mutex: Arc<Mutex<()>>,
    worker_id: String,
    notification_tx: Option<mpsc::UnboundedSender<UserNotification>>,
}

impl GenerationQueue {
    pub fn new(
        pool: SqlitePool,
        router: Arc<GenerationRouter>,
        deployer: Option<Arc<dyn SiteDeployer>>,
        config: GenerationQueueConfig,
        notification_tx: Option<mpsc::UnboundedSender<UserNotification>>,
    ) -> Self {
        Self {
            pool,
            router,
            deployer,
            repo_owner: config.repo_owner,
            github_token: config.github_token,
            semaphore: Arc::new(Semaphore::new(config.max_concurrency.max(1))),
            lock_mutex: Arc::new(Mutex::new(())),
            worker_id: config.worker_id,
            notification_tx,
        }
    }

    pub async fn tick(&self) -> anyhow::Result<()> {
        let _permit = self.semaphore.acquire().await?;
        self.reclaim_stale_running_jobs().await?;
        let Some(job) = self.lock_next_job().await? else {
            return Ok(());
        };

        info!(job_id = %job.job_id, order_id = %job.order_id, "running generation job");
        let job_id = job.job_id;
        let website_id = job.website_id;
        let attempt_count = job.attempt_count;
        let request = job.into_generation_request();
        sqlx::query(
            "UPDATE orders SET site_status = 'generating', updated_at = datetime('now') WHERE id = ?",
        )
        .bind(request.order_id)
        .execute(&self.pool)
        .await?;

        match self.router.generate(&request).await {
            Ok(result) => {
                sqlx::query(
                    r#"
                    UPDATE generation_jobs
                    SET status = 'succeeded', finished_at = datetime('now'), locked_by = NULL, locked_at = NULL
                    WHERE id = ?
                    "#,
                )
                .bind(job_id)
                .execute(&self.pool)
                .await?;

                sqlx::query(
                    r#"
                    UPDATE websites
                    SET generation_provider_used = ?, deployment_status = 'generated', updated_at = datetime('now')
                    WHERE order_id = ?
                    "#,
                )
                .bind(result.provider_name)
                .bind(request.order_id)
                .execute(&self.pool)
                .await?;

                if let Some(deployer) = &self.deployer {
                    sqlx::query(
                        "UPDATE orders SET site_status = 'deploying', updated_at = datetime('now') WHERE id = ?",
                    )
                    .bind(request.order_id)
                    .execute(&self.pool)
                    .await?;
                    sqlx::query(
                        "UPDATE websites SET deployment_status = 'deploying', updated_at = datetime('now') WHERE id = ?",
                    )
                    .bind(website_id)
                    .execute(&self.pool)
                    .await?;
                    let deployment_request =
                        request.deployment_request(&self.repo_owner, &self.github_token);
                    let deployment = self
                        .deploy_with_retries(deployer.as_ref(), &deployment_request, website_id)
                        .await;
                    match deployment {
                        Ok(deployment) => {
                            for stage in [
                                "github_create",
                                "git_push",
                                "pages_create",
                                "pages_deploy",
                                "domain_bind",
                            ] {
                                db::record_deployment_attempt(
                                    &self.pool,
                                    website_id,
                                    stage,
                                    "succeeded",
                                    None,
                                )
                                .await?;
                            }
                            db::update_website_deployment_success(
                                &self.pool,
                                request.order_id,
                                &deployment,
                            )
                            .await?;
                            sqlx::query(
                                "UPDATE orders SET site_status = 'live', updated_at = datetime('now') WHERE id = ?",
                            )
                            .bind(request.order_id)
                            .execute(&self.pool)
                            .await?;

                            let site_url = deployment
                                .custom_domain
                                .as_deref()
                                .map(|d| format!("https://{d}"))
                                .unwrap_or_else(|| deployment.cloudflare_pages_url.clone());
                            self.notify_user(
                                request.order_id,
                                &format!(
                                    "🎉 <b>Your website is live!</b>\n\n🌐 {site_url}\n\nCheck /status for details."
                                ),
                            )
                            .await;
                        }
                        Err(error) => {
                            error!(order_id = %request.order_id, error = %error, "deployment failed");
                            let deployment_status = if error.stage() == "domain_bind" {
                                "domain_bind_failed"
                            } else {
                                "deployment_failed"
                            };
                            sqlx::query(
                                r#"
                                UPDATE websites
                                SET deployment_status = ?, updated_at = datetime('now')
                                WHERE order_id = ?
                                "#,
                            )
                            .bind(deployment_status)
                            .bind(request.order_id)
                            .execute(&self.pool)
                            .await?;
                            let went_to_review = self
                                .retry_or_manual_review(job_id, request.order_id, attempt_count)
                                .await?;
                            if went_to_review {
                                self.notify_user(
                                    request.order_id,
                                    "⚠️ <b>Website deployment failed after retries.</b>\n\nYour order needs manual review. Contact admin for assistance.",
                                )
                                .await;
                            }
                        }
                    }
                } else {
                    error!(
                        order_id = %request.order_id,
                        "API_SERVICE_BASE_URL/API_SERVICE_API_KEY are required; meme-bot cannot deploy directly"
                    );
                    sqlx::query(
                        "UPDATE orders SET site_status = 'needs_manual_review', updated_at = datetime('now') WHERE id = ?",
                    )
                    .bind(request.order_id)
                    .execute(&self.pool)
                    .await?;
                    sqlx::query(
                        r#"
                        UPDATE websites
                        SET deployment_status = 'deployment_failed', updated_at = datetime('now')
                        WHERE order_id = ?
                        "#,
                    )
                    .bind(request.order_id)
                    .execute(&self.pool)
                    .await?;
                    db::record_deployment_attempt(
                        &self.pool,
                        website_id,
                        "pages_deploy",
                        "failed",
                        Some("API_SERVICE_BASE_URL and API_SERVICE_API_KEY are required; meme-bot cannot deploy directly"),
                    )
                    .await?;
                    self.mark_manual_review(job_id, request.order_id).await?;
                    self.notify_user(
                        request.order_id,
                        "⚠️ <b>Website deployment is not configured.</b>\n\nYour order needs manual review. Contact admin for assistance.",
                    )
                    .await;
                }
            }
            Err(GenerationError::QuotaExhausted | GenerationError::ProviderUnavailable) => {
                let went_to_review = self
                    .retry_or_manual_review(job_id, request.order_id, attempt_count)
                    .await?;
                if went_to_review {
                    self.notify_user(
                        request.order_id,
                        "⚠️ <b>Website generation could not complete.</b>\n\nAll providers are unavailable. Your order needs manual review. Contact admin.",
                    )
                    .await;
                }
            }
            Err(error) => {
                error!(order_id = %request.order_id, error = %error, "generation failed");
                let went_to_review = self
                    .retry_or_manual_review(job_id, request.order_id, attempt_count)
                    .await?;
                if went_to_review {
                    self.notify_user(
                        request.order_id,
                        "⚠️ <b>Website generation failed after retries.</b>\n\nYour order needs manual review. Contact admin for assistance.",
                    )
                    .await;
                }
            }
        }
        Ok(())
    }

    async fn lock_next_job(&self) -> anyhow::Result<Option<GenerationJobRow>> {
        let _guard = self.lock_mutex.lock().await;
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            r#"
            SELECT gj.id AS job_id, gj.order_id, gj.website_id, gj.attempt_count,
                   o.build_mode, o.template_id,
                   w.token_ticker, w.token_name, w.contract_address,
                   w.x_url, w.telegram_url, w.logo_asset_path, w.requested_domain
            FROM generation_jobs gj
            JOIN orders o ON o.id = gj.order_id
            JOIN websites w ON w.id = gj.website_id
            WHERE gj.status = 'queued'
              AND o.payment_status = 'paid'
            ORDER BY gj.priority DESC, gj.queued_at ASC
            LIMIT 1
            "#,
        )
        .fetch_optional(&mut *tx)
        .await?;

        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };

        let job_id: Uuid = Uuid::parse_str(row.get::<&str, _>("job_id"))?;
        let update = sqlx::query(
            r#"
            UPDATE generation_jobs
            SET status = 'running', started_at = COALESCE(started_at, datetime('now')),
                locked_by = ?, locked_at = datetime('now'), attempt_count = attempt_count + 1
            WHERE id = ?
              AND status = 'queued'
            "#,
        )
        .bind(&self.worker_id)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
        if update.rows_affected() != 1 {
            tx.commit().await?;
            return Ok(None);
        }
        tx.commit().await?;

        Ok(Some(GenerationJobRow {
            job_id,
            order_id: Uuid::parse_str(row.get::<&str, _>("order_id"))?,
            website_id: Uuid::parse_str(row.get::<&str, _>("website_id"))?,
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
            attempt_count: row.get::<i64, _>("attempt_count") + 1,
        }))
    }

    async fn reclaim_stale_running_jobs(&self) -> anyhow::Result<()> {
        let stale_cutoff = format!("-{STALE_RUNNING_JOB_MINUTES} minutes");
        let retry_rows = sqlx::query(
            r#"
            UPDATE generation_jobs
            SET status = 'queued',
                finished_at = NULL,
                locked_by = NULL,
                locked_at = NULL
            WHERE status = 'running'
              AND locked_at IS NOT NULL
              AND locked_at < datetime('now', ?)
              AND attempt_count < ?
            RETURNING order_id
            "#,
        )
        .bind(&stale_cutoff)
        .bind(MAX_JOB_ATTEMPTS)
        .fetch_all(&self.pool)
        .await?;

        for row in retry_rows {
            let order_id: Uuid = Uuid::parse_str(row.get::<&str, _>("order_id"))?;
            sqlx::query(
                "UPDATE orders SET site_status = 'queued', updated_at = datetime('now') WHERE id = ?",
            )
            .bind(order_id)
            .execute(&self.pool)
            .await?;
            warn!(%order_id, "requeued stale running generation job");
        }

        let review_rows = sqlx::query(
            r#"
            UPDATE generation_jobs
            SET status = 'needs_manual_review',
                finished_at = datetime('now'),
                locked_by = NULL,
                locked_at = NULL
            WHERE status = 'running'
              AND locked_at IS NOT NULL
              AND locked_at < datetime('now', ?)
              AND attempt_count >= ?
            RETURNING order_id
            "#,
        )
        .bind(&stale_cutoff)
        .bind(MAX_JOB_ATTEMPTS)
        .fetch_all(&self.pool)
        .await?;

        for row in review_rows {
            let order_id: Uuid = Uuid::parse_str(row.get::<&str, _>("order_id"))?;
            sqlx::query(
                "UPDATE orders SET site_status = 'needs_manual_review', updated_at = datetime('now') WHERE id = ?",
            )
            .bind(order_id)
            .execute(&self.pool)
            .await?;
            warn!(%order_id, "moved stale running generation job to manual review");
            self.notify_user(
                order_id,
                "⚠️ <b>Website processing stalled after retries.</b>\n\nYour order needs manual review. Contact admin for assistance.",
            )
            .await;
        }

        Ok(())
    }

    async fn deploy_with_retries(
        &self,
        deployer: &dyn SiteDeployer,
        request: &DeploymentRequest,
        website_id: Uuid,
    ) -> Result<crate::deployment::DeploymentResult, crate::deployment::DeploymentError> {
        let mut last_error = None;
        for attempt in 1..=MAX_JOB_ATTEMPTS {
            match deployer.deploy(request).await {
                Ok(result) => return Ok(result),
                Err(error) => {
                    error!(
                        order_id = %request.custom_domain.as_deref().unwrap_or(&request.repo_name),
                        attempt,
                        error = %error,
                        "deployment failed"
                    );
                    if let Err(record_error) = db::record_deployment_attempt(
                        &self.pool,
                        website_id,
                        error.stage(),
                        "failed",
                        Some(&format!("attempt {attempt}: {error}")),
                    )
                    .await
                    {
                        error!(error = %record_error, "failed to record deployment retry");
                    }
                    if attempt >= MAX_JOB_ATTEMPTS {
                        return Err(error);
                    }
                    last_error = Some(error);
                    sleep(Duration::from_secs(attempt as u64 * 5)).await;
                }
            }
        }
        Err(last_error.unwrap_or_else(|| {
            crate::deployment::DeploymentError::CloudflarePages(
                "deployment failed after retries".to_string(),
            )
        }))
    }

    async fn mark_manual_review(&self, job_id: Uuid, order_id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE orders SET site_status = 'needs_manual_review', updated_at = datetime('now') WHERE id = ?",
        )
        .bind(order_id)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            r#"
            UPDATE generation_jobs
            SET status = 'needs_manual_review', finished_at = datetime('now'), locked_by = NULL, locked_at = NULL
            WHERE id = ?
            "#,
        )
        .bind(job_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn retry_or_manual_review(
        &self,
        job_id: Uuid,
        order_id: Uuid,
        attempt_count: i64,
    ) -> anyhow::Result<bool> {
        if attempt_count < MAX_JOB_ATTEMPTS {
            sqlx::query(
                "UPDATE orders SET site_status = 'queued', updated_at = datetime('now') WHERE id = ?",
            )
            .bind(order_id)
            .execute(&self.pool)
            .await?;
            sqlx::query(
                r#"
                UPDATE generation_jobs
                SET status = 'queued', finished_at = NULL, locked_by = NULL, locked_at = NULL
                WHERE id = ?
                "#,
            )
            .bind(job_id)
            .execute(&self.pool)
            .await?;
            return Ok(false);
        }

        self.mark_manual_review(job_id, order_id).await?;
        Ok(true)
    }

    async fn notify_user(&self, order_id: Uuid, message: &str) {
        let Some(tx) = &self.notification_tx else {
            return;
        };
        match sqlx::query(
            "SELECT u.telegram_user_id FROM users u JOIN orders o ON o.user_id = u.id WHERE o.id = ?",
        )
        .bind(order_id)
        .fetch_optional(&self.pool)
        .await
        {
            Ok(Some(row)) => {
                let telegram_user_id: i64 = row.get("telegram_user_id");
                let notification = UserNotification {
                    telegram_user_id,
                    message: message.to_string(),
                };
                if tx.send(notification).is_err() {
                    warn!(order_id = %order_id, "notification channel closed");
                }
            }
            Ok(None) => {
                warn!(order_id = %order_id, "cannot notify user: order not found");
            }
            Err(error) => {
                warn!(order_id = %order_id, ?error, "cannot notify user: db lookup failed");
            }
        }
    }
}

impl SiteGenerationRequest {
    fn deployment_request(&self, repo_owner: &str, github_token: &str) -> DeploymentRequest {
        let domain = self.input.requested_domain.clone();
        let repo_name = domain
            .clone()
            .unwrap_or_else(|| format!("site-{}", &self.order_id.simple().to_string()[..8]));
        let project_name = project_name_from_repo_name(&repo_name);

        DeploymentRequest {
            repo_owner: repo_owner.to_string(),
            repo_name,
            github_token: github_token.to_string(),
            site_dir: self.output_dir.display().to_string(),
            cloudflare_project_name: project_name,
            production_branch: "main".to_string(),
            custom_domain: domain,
        }
    }
}

fn project_name_from_repo_name(repo_name: &str) -> String {
    repo_name
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
        .to_string()
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
    attempt_count: i64,
}

impl GenerationJobRow {
    fn into_generation_request(self) -> SiteGenerationRequest {
        let dir_name = self
            .requested_domain
            .clone()
            .unwrap_or_else(|| format!("site-{}", &self.order_id.simple().to_string()[..8]));
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
            output_dir: format!("/www/websites/{dir_name}").into(),
        }
    }
}
