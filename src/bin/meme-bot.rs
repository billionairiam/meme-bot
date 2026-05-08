use std::time::Duration;

use anyhow::Context;
use meme_bot::app::App;
use meme_bot::config::Config;
use meme_bot::telegram::run_bot;
use sqlx::sqlite::SqlitePoolOptions;
use teloxide::{prelude::*, types::ParseMode};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

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

    let (notification_tx, mut notification_rx) =
        mpsc::unbounded_channel::<meme_bot::queue::UserNotification>();

    let app = App::new(config.clone(), pool.clone());
    let queue = app.build_queue(Some(notification_tx));

    let notify_bot = if config.telegram_bot_token.trim().is_empty() {
        None
    } else {
        let bot = Bot::new(config.telegram_bot_token.clone());
        let _ = bot.get_me().await;
        Some(bot)
    };

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

    let notifier_handle = tokio::spawn(async move {
        while let Some(notification) = notification_rx.recv().await {
            let Some(bot) = &notify_bot else {
                warn!("notification channel received message but no bot is configured");
                continue;
            };
            let chat_id = teloxide::types::ChatId(notification.telegram_user_id);
            if let Err(error) = bot
                .send_message(chat_id, &notification.message)
                .parse_mode(ParseMode::Html)
                .await
            {
                warn!(
                    telegram_user_id = notification.telegram_user_id,
                    ?error,
                    "failed to send notification"
                );
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
        r = notifier_handle => {
            error!("notifier task ended unexpectedly: {:?}", r);
        }
    }

    Ok(())
}
