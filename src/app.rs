use std::sync::Arc;

use sqlx::PgPool;
use tracing::info;

use crate::{
    config::Config,
    deployment::CloudflarePagesDeployer,
    generation::{CliGenerationProvider, GenerationRouter},
    queue::GenerationQueue,
};

pub struct App {
    config: Config,
    pool: PgPool,
}

impl App {
    pub fn new(config: Config, pool: PgPool) -> Self {
        Self { config, pool }
    }

    pub async fn run_once(&self) -> anyhow::Result<()> {
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
        let queue = GenerationQueue::new(
            self.pool.clone(),
            router,
            Some(Arc::new(CloudflarePagesDeployer::new(
                self.config.cloudflare_account_id.clone(),
                self.config.cloudflare_api_token.clone(),
                self.config.cloudflare_wrangler_command.clone(),
                self.config.cloudflare_wrangler_args.clone(),
            ))),
            self.config.generation_max_concurrency,
            "worker-1".to_string(),
        );

        info!(
            max_concurrency = self.config.generation_max_concurrency,
            primary_provider = self.config.generation_primary_provider.as_str(),
            "generation queue configured"
        );

        queue.tick().await?;
        Ok(())
    }
}
