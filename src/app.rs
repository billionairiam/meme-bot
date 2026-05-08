use std::sync::Arc;

use sqlx::SqlitePool as Pool;
use tracing::info;

use crate::{
    config::Config,
    deployment::{CloudflarePagesDeployer, HttpClientPagesDeployer, SiteDeployer},
    generation::{CliGenerationProvider, GenerationRouter},
    queue::GenerationQueue,
};

pub struct App {
    config: Config,
    pool: Pool,
}

impl App {
    pub fn new(config: Config, pool: Pool) -> Self {
        Self { config, pool }
    }

    pub fn build_queue(&self) -> GenerationQueue {
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
        ));

        let deployer: Option<Arc<dyn SiteDeployer>> =
            if let (Some(base_url), Some(api_key)) = (
                self.config.api_service_base_url.as_deref(),
                self.config.api_service_api_key.as_deref(),
            ) {
                Some(Arc::new(HttpClientPagesDeployer::new(
                    base_url.to_string(),
                    api_key.to_string(),
                )))
            } else {
                Some(Arc::new(CloudflarePagesDeployer::new(
                    self.config.cloudflare_account_id.clone(),
                    self.config.cloudflare_api_token.clone(),
                    self.config.cloudflare_wrangler_command.clone(),
                    self.config.cloudflare_wrangler_args.clone(),
                )))
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
            self.config.generation_max_concurrency,
            "worker-1".to_string(),
        )
    }

    pub async fn run_once(&self) -> anyhow::Result<()> {
        self.build_queue().tick().await
    }
}
