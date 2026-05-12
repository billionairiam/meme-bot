mod domain_flow;
mod handlers;
mod helpers;
mod keyboards;
mod payment;
mod screens;
mod types;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use sqlx::SqlitePool;
use teloxide::{
    dispatching::UpdateFilterExt,
    dptree,
    prelude::*,
    utils::client_from_env,
};
use tracing::info;

use crate::config::Config;

pub use types::{TelegramUiConfig, TemplateOption};
pub use screens::payment_instruction;

use handlers::{handle_callback, handle_message};
use helpers::configured_proxy;
use types::Sessions;

pub async fn run_bot(config: Config, pool: SqlitePool) -> anyhow::Result<()> {
    if config.telegram_bot_token.trim().is_empty() {
        anyhow::bail!("TELEGRAM_BOT_TOKEN is required to run the Telegram bot");
    }

    let proxy = configured_proxy();
    if let Some(proxy) = proxy.as_deref() {
        info!(proxy = %proxy, "using Telegram proxy from TELOXIDE_PROXY");
    } else {
        info!("TELOXIDE_PROXY is not set; connecting to Telegram directly");
    }

    let bot = if proxy.is_some() {
        Bot::with_client(config.telegram_bot_token.clone(), client_from_env())
    } else {
        Bot::new(config.telegram_bot_token.clone())
    };
    bot.get_me().await.map_err(|error| {
        anyhow::anyhow!(
            "cannot connect to Telegram Bot API. If you are behind a proxy, set TELOXIDE_PROXY, for example TELOXIDE_PROXY=http://127.0.0.1:7890. Original error: {error}"
        )
    })?;

    let ui_config = Arc::new(TelegramUiConfig::from_config(&config));
    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(handle_message))
        .branch(Update::filter_callback_query().endpoint(handle_callback));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![ui_config, sessions, pool])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}
