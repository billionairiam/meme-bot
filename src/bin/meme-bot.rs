use std::time::Duration;

use anyhow::Context;
use meme_bot::app::App;
use meme_bot::config::Config;
use meme_bot::telegram::run_bot;
use sqlx::sqlite::SqlitePoolOptions;
use tracing::{error, info};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = Config::from_env()?;

    let database_url = config
        .database_url
        .as_deref()
        .context("DATABASE_URL is required")?;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(database_url)
        .await
        .context("connect sqlite")?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("run migrations")?;

    let app = App::new(config.clone(), pool.clone());
    let queue = app.build_queue();

    let telegram_handle = tokio::spawn(async move {
        if let Err(e) = run_bot(config, pool).await {
            error!(?e, "telegram bot exited");
        }
    });

    let worker_handle = tokio::spawn(async move {
        info!("queue worker loop started");
        loop {
            match queue.tick().await {
                Ok(()) => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                Err(e) => {
                    error!(?e, "queue tick error");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }
        }
    });

    tokio::select! {
        r = telegram_handle => {
            error!("telegram bot task ended unexpectedly: {:?}", r);
        }
        r = worker_handle => {
            error!("queue worker task ended unexpectedly: {:?}", r);
        }
    }

    Ok(())
}
