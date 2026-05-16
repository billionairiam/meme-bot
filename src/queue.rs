use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::{Mutex, Semaphore, mpsc};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    db,
    deployment::{DeploymentRequest, SiteDeployer},
    generation::{GenerationError, GenerationRouter, SiteGenerationRequest},
    models::{Order, OrderStatus, WebsiteInput},
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
    pub generated_sites_dir: String,
}

pub struct GenerationQueue {
    pool: SqlitePool,
    router: Arc<GenerationRouter>,
    deployer: Option<Arc<dyn SiteDeployer>>,
    repo_owner: String,
    github_token: String,
    generated_sites_dir: String,
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
            generated_sites_dir: config.generated_sites_dir,
            semaphore: Arc::new(Semaphore::new(config.max_concurrency.max(1))),
            lock_mutex: Arc::new(Mutex::new(())),
            worker_id: config.worker_id,
            notification_tx,
        }
    }

    pub async fn tick(&self) -> anyhow::Result<()> {
        let _permit = self.semaphore.acquire().await?;
        self.reclaim_stale_locked_orders().await?;

        let order = {
            let _guard = self.lock_mutex.lock().await;
            db::lock_next_generation_order(&self.pool, &self.worker_id, MAX_JOB_ATTEMPTS).await?
        };

        let Some(order) = order else {
            return Ok(());
        };

        let order_id = order.id;
        let attempt_count = order.attempt_count;

        info!(
            order_id = %order_id,
            attempt = attempt_count,
            domain = order.requested_domain.as_deref().unwrap_or("<none>"),
            ticker = order.token_ticker.as_deref().unwrap_or("<none>"),
            "locked order for generation"
        );

        let Some(input) = order.website_input() else {
            error!(order_id = %order_id, "order is missing required wizard fields");
            self.mark_manual_review(order_id).await?;
            self.notify_user(
                order_id,
                "⚠️ <b>Order is incomplete.</b>\n\nYour order needs manual review. Contact admin.",
            )
            .await;
            return Ok(());
        };

        let request = build_generation_request(&order, &input, &self.generated_sites_dir);

        info!(
            order_id = %order_id,
            output_dir = %request.output_dir.display(),
            build_mode = %request.build_mode,
            domain = input.requested_domain.as_deref().unwrap_or("<none>"),
            "starting site generation"
        );

        db::update_order_status(&self.pool, order_id, OrderStatus::PaidAndGenerating).await?;

        match self.router.generate(&request).await {
            Ok(result) => {
                info!(
                    order_id = %order_id,
                    provider = %result.provider_name,
                    "site generation succeeded"
                );

                // Create website record
                if let Err(e) = db::create_website(
                    &self.pool,
                    order.user_id,
                    order_id,
                    &input,
                    Some(&result.provider_name),
                )
                .await
                {
                    error!(order_id = %order_id, error = %e, "failed to create website record");
                }

                if let Some(deployer) = &self.deployer {
                    let deployment_request =
                        request.deployment_request(&self.repo_owner, &self.github_token);
                    info!(
                        order_id = %order_id,
                        repo_name = %deployment_request.repo_name,
                        project_name = %deployment_request.cloudflare_project_name,
                        custom_domain = deployment_request.custom_domain.as_deref().unwrap_or("<none>"),
                        "starting site deployment"
                    );
                    match deployer.deploy(&deployment_request).await {
                        Ok(deployment) => {
                            info!(
                                order_id = %order_id,
                                repo = %format!("{}/{}", deployment.github_owner, deployment.github_repo_name),
                                pages_url = %deployment.cloudflare_pages_url,
                                custom_domain = deployment.custom_domain.as_deref().unwrap_or("<none>"),
                                "site deployment succeeded"
                            );
                            db::update_website_deployment_success(
                                &self.pool,
                                order_id,
                                &deployment,
                            )
                            .await?;

                            let site_url = deployment
                                .custom_domain
                                .as_deref()
                                .map(|d| format!("https://{d}"))
                                .unwrap_or_else(|| deployment.cloudflare_pages_url.clone());
                            info!(
                                order_id = %order_id,
                                site_url = %site_url,
                                "notifying user of successful deployment"
                            );
                            self.notify_user(
                                order_id,
                                &format!(
                                    "🎉 <b>Your website is live!</b>\n\n🌐 {site_url}\n\nCheck /history for details."
                                ),
                            ).await;
                        }
                        Err(error) => {
                            error!(
                                order_id = %order_id,
                                stage = error.stage(),
                                error = %error,
                                "deployment failed after retries"
                            );
                            self.mark_manual_review(order_id).await?;
                            self.notify_user(
                                order_id,
                                &format!(
                                    "⚠️ <b>Website generated successfully but deployment failed.</b>\n\nStage: {}\nError: {}\n\nYour order needs manual review. Contact admin.",
                                    error.stage(),
                                    error
                                ),
                            ).await;
                        }
                    }
                } else {
                    error!(
                        order_id = %order_id,
                        "API_SERVICE_BASE_URL/API_SERVICE_API_KEY are required; meme-bot cannot deploy directly"
                    );
                    self.mark_manual_review(order_id).await?;
                    self.notify_user(
                        order_id,
                        "⚠️ <b>Website deployment is not configured.</b>\n\nYour order needs manual review. Contact admin.",
                    ).await;
                }
            }
            Err(GenerationError::QuotaExhausted | GenerationError::ProviderUnavailable) => {
                warn!(
                    order_id = %order_id,
                    attempt = attempt_count,
                    "generation provider unavailable or quota exhausted"
                );
                let went_to_review = self.retry_or_manual_review(order_id, attempt_count).await?;
                if went_to_review {
                    self.notify_user(
                        order_id,
                        "⚠️ <b>Website generation could not complete.</b>\n\nAll providers are unavailable. Contact admin.",
                    ).await;
                }
            }
            Err(error) => {
                error!(
                    order_id = %order_id,
                    attempt = attempt_count,
                    error = %error,
                    "generation failed"
                );
                let went_to_review = self.retry_or_manual_review(order_id, attempt_count).await?;
                if went_to_review {
                    self.notify_user(
                        order_id,
                        "⚠️ <b>Website generation failed after retries.</b>\n\nYour order needs manual review. Contact admin.",
                    ).await;
                }
            }
        }
        Ok(())
    }

    async fn reclaim_stale_locked_orders(&self) -> anyhow::Result<()> {
        let results = db::reclaim_stale_locked_orders(
            &self.pool,
            STALE_RUNNING_JOB_MINUTES,
            MAX_JOB_ATTEMPTS,
        )
        .await?;

        for (order_id, went_to_review) in results {
            if went_to_review {
                warn!(%order_id, "moved stale order to manual review");
                self.notify_user(
                    order_id,
                    "⚠️ <b>Website processing stalled after retries.</b>\n\nYour order needs manual review. Contact admin.",
                ).await;
            } else {
                warn!(%order_id, "requeued stale locked order");
            }
        }
        Ok(())
    }

    async fn mark_manual_review(&self, order_id: Uuid) -> anyhow::Result<()> {
        db::mark_order_manual_review(&self.pool, order_id).await
    }

    async fn retry_or_manual_review(
        &self,
        order_id: Uuid,
        attempt_count: i64,
    ) -> anyhow::Result<bool> {
        if attempt_count < MAX_JOB_ATTEMPTS {
            warn!(
                order_id = %order_id,
                attempt = attempt_count,
                max_attempts = MAX_JOB_ATTEMPTS,
                "requeueing order for retry"
            );
            db::requeue_order_for_retry(&self.pool, order_id).await?;
            return Ok(false);
        }
        self.mark_manual_review(order_id).await?;
        warn!(
            order_id = %order_id,
            attempt = attempt_count,
            "order moved to manual review after retries"
        );
        Ok(true)
    }

    async fn notify_user(&self, order_id: Uuid, message: &str) {
        let Some(tx) = &self.notification_tx else {
            return;
        };
        match sqlx::query(
            "SELECT u.telegram_user_id FROM users u JOIN orders o ON o.user_id = u.id WHERE o.id = ?",
        )
        .bind(order_id.to_string())
        .fetch_optional(&self.pool)
        .await
        {
            Ok(Some(row)) => {
                use sqlx::Row;
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

fn build_generation_request(
    order: &Order,
    input: &WebsiteInput,
    generated_sites_dir: &str,
) -> SiteGenerationRequest {
    let dir_name = order
        .requested_domain
        .clone()
        .unwrap_or_else(|| format!("site-{}", &order.id.simple().to_string()[..8]));
    let output_dir = std::path::Path::new(generated_sites_dir).join(dir_name);
    SiteGenerationRequest {
        order_id: order.id,
        build_mode: order
            .build_mode
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| "ai_free".to_string()),
        template_local_path: order.template_id.clone(),
        input: input.clone(),
        output_dir,
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

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::models::{BuildMode, OrderStatus, TokenChain};

    use super::build_generation_request;

    #[test]
    fn generation_request_uses_configured_sites_dir_and_domain_folder() {
        let order = crate::models::Order {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            status: OrderStatus::PaidAndGenerating,
            build_mode: Some(BuildMode::AiFree),
            template_id: None,
            token_chain: Some(TokenChain::Eth),
            token_ticker: Some("AAA".to_string()),
            token_name: Some("AAA BBB".to_string()),
            contract_address: Some("0x123".to_string()),
            x_url: Some("https://x.com/aaabbb".to_string()),
            telegram_url: Some("https://t.me/aaabbb".to_string()),
            logo_asset_path: Some("/tmp/logo.png".to_string()),
            requested_domain: Some("aaabbb.com".to_string()),
            site_price_usdt: rust_decimal::Decimal::new(50, 0),
            domain_price_usdt: rust_decimal::Decimal::ZERO,
            total_usdt: rust_decimal::Decimal::new(50, 0),
            payment_chain: None,
            tx_hash: None,
            payment_expires_at: None,
            paid_at: None,
            pending_contract_address: None,
            pending_x_url: None,
            pending_telegram_url: None,
            pending_edit_field: None,
            edit_status: "none".to_string(),
            edit_payment_chain: None,
            edit_tx_hash: None,
            edit_payment_expires_at: None,
            edit_paid_at: None,
            edit_success_count: 0,
            attempt_count: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            alternative_domains_json: None,
        };
        let input = order.website_input().unwrap();

        let request = build_generation_request(&order, &input, "/www/websites");

        assert_eq!(
            request.output_dir,
            std::path::PathBuf::from("/www/websites/aaabbb.com")
        );
    }
}
