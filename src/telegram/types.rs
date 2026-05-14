use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use rust_decimal::Decimal;
use teloxide::types::{ChatId, MessageId, UserId};
use uuid::Uuid;

use crate::{config::GenerationProviderName, models::PaymentChain};

pub(crate) type Sessions = Arc<Mutex<HashMap<UserId, OrderDraft>>>;

pub(crate) const EXTERNAL_STEP_MAX_ATTEMPTS: usize = 3;
pub(crate) const DRAFT_SESSION_TIMEOUT_MINUTES: i64 = crate::db::ASSET_COLLECTION_TIMEOUT_MINUTES;

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
        Self {
            id: format!("template_{template_number:02}"),
            name: format!("{template_number:02}.{preview_domain}"),
            preview_url: format!("{preview_base_url}/{template_number:02}"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct UiMessage {
    pub chat_id: ChatId,
    pub message_id: MessageId,
}

#[derive(Clone)]
pub struct TelegramUiConfig {
    pub required_group: Option<RequiredGroup>,
    pub generation_primary_provider: GenerationProviderName,
    pub agent_timeout_seconds: u64,
    pub gemini_agent_command: String,
    pub gemini_agent_args: String,
    pub codex_agent_command: String,
    pub codex_agent_args: String,
    pub generated_sites_dir: String,
    pub api_service_base_url: Option<String>,
    pub api_service_api_key: Option<String>,
    pub github_owner: String,
    pub template_catalog_url: String,
    pub template_preview_domain: String,
    pub template_options: Vec<TemplateOption>,
    pub admin_contact: String,
    pub template_prices_usdt: Vec<Decimal>,
    pub ai_creation_price_usdt: Decimal,
    pub usdt_erc20_address: String,
    pub usdt_tron_address: String,
    pub usdt_sol_address: String,
    pub usdt_erc20_rpc_url: String,
    pub usdt_erc20_contract: String,
    pub usdt_erc20_min_confirmations: u64,
    pub tron_grid_base_url: String,
    pub tron_grid_api_key: Option<String>,
    pub usdt_tron_contract: String,
    pub usdt_tron_min_confirmations: u64,
    pub solana_rpc_url: String,
    pub usdt_sol_mint: String,
    pub usdt_sol_min_confirmations: u64,
    pub auto_purchase_after_tx: bool,
    pub skip_payment_verification: bool,
    pub skip_domain_purchase: bool,
}

#[derive(Clone)]
pub enum RequiredGroup {
    Id(i64),
    Username(String),
}

impl TelegramUiConfig {
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self {
            required_group: parse_required_group(&config.telegram_required_group_id),
            generation_primary_provider: config.generation_primary_provider,
            agent_timeout_seconds: config.agent_timeout_seconds,
            gemini_agent_command: config.gemini_agent_command.clone(),
            gemini_agent_args: config.gemini_agent_args.clone(),
            codex_agent_command: config.codex_agent_command.clone(),
            codex_agent_args: config.codex_agent_args.clone(),
            generated_sites_dir: config.generated_sites_dir.clone(),
            api_service_base_url: config.api_service_base_url.clone(),
            api_service_api_key: config.api_service_api_key.clone(),
            github_owner: config.github_owner.clone(),
            template_catalog_url: config.template_catalog_url.clone(),
            template_preview_domain: config.template_preview_domain.clone(),
            template_options: build_template_options(
                &config.template_preview_domain,
                &config.template_preview_base_url,
            ),
            admin_contact: config.admin_contact.clone(),
            template_prices_usdt: config.template_prices_usdt.clone(),
            ai_creation_price_usdt: config.ai_creation_price_usdt,
            usdt_erc20_address: config.usdt_erc20_address.clone(),
            usdt_tron_address: config.usdt_tron_address.clone(),
            usdt_sol_address: config.usdt_sol_address.clone(),
            usdt_erc20_rpc_url: config.usdt_erc20_rpc_url.clone(),
            usdt_erc20_contract: config.usdt_erc20_contract.clone(),
            usdt_erc20_min_confirmations: config.usdt_erc20_min_confirmations,
            tron_grid_base_url: config.tron_grid_base_url.clone(),
            tron_grid_api_key: config.tron_grid_api_key.clone(),
            usdt_tron_contract: config.usdt_tron_contract.clone(),
            usdt_tron_min_confirmations: config.usdt_tron_min_confirmations,
            solana_rpc_url: config.solana_rpc_url.clone(),
            usdt_sol_mint: config.usdt_sol_mint.clone(),
            usdt_sol_min_confirmations: config.usdt_sol_min_confirmations,
            auto_purchase_after_tx: config.auto_purchase_after_tx,
            skip_payment_verification: config.skip_payment_verification,
            skip_domain_purchase: config.skip_domain_purchase,
        }
    }
}

fn build_template_options(preview_domain: &str, preview_base_url: &str) -> Vec<TemplateOption> {
    (1..=12)
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

#[derive(Clone, Debug)]
pub(crate) struct OrderDraft {
    pub build_mode: BuildModeDraft,
    pub step: DraftStep,
    pub site_price_usdt: Decimal,
    pub template_id: Option<String>,
    pub logo_file_id: Option<String>,
    pub ticker: Option<String>,
    pub token_name: Option<String>,
    pub contract_address: Option<String>,
    pub x_link: Option<String>,
    pub telegram_link: Option<String>,
    pub website_url: Option<String>,
    pub domain_price_usdt: Option<Decimal>,
    pub alternative_domains: Option<Vec<crate::domain::DomainQuote>>,
    pub total_usdt: Option<Decimal>,
    pub payment_chain: Option<PaymentChain>,
    pub order_id: Option<Uuid>,
    pub db_user_id: Option<Uuid>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Debug)]
pub(crate) enum BuildModeDraft {
    Template,
    AiFree,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DraftStep {
    WaitingLogo,
    WaitingTicker,
    WaitingTokenName,
    WaitingContractAddress,
    WaitingXLink,
    WaitingTelegramLink,
    WaitingWebsiteUrl,
    ReadyToPay,
    WaitingTx,
    PaymentConfirmed,
}

impl DraftStep {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WaitingLogo => "waiting_logo",
            Self::WaitingTicker => "waiting_ticker",
            Self::WaitingTokenName => "waiting_token_name",
            Self::WaitingContractAddress => "waiting_contract_address",
            Self::WaitingXLink => "waiting_x_link",
            Self::WaitingTelegramLink => "waiting_telegram_link",
            Self::WaitingWebsiteUrl => "waiting_website_url",
            Self::ReadyToPay => "ready_to_pay",
            Self::WaitingTx => "waiting_tx",
            Self::PaymentConfirmed => "payment_confirmed",
        }
    }
}

impl OrderDraft {
    pub fn new_ai(price_usdt: Decimal) -> Self {
        Self::new(BuildModeDraft::AiFree, price_usdt, None)
    }

    pub fn new_template(template: TemplateOption, price_usdt: Decimal) -> Self {
        Self::new(
            BuildModeDraft::Template,
            price_usdt,
            Some(template.id.to_string()),
        )
    }

    fn new(
        build_mode: BuildModeDraft,
        site_price_usdt: Decimal,
        template_id: Option<String>,
    ) -> Self {
        Self {
            build_mode,
            step: DraftStep::WaitingLogo,
            site_price_usdt,
            template_id,
            logo_file_id: None,
            ticker: None,
            token_name: None,
            contract_address: None,
            x_link: None,
            telegram_link: None,
            website_url: None,
            domain_price_usdt: None,
            alternative_domains: None,
            total_usdt: None,
            payment_chain: None,
            order_id: None,
            db_user_id: None,
            updated_at: chrono::Utc::now(),
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = chrono::Utc::now();
    }

    pub fn is_expired(&self) -> bool {
        chrono::Utc::now() - self.updated_at
            > chrono::Duration::minutes(DRAFT_SESSION_TIMEOUT_MINUTES)
    }
}
