use std::sync::Arc;

use sqlx::SqlitePool as Pool;
use tokio::sync::mpsc;
use tracing::info;

use crate::{
    api_client::HttpClientPagesDeployer,
    config::Config,
    deployment::SiteDeployer,
    generation::{CliGenerationProvider, GenerationRouter},
    queue::{GenerationQueue, GenerationQueueConfig, UserNotification},
};

pub struct App {
    config: Config,
    pool: Pool,
}

impl App {
    pub fn new(config: Config, pool: Pool) -> Self {
        Self { config, pool }
    }

    pub fn build_queue(
        &self,
        notification_tx: Option<mpsc::UnboundedSender<UserNotification>>,
    ) -> GenerationQueue {
        let router = Arc::new(GenerationRouter::new(
            self.config.generation_primary_provider,
            Box::new(CliGenerationProvider::gemini(
                self.config.gemini_agent_command.clone(),
                self.config.gemini_agent_args.clone(),
                self.config.agent_timeout_seconds,
            )),
            Box::new(CliGenerationProvider::codex(
                self.config.codex_agent_command.clone(),
                self.config.codex_agent_args.clone(),
                self.config.agent_timeout_seconds,
            )),
            Box::new(CliGenerationProvider::claude(
                self.config.claude_agent_command.clone(),
                self.config.claude_agent_args.clone(),
                self.config.agent_timeout_seconds,
            )),
        ));

        let deployer: Option<Arc<dyn SiteDeployer>> = match (
            self.config.api_service_base_url.as_deref(),
            self.config.api_service_api_key.as_deref(),
        ) {
            (Some(base_url), Some(api_key)) => Some(Arc::new(HttpClientPagesDeployer::new(
                base_url.to_string(),
                api_key.to_string(),
            ))),
            _ => None,
        };

        info!(
            max_concurrency = self.config.generation_max_concurrency,
            primary_provider = self.config.generation_primary_provider.as_str(),
            "generation queue configured"
        );

        GenerationQueue::new(
            self.pool.clone(),
            router,
            deployer,
            GenerationQueueConfig {
                max_concurrency: self.config.generation_max_concurrency,
                worker_id: "worker-1".to_string(),
                repo_owner: self.config.github_owner.clone(),
                github_token: self.config.github_token.clone(),
                generated_sites_dir: self.config.generated_sites_dir.clone(),
            },
            notification_tx,
        )
    }

    pub async fn run_once(&self) -> anyhow::Result<()> {
        self.build_queue(None).tick().await
    }
}
