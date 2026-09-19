use std::sync::Arc;

use teloxide::types::{ChatId, MessageId};

use crate::config::Config;

pub(crate) const EXTERNAL_STEP_MAX_ATTEMPTS: usize = 3;
pub(crate) const TEMPLATE_PRICE_USDT: u16 = 200;
pub(crate) const TEMPLATE_SITE_URL: &str = "https://memewebs.store";

#[derive(Clone, Copy, Debug)]
pub(crate) struct UiMessage {
    pub chat_id: ChatId,
    pub message_id: MessageId,
}

/// Telegram-specific config that wraps `Arc<Config>` to avoid duplicating fields.
/// Implements `Deref<Target = Config>` so all `Config` fields are accessible directly.
#[derive(Clone)]
pub struct TelegramUiConfig {
    config: Arc<Config>,
    pub required_group: Option<RequiredGroup>,
}

#[derive(Clone)]
pub enum RequiredGroup {
    Id(i64),
    Username(String),
}

impl std::ops::Deref for TelegramUiConfig {
    type Target = Config;
    fn deref(&self) -> &Config {
        &self.config
    }
}

impl TelegramUiConfig {
    pub fn new(config: Arc<Config>) -> Self {
        let required_group = parse_required_group(&config.telegram_required_group_id);
        Self {
            config,
            required_group,
        }
    }

    pub(crate) fn payment_address_for_chain(&self, chain: crate::models::PaymentChain) -> &str {
        match chain {
            crate::models::PaymentChain::Erc20 => &self.usdt_erc20_address,
            crate::models::PaymentChain::Tron => &self.usdt_tron_address,
            crate::models::PaymentChain::Sol => &self.usdt_sol_address,
        }
    }

    pub(crate) fn shared_config(&self) -> Arc<Config> {
        Arc::clone(&self.config)
    }
}

fn parse_required_group(value: &str) -> Option<RequiredGroup> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else if let Ok(id) = value.parse::<i64>() {
        Some(RequiredGroup::Id(id))
    } else {
        let username = value
            .trim_start_matches("https://t.me/")
            .trim_start_matches("t.me/")
            .trim_start_matches('@');
        Some(RequiredGroup::Username(format!("@{username}")))
    }
}
