#![allow(dead_code)]

mod app;
mod config;
mod db;
mod deployment;
mod domain;
mod generation;
mod models;
mod payment;
mod queue;
mod telegram;
mod validation;

use anyhow::Context;
use config::Config;
use sqlx::postgres::PgPoolOptions;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Config::from_env()?;
    if config.telegram_frontend_only || config.database_url.is_none() {
        info!("starting Telegram frontend only mode");
        telegram::run_frontend(config).await?;
        return Ok(());
    }

    let database_url = config
        .database_url
        .as_deref()
        .context("DATABASE_URL is required unless TELEGRAM_FRONTEND_ONLY=true")?;
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await
        .context("connect postgres")?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("run migrations")?;

    let app = app::App::new(config, pool);
    info!("meme bot backend initialized");
    app.run_once().await?;
    Ok(())
}
