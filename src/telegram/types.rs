use std::sync::Arc;

use rust_decimal::Decimal;
use teloxide::types::{ChatId, MessageId};

use crate::config::Config;

pub(crate) const EXTERNAL_STEP_MAX_ATTEMPTS: usize = 3;

#[derive(Clone, Debug)]
pub struct TemplateOption {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) preview_url: String,
}

impl TemplateOption {
    pub(crate) fn new(
        template_number: usize,
        preview_domain: &str,
        preview_base_url: &str,
    ) -> Self {
        let number = format_template_number(template_number);
        Self {
            id: format!("template_{number}"),
            name: format!("{number}.{preview_domain}"),
            preview_url: format!("{}/{number}", preview_base_url.trim_end_matches('/')),
        }
    }

    pub(crate) fn find(options: &[TemplateOption], template_id: &str) -> Option<TemplateOption> {
        options
            .iter()
            .find(|template| template.id == template_id)
            .cloned()
    }

    pub(crate) fn price(&self, prices: &[Decimal]) -> Decimal {
        let index = self
            .id
            .strip_prefix("template_")
            .and_then(|n| n.parse::<usize>().ok())
            .map(|n| n.saturating_sub(1))
            .unwrap_or(0);
        prices.get(index).copied().unwrap_or(Decimal::ZERO)
    }

    pub(crate) fn price_by_id(
        options: &[TemplateOption],
        template_id: &str,
        prices: &[Decimal],
    ) -> Decimal {
        let index = options
            .iter()
            .position(|template| template.id == template_id)
            .unwrap_or(0);
        prices.get(index).copied().unwrap_or(Decimal::ZERO)
    }
}

pub(crate) fn format_template_number(template_number: usize) -> String {
    if template_number < 100 {
        format!("{template_number:02}")
    } else {
        template_number.to_string()
    }
}

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
    pub template_options: Vec<TemplateOption>,
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
        let template_options = build_template_options(
            &config.template_preview_domain,
            &config.template_preview_base_url,
            config.template_prices_usdt.len(),
        );
        let required_group = parse_required_group(&config.telegram_required_group_id);
        Self {
            config,
            template_options,
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
}

fn build_template_options(
    preview_domain: &str,
    preview_base_url: &str,
    template_count: usize,
) -> Vec<TemplateOption> {
    (1..=template_count)
        .map(|n| TemplateOption::new(n, preview_domain, preview_base_url))
        .collect()
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
