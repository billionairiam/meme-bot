use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use meme_bot::app::App;
use meme_bot::config::{Config, load_env_file};
use meme_bot::logging::init_logging;
use meme_bot::telegram::run_bot;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use teloxide::{prelude::*, types::ParseMode};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let env_file = load_env_file("MEME_BOT_ENV_FILE", ".env.meme-bot")?;
    let _log_guard = init_logging("meme-bot");
    if let Some(path) = env_file {
        info!(env_file = %path, "loaded meme-bot env file");
    } else {
        info!("meme-bot env file not found; using process environment only");
    }

    let config = Arc::new(Config::from_env()?);
    if config.skip_payment_verification {
        warn!(
            "⚠️  SKIP_PAYMENT_VERIFICATION is enabled — all TX hashes will be accepted without on-chain verification"
        );
    }
    if config.skip_domain_purchase {
        warn!(
            "⚠️  SKIP_DOMAIN_PURCHASE is enabled — domain purchase and DNS setup will be skipped"
        );
    }

    let database_url = config
        .database_url
        .as_deref()
        .context("DATABASE_URL is required")?;
    let connect_opts = SqliteConnectOptions::from_str(database_url)?
        .pragma("foreign_keys", "ON")
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_opts)
        .await
        .context("connect sqlite")?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("run migrations")?;
    let stores = meme_bot::db::DbStores::new(pool.clone());
    stores
        .normalize_sqlite_uuid_storage()
        .await
        .context("normalize sqlite uuid storage")?;
    match stores.orders.reset_generating_on_startup().await {
        Ok(0) => {}
        Ok(count) => info!(
            count,
            "reset stuck paid_and_generating orders (cleared locks)"
        ),
        Err(error) => warn!(?error, "failed to reset stuck generating orders"),
    }

    let (notification_tx, mut notification_rx) =
        mpsc::unbounded_channel::<meme_bot::queue::UserNotification>();

    let app = App::new(Arc::clone(&config), pool.clone());
    let queue = app.build_queue(Some(notification_tx));

    let notify_bot = if config.telegram_bot_token.trim().is_empty() {
        None
    } else {
        let bot = Bot::new(config.telegram_bot_token.clone());
        let _ = bot.get_me().await;
        Some(bot)
    };

    let bot_config = Arc::clone(&config);
    let telegram_handle = tokio::spawn(async move {
        if let Err(e) = run_bot(bot_config, pool).await {
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
