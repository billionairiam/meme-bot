use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use rust_decimal::Decimal;
use sqlx::SqlitePool;
use teloxide::{
    dispatching::UpdateFilterExt,
    dptree,
    prelude::*,
    types::{
        CallbackQuery, ChatId, InlineKeyboardButton, InlineKeyboardMarkup, Message, MessageId,
        ParseMode, Recipient, UserId,
    },
    utils::client_from_env,
};
use tokio::time::{Duration as TokioDuration, sleep};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    config::{Config, GenerationProviderName, NamecheapConfig},
    db,
    domain::{
        CloudflareDnsProvisioner, DnsProvisioner, DomainError, DomainPurchaseRequest,
        DomainQuoteRequest, DomainRegistrar, HttpClientDnsProvisioner, HttpClientDomainRegistrar,
        NamecheapDnsProvisioner, NamecheapRegistrar,
    },
    models::{PaymentChain, WebsiteInput},
    payment::{
        ChainPaymentVerifier, PaymentService, PaymentVerificationError, PaymentVerificationOutcome,
        PaymentVerificationRequest, PaymentVerifier,
    },
    validation::{
        validate_contract_address, validate_domain, validate_telegram_url, validate_x_url,
    },
};

type Sessions = Arc<Mutex<HashMap<UserId, OrderDraft>>>;

const EXTERNAL_STEP_MAX_ATTEMPTS: usize = 3;

const TEMPLATE_OPTIONS: [TemplateOption; 12] = [
    TemplateOption::new("template_01", "example1.web", 30),
    TemplateOption::new("template_02", "example2.web", 30),
    TemplateOption::new("template_03", "example3.web", 30),
    TemplateOption::new("template_04", "example4.web", 40),
    TemplateOption::new("template_05", "example5.web", 40),
    TemplateOption::new("template_06", "example6.web", 40),
    TemplateOption::new("template_07", "example7.web", 50),
    TemplateOption::new("template_08", "example8.web", 50),
    TemplateOption::new("template_09", "example9.web", 50),
    TemplateOption::new("template_10", "example10.web", 60),
    TemplateOption::new("template_11", "example11.web", 60),
    TemplateOption::new("template_12", "example12.web", 60),
];

#[derive(Clone, Copy, Debug)]
struct TemplateOption {
    id: &'static str,
    name: &'static str,
    price_usdt: i64,
}

#[derive(Clone, Copy, Debug)]
struct UiMessage {
    chat_id: ChatId,
    message_id: MessageId,
}

impl TemplateOption {
    const fn new(id: &'static str, name: &'static str, price_usdt: i64) -> Self {
        Self {
            id,
            name,
            price_usdt,
        }
    }
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
    pub cloudflare_account_id: String,
    pub cloudflare_api_token: String,
    pub cloudflare_wrangler_command: String,
    pub cloudflare_wrangler_args: String,
    pub api_service_base_url: Option<String>,
    pub api_service_api_key: Option<String>,
    pub github_owner: String,
    pub template_catalog_url: String,
    pub admin_contact: String,
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
    pub namecheap: NamecheapConfig,
}

#[derive(Clone)]
pub enum RequiredGroup {
    Id(i64),
    Username(String),
}

impl TelegramUiConfig {
    pub fn from_config(config: &Config) -> Self {
        Self {
            required_group: parse_required_group(&config.telegram_required_group_id),
            generation_primary_provider: config.generation_primary_provider,
            agent_timeout_seconds: config.agent_timeout_seconds,
            gemini_agent_command: config.gemini_agent_command.clone(),
            gemini_agent_args: config.gemini_agent_args.clone(),
            codex_agent_command: config.codex_agent_command.clone(),
            codex_agent_args: config.codex_agent_args.clone(),
            generated_sites_dir: config.generated_sites_dir.clone(),
            cloudflare_account_id: config.cloudflare_account_id.clone(),
            cloudflare_api_token: config.cloudflare_api_token.clone(),
            cloudflare_wrangler_command: config.cloudflare_wrangler_command.clone(),
            cloudflare_wrangler_args: config.cloudflare_wrangler_args.clone(),
            api_service_base_url: config.api_service_base_url.clone(),
            api_service_api_key: config.api_service_api_key.clone(),
            github_owner: config.github_owner.clone(),
            template_catalog_url: config.template_catalog_url.clone(),
            admin_contact: config.admin_contact.clone(),
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
            namecheap: config.namecheap.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct OrderDraft {
    build_mode: BuildModeDraft,
    step: DraftStep,
    site_price_usdt: Decimal,
    template_id: Option<String>,
    logo_file_id: Option<String>,
    ticker: Option<String>,
    token_name: Option<String>,
    contract_address: Option<String>,
    x_link: Option<String>,
    telegram_link: Option<String>,
    website_url: Option<String>,
    domain_quote_id: Option<Uuid>,
    domain_price_usdt: Option<Decimal>,
    total_usdt: Option<Decimal>,
    payment_chain: Option<PaymentChain>,
    order_id: Option<Uuid>,
    db_user_id: Option<Uuid>,
}

#[derive(Clone, Debug)]
enum BuildModeDraft {
    Template,
    AiFree,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DraftStep {
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

impl OrderDraft {
    fn new_ai() -> Self {
        Self::new(BuildModeDraft::AiFree, Decimal::new(50, 0), None)
    }

    fn new_template(template: TemplateOption) -> Self {
        Self::new(
            BuildModeDraft::Template,
            Decimal::new(template.price_usdt, 0),
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
            domain_quote_id: None,
            domain_price_usdt: None,
            total_usdt: None,
            payment_chain: None,
            order_id: None,
            db_user_id: None,
        }
    }
}

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

async fn handle_message(
    bot: Bot,
    msg: Message,
    config: Arc<TelegramUiConfig>,
    sessions: Sessions,
    pool: SqlitePool,
) -> ResponseResult<()> {
    if !ensure_group_member(&bot, &msg, &config).await? {
        return Ok(());
    }

    let Some(user) = msg.from.as_ref() else {
        return show_main_menu(&bot, msg.chat.id, &config).await;
    };
    let user_id = user.id;

    if let Some(text) = msg.text() {
        if let Some(template_id) = parse_template_deep_link(text) {
            return show_template_confirmation(&bot, msg.chat.id, template_id).await;
        }

        match text.trim() {
            "/start" | "/help" => return show_main_menu(&bot, msg.chat.id, &config).await,
            "/create" => return show_create_menu(&bot, msg.chat.id).await,
            "/status" => return show_status(&bot, msg.chat.id, user_id, &sessions, &pool).await,
            "/cancel" => {
                remove_session(user_id, &sessions);
                return bot
                    .send_message(msg.chat.id, "<b>Order cancelled.</b>")
                    .parse_mode(ParseMode::Html)
                    .reply_markup(main_menu_keyboard())
                    .await
                    .map(|_| ());
            }
            _ => {}
        }
    }

    handle_order_step(bot, msg, user_id, config, sessions, &pool).await
}

async fn handle_callback(
    bot: Bot,
    query: CallbackQuery,
    config: Arc<TelegramUiConfig>,
    sessions: Sessions,
    pool: SqlitePool,
) -> ResponseResult<()> {
    bot.answer_callback_query(query.id.clone()).await?;

    let Some(message) = query.message.as_ref() else {
        return Ok(());
    };
    let chat_id = message.chat().id;
    let target = UiMessage {
        chat_id,
        message_id: message.id(),
    };

    if !ensure_group_member_for_user(&bot, chat_id, query.from.id, &config).await? {
        return Ok(());
    }

    match query.data.as_deref() {
        Some("main") => edit_main_menu(&bot, target, &config).await,
        Some("create") => edit_create_menu(&bot, target).await,
        Some("template_mode") => edit_template_mode(&bot, target).await,
        Some("ai_mode") => {
            clear_screen_keyboard(&bot, target).await?;
            set_session(query.from.id, OrderDraft::new_ai(), &sessions);
            edit_ask_logo(&bot, target).await
        }
        Some("pay_menu") => edit_payment_chain_menu(&bot, target).await,
        Some("status") => edit_status(&bot, target, query.from.id, &sessions, &pool).await,
        Some("contact_admin") => edit_contact_admin(&bot, target, &config).await,
        Some("pay_erc20") => {
            edit_payment_chain_and_ask_tx(
                &bot,
                target,
                query.from.id,
                PaymentChain::Erc20,
                &config.usdt_erc20_address,
                &sessions,
                &pool,
            )
            .await
        }
        Some("pay_tron") => {
            edit_payment_chain_and_ask_tx(
                &bot,
                target,
                query.from.id,
                PaymentChain::Tron,
                &config.usdt_tron_address,
                &sessions,
                &pool,
            )
            .await
        }
        Some("pay_sol") => {
            edit_payment_chain_and_ask_tx(
                &bot,
                target,
                query.from.id,
                PaymentChain::Sol,
                &config.usdt_sol_address,
                &sessions,
                &pool,
            )
            .await
        }
        Some(data) if data.starts_with("select_tpl:") => {
            let template_id = data.trim_start_matches("select_tpl:");
            edit_template_confirmation(&bot, target, template_id).await
        }
        Some(data) if data.starts_with("confirm_tpl:") => {
            match find_template(data.trim_start_matches("confirm_tpl:")) {
                Some(template) => {
                    clear_screen_keyboard(&bot, target).await?;
                    set_session(query.from.id, OrderDraft::new_template(template), &sessions);
                    edit_ask_logo(&bot, target).await
                }
                None => {
                    edit_screen(
                        &bot,
                        target,
                        "<b>Template not found.</b>\n\nPlease choose again.",
                        template_keyboard(),
                    )
                    .await
                }
            }
        }
        Some("cancel") => {
            remove_session(query.from.id, &sessions);
            edit_screen(
                &bot,
                target,
                "🛑 <b>Order cancelled</b>\n\nYou can start a new meme website order anytime.",
                main_menu_keyboard(),
            )
            .await
        }
        _ => edit_main_menu(&bot, target, &config).await,
    }
}

async fn handle_order_step(
    bot: Bot,
    msg: Message,
    user_id: UserId,
    config: Arc<TelegramUiConfig>,
    sessions: Sessions,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let Some(mut draft) = get_session(user_id, &sessions) else {
        return show_main_menu(
            &bot,
            msg.chat.id,
            &TelegramUiConfig {
                required_group: None,
                template_catalog_url: String::new(),
                admin_contact: "@admin".to_string(),
                usdt_erc20_address: String::new(),
                usdt_tron_address: String::new(),
                usdt_sol_address: String::new(),
                usdt_erc20_rpc_url: String::new(),
                usdt_erc20_contract: String::new(),
                usdt_erc20_min_confirmations: 12,
                tron_grid_base_url: String::new(),
                tron_grid_api_key: None,
                usdt_tron_contract: String::new(),
                usdt_tron_min_confirmations: 20,
                solana_rpc_url: String::new(),
                usdt_sol_mint: String::new(),
                usdt_sol_min_confirmations: 1,
                namecheap: config.namecheap.clone(),
                generation_primary_provider: config.generation_primary_provider,
                agent_timeout_seconds: config.agent_timeout_seconds,
                gemini_agent_command: config.gemini_agent_command.clone(),
                gemini_agent_args: config.gemini_agent_args.clone(),
                codex_agent_command: config.codex_agent_command.clone(),
                codex_agent_args: config.codex_agent_args.clone(),
                generated_sites_dir: config.generated_sites_dir.clone(),
                cloudflare_account_id: config.cloudflare_account_id.clone(),
                cloudflare_api_token: config.cloudflare_api_token.clone(),
                cloudflare_wrangler_command: config.cloudflare_wrangler_command.clone(),
                cloudflare_wrangler_args: config.cloudflare_wrangler_args.clone(),
                api_service_base_url: config.api_service_base_url.clone(),
                api_service_api_key: config.api_service_api_key.clone(),
                github_owner: config.github_owner.clone(),
            },
        )
        .await;
    };

    match draft.step {
        DraftStep::WaitingLogo => {
            let Some(photo) = msg.photo().and_then(|photos| photos.last()) else {
                return bot
                    .send_message(
                        msg.chat.id,
                        "🖼 <b>Step 1/7</b>\n\nUpload your token logo image.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            draft.logo_file_id = Some(photo.file.id.0.clone());
            draft.step = DraftStep::WaitingTicker;
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                "✅ <b>Logo received.</b>\n\n🔤 <b>Step 2/7</b>\nSend ticker, for example: <code>PEPE</code>",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        DraftStep::WaitingTicker => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "🔤 <b>Step 2/7</b>\n\nSend ticker as text.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            let ticker = text.to_ascii_uppercase();
            if ticker.len() < 2
                || ticker.len() > 12
                || !ticker.chars().all(|ch| ch.is_ascii_alphanumeric())
            {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid ticker.</b>\n\nUse 2-12 letters/numbers, for example: <code>PEPE</code>",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            draft.ticker = Some(ticker);
            draft.step = DraftStep::WaitingTokenName;
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                "✅ <b>Ticker saved.</b>\n\n🏷 <b>Step 3/7</b>\nSend token name, for example: <code>Pepe Coin</code>",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        DraftStep::WaitingTokenName => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "🏷 <b>Step 3/7</b>\n\nSend token name as text.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            if text.len() < 2 || text.len() > 48 {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid token name.</b>\n\nUse 2-48 characters.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            draft.token_name = Some(text.to_string());
            draft.step = DraftStep::WaitingContractAddress;
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                "✅ <b>Token name saved.</b>\n\n🔗 <b>Step 4/7</b>\nSend contract address.",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        DraftStep::WaitingContractAddress => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(
                        msg.chat.id,
                        "🔗 <b>Step 4/7</b>\n\nSend contract address as text.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            if validate_contract_address(text).is_err() {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid contract address.</b>\n\nSend an EVM, TRON, or SOL address.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            draft.contract_address = Some(text.to_string());
            draft.step = DraftStep::WaitingXLink;
            set_session(user_id, draft, &sessions);
            bot.send_message(msg.chat.id, "✅ <b>Contract address saved.</b>\n\n✕ <b>Step 5/7</b>\nSend X link, for example:\n<code>https://x.com/yourproject</code>")
                .parse_mode(ParseMode::Html)
                .await
                .map(|_| ())
        }
        DraftStep::WaitingXLink => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "✕ <b>Step 5/7</b>\n\nSend X link as text.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            if validate_x_url(text).is_err() {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid X link.</b>\n\nUse:\n<code>https://x.com/username</code>",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            draft.x_link = Some(text.to_string());
            draft.step = DraftStep::WaitingTelegramLink;
            set_session(user_id, draft, &sessions);
            bot.send_message(msg.chat.id, "✅ <b>X link saved.</b>\n\n💬 <b>Step 6/7</b>\nSend Telegram link, for example:\n<code>https://t.me/yourgroup</code>")
                .parse_mode(ParseMode::Html)
                .await
                .map(|_| ())
        }
        DraftStep::WaitingTelegramLink => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(
                        msg.chat.id,
                        "💬 <b>Step 6/7</b>\n\nSend Telegram link as text.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            if validate_telegram_url(text).is_err() {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid Telegram link.</b>\n\nUse:\n<code>https://t.me/yourgroup</code>",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            draft.telegram_link = Some(text.to_string());
            draft.step = DraftStep::WaitingWebsiteUrl;
            set_session(user_id, draft, &sessions);
            bot.send_message(msg.chat.id, "✅ <b>Telegram link saved.</b>\n\n🌐 <b>Step 7/7</b>\nSend desired website URL/domain, for example:\n<code>pepe.xyz</code>\n\nI will check availability and price.")
                .parse_mode(ParseMode::Html)
                .await
                .map(|_| ())
        }
        DraftStep::WaitingWebsiteUrl => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(
                        msg.chat.id,
                        "🌐 <b>Step 7/7</b>\n\nSend website URL/domain as text.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            let domain = normalize_domain(text);
            if validate_domain(&domain).is_err() {
                return bot
                    .send_message(msg.chat.id, "<b>Invalid or unsupported domain.</b>\n\nSupported TLDs:\n<code>.com</code> <code>.net</code> <code>.org</code> <code>.xyz</code> <code>.fun</code> <code>.site</code> <code>.online</code> <code>.io</code> <code>.co</code> <code>.me</code> <code>.cc</code> <code>.app</code> <code>.dev</code> <code>.world</code> <code>.space</code> <code>.club</code> <code>.store</code> <code>.shop</code>")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }

            let registrar: Box<dyn DomainRegistrar> = if let (Some(base_url), Some(api_key)) = (
                config.api_service_base_url.as_deref(),
                config.api_service_api_key.as_deref(),
            ) {
                Box::new(HttpClientDomainRegistrar::new(
                    base_url.to_string(),
                    api_key.to_string(),
                ))
            } else {
                Box::new(NamecheapRegistrar::new(config.namecheap.clone()))
            };
            match quote_domain_with_retries(registrar.as_ref(), &domain).await {
                Ok(quote) if quote.available && !quote.is_premium => {
                    let db_user_id = match db::upsert_user(
                        pool,
                        user_id.0 as i64,
                        None,
                        None,
                        None,
                        true,
                    )
                    .await
                    {
                        Ok(id) => id,
                        Err(e) => {
                            warn!(
                                ?e,
                                user_id = user_id.0 as i64,
                                "failed to upsert user for domain quote"
                            );
                            return bot
                                    .send_message(
                                        msg.chat.id,
                                        "<b>Internal error.</b>\n\nFailed to record domain quote. Please try again.",
                                    )
                                    .parse_mode(ParseMode::Html)
                                    .await
                                    .map(|_| ());
                        }
                    };
                    draft.website_url = Some(domain);
                    draft.db_user_id = Some(db_user_id);
                    draft.domain_price_usdt = Some(quote.price_usd);
                    let total = draft.site_price_usdt + quote.price_usd;
                    match db::record_domain_quote(pool, db_user_id, &quote, quote.price_usd).await {
                        Ok(quote_id) => draft.domain_quote_id = Some(quote_id),
                        Err(e) => {
                            warn!(?e, "failed to record domain quote");
                            return bot
                                .send_message(
                                    msg.chat.id,
                                    "<b>Internal error.</b>\n\nFailed to record domain quote. Please try again.",
                                )
                                .parse_mode(ParseMode::Html)
                                .await
                                .map(|_| ());
                        }
                    }
                    draft.total_usdt = Some(total);
                    draft.step = DraftStep::ReadyToPay;
                    set_session(user_id, draft.clone(), &sessions);
                    bot.send_message(
                        msg.chat.id,
                        format!(
                            "✅ <b>Domain available.</b>\n\nDomain price: <b>{}</b> USDT\nWebsite price: <b>{}</b> USDT\n\nTotal: <b>{}</b> USDT\n\nChoose payment chain. After payment, send the TX hash and I will verify it on-chain.",
                            quote.price_usd, draft.site_price_usdt, total
                        ),
                    )
                    .parse_mode(ParseMode::Html)
                    .reply_markup(payment_chain_keyboard())
                    .await
                    .map(|_| ())
                }
                Ok(_) => bot
                    .send_message(
                        msg.chat.id,
                        "<b>Domain unavailable or premium.</b>\n\nPlease send another domain.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ()),
                Err(error) => bot
                    .send_message(
                        msg.chat.id,
                        format!(
                            "<b>Domain check failed after retries.</b>\n\n{}\n\nContact admin if this persists.",
                            html_escape(&domain_error_for_user(&error))
                        ),
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ()),
            }
        }
        DraftStep::ReadyToPay => bot
            .send_message(
                msg.chat.id,
                "💳 <b>Payment</b>\n\nChoose a payment chain first.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(payment_chain_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingTx => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "💳 <b>Payment</b>\n\nPaste TX hash as text.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            if !looks_like_tx_hash(text) {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid TX format.</b>\n\nPaste the transaction hash.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }

            let mut status_parts: Vec<String> = vec![];

            if let Some(order_id) = draft.order_id {
                let chain = draft.payment_chain.unwrap_or(PaymentChain::Erc20);
                let payment_info = match db::get_order_payment_info(pool, order_id).await {
                    Ok(info) => info,
                    Err(e) => {
                        warn!(?e, order_id = %order_id, "failed to load payment info");
                        return bot
                            .send_message(msg.chat.id, "⚠️ <b>Payment check failed.</b>\n\nOrder not found. Contact admin.")
                            .parse_mode(ParseMode::Html)
                            .await
                            .map(|_| ());
                    }
                };
                if payment_info.payment_status == "paid" {
                    return bot
                        .send_message(msg.chat.id, "<b>Payment already confirmed.</b>\n\nYour website is queued for generation.")
                        .parse_mode(ParseMode::Html)
                        .reply_markup(status_keyboard())
                        .await
                        .map(|_| ());
                }
                match db::tx_hash_already_used(pool, chain, text).await {
                    Ok(true) => {
                        return bot
                            .send_message(msg.chat.id, "<b>TX already used.</b>\n\nSend a transaction hash that has not been used for another order.")
                            .parse_mode(ParseMode::Html)
                            .await
                            .map(|_| ());
                    }
                    Ok(false) => {}
                    Err(e) => warn!(?e, "failed to check tx reuse"),
                }

                let expected_to_address =
                    payment_address_for_chain(&config, chain).trim().to_string();
                if expected_to_address.is_empty() {
                    return bot
                        .send_message(
                            msg.chat.id,
                            "<b>Payment address is not configured.</b>\n\nSet the matching USDT receive address in <code>.env</code>, then restart the bot.",
                        )
                        .parse_mode(ParseMode::Html)
                        .await
                        .map(|_| ());
                }
                let verifier = ChainPaymentVerifier::new(
                    config.usdt_erc20_rpc_url.clone(),
                    config.usdt_erc20_contract.clone(),
                    config.usdt_erc20_min_confirmations,
                    config.tron_grid_base_url.clone(),
                    config.tron_grid_api_key.clone(),
                    config.usdt_tron_contract.clone(),
                    config.usdt_tron_min_confirmations,
                    config.solana_rpc_url.clone(),
                    config.usdt_sol_mint.clone(),
                    config.usdt_sol_min_confirmations,
                );
                let service = PaymentService::new(verifier);
                let tx_submitted_at = chrono::Utc::now();
                let verify_request = PaymentVerificationRequest {
                    order_id,
                    chain,
                    tx_hash: text.to_string(),
                    expected_to_address,
                    required_amount_usdt: payment_info.amount_usdt,
                    order_created_at: payment_info.created_at,
                    submitted_at: tx_submitted_at,
                    expires_at: payment_info
                        .payment_expires_at
                        .unwrap_or_else(chrono::Utc::now),
                };
                let verification = verify_payment_with_retries(&service, verify_request).await;
                if let Err(e) = db::record_payment_attempt(
                    pool,
                    payment_info.id,
                    payment_info.user_id,
                    chain,
                    text,
                    verification.as_ref(),
                )
                .await
                {
                    warn!(?e, order_id = %order_id, "failed to record payment attempt");
                }

                let outcome = match verification {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        return bot
                            .send_message(
                                msg.chat.id,
                                format!(
                                    "❌ <b>Payment not confirmed.</b>\n\n{}\n\nIf you just paid, wait for confirmations and send the TX again.",
                                    html_escape(&payment_error_for_user(&error))
                                ),
                            )
                            .parse_mode(ParseMode::Html)
                            .await
                            .map(|_| ());
                    }
                };

                if let Err(e) =
                    db::confirm_payment_and_mark_paid(pool, order_id, chain, text, &outcome).await
                {
                    warn!(?e, order_id = %order_id, "failed to confirm payment transaction");
                    return bot
                        .send_message(msg.chat.id, "⚠️ <b>Payment already used or could not be recorded.</b>\n\nContact admin.")
                        .parse_mode(ParseMode::Html)
                        .await
                        .map(|_| ());
                }
                status_parts.push(format!(
                    "Payment verified: {} USDT, confirmations: {}.",
                    outcome.observed_amount_usdt,
                    outcome
                        .confirmations
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "confirmed".to_string())
                ));

                if let (Some(db_uid), Some(input)) =
                    (draft.db_user_id, website_input_from_draft(&draft))
                {
                    match db::create_website(pool, db_uid, order_id, &input).await {
                        Ok(website_id) => {
                            if let Some(quote_id) = draft.domain_quote_id
                                && let Err(e) =
                                    db::attach_quote_website(pool, quote_id, website_id).await
                            {
                                warn!(?e, quote_id = %quote_id, website_id = %website_id, "failed to attach quote to website");
                            }
                            let (domain_ready, purchase_message) =
                                maybe_purchase_domain_after_tx(&config, pool, &draft).await;
                            if !purchase_message.is_empty() {
                                status_parts.push(purchase_message.trim().to_string());
                            }
                            if !domain_ready {
                                if let Err(e) = db::mark_order_manual_review(pool, order_id).await {
                                    warn!(?e, order_id = %order_id, "failed to mark manual review");
                                }
                                status_parts.push(
                                    "Website generation paused after repeated domain setup failures. Contact admin."
                                        .to_string(),
                                );
                            } else if let Err(e) =
                                db::enqueue_generation_job(pool, order_id, website_id).await
                            {
                                warn!(?e, order_id = %order_id, "failed to enqueue generation job");
                                status_parts.push("⚠️ Failed to queue generation.".to_string());
                            } else {
                                status_parts.push("Website queued for generation.".to_string());
                            }
                        }
                        Err(e) => {
                            warn!(?e, order_id = %order_id, "failed to create website record");
                            let _ = db::mark_order_manual_review(pool, order_id).await;
                            status_parts.push("⚠️ Failed to create website record.".to_string());
                        }
                    }
                } else {
                    warn!(order_id = %order_id, "paid order is missing user id or website input");
                    let _ = db::mark_order_manual_review(pool, order_id).await;
                    status_parts.push(
                        "⚠️ Payment was recorded, but website input is incomplete. Contact admin."
                            .to_string(),
                    );
                }
            } else {
                status_parts.push("⚠️ No order found. Please restart with /create.".to_string());
            }

            draft.step = DraftStep::PaymentConfirmed;
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                format!(
                    "✅ <b>TX received.</b>\n\n<b>Payment confirmed.</b>\n\n{}",
                    status_parts.join("\n")
                ),
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(status_keyboard())
            .await
            .map(|_| ())
        }
        DraftStep::PaymentConfirmed => bot
            .send_message(
                msg.chat.id,
                "<b>Payment already confirmed.</b>\n\nYour website is queued for generation.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(status_keyboard())
            .await
            .map(|_| ()),
    }
}

async fn ensure_group_member(
    bot: &Bot,
    msg: &Message,
    config: &TelegramUiConfig,
) -> ResponseResult<bool> {
    let Some(user) = msg.from.as_ref() else {
        return Ok(true);
    };
    ensure_group_member_for_user(bot, msg.chat.id, user.id, config).await
}

async fn ensure_group_member_for_user(
    bot: &Bot,
    chat_id: ChatId,
    user_id: UserId,
    config: &TelegramUiConfig,
) -> ResponseResult<bool> {
    let Some(group) = &config.required_group else {
        return Ok(true);
    };

    let result = match group {
        RequiredGroup::Id(id) => bot.get_chat_member(ChatId(*id), user_id).await,
        RequiredGroup::Username(username) => {
            bot.get_chat_member(Recipient::ChannelUsername(username.clone()), user_id)
                .await
        }
    };

    match result {
        Ok(member) if member.is_present() => Ok(true),
        _ => {
            bot.send_message(
                chat_id,
                "<b>Access required</b>\n\nPlease join the support group before using this bot:\nhttps://t.me/MeMe_Web_Support",
            )
            .parse_mode(ParseMode::Html)
            .await?;
            Ok(false)
        }
    }
}

async fn send_screen(
    bot: &Bot,
    chat_id: ChatId,
    text: impl Into<String>,
    reply_markup: InlineKeyboardMarkup,
) -> ResponseResult<()> {
    bot.send_message(chat_id, text)
        .parse_mode(ParseMode::Html)
        .reply_markup(reply_markup)
        .await
        .map(|_| ())
}

async fn edit_screen(
    bot: &Bot,
    target: UiMessage,
    text: impl Into<String>,
    reply_markup: InlineKeyboardMarkup,
) -> ResponseResult<()> {
    match bot
        .edit_message_text(target.chat_id, target.message_id, text)
        .parse_mode(ParseMode::Html)
        .reply_markup(reply_markup)
        .await
    {
        Ok(_) => Ok(()),
        Err(error) if is_message_not_modified(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

async fn clear_screen_keyboard(bot: &Bot, target: UiMessage) -> ResponseResult<()> {
    match bot
        .edit_message_reply_markup(target.chat_id, target.message_id)
        .await
    {
        Ok(_) => Ok(()),
        Err(error) if is_message_not_modified(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

fn is_message_not_modified(error: &teloxide::RequestError) -> bool {
    error
        .to_string()
        .to_ascii_lowercase()
        .contains("message is not modified")
}

fn main_menu_text(config: &TelegramUiConfig) -> String {
    format!(
        "🚀 <b>Auto Meme Web Bot</b>\n\n<b>Create meme websites in minutes</b>\nTemplate mode, AI mode, domain check, payment, generation, and launch.\n\n<b>Access group</b>\nhttps://t.me/MeMe_Web_Support\n\n<b>Template catalog</b>\n{}",
        html_escape(&config.template_catalog_url)
    )
}

fn create_menu_text() -> &'static str {
    "🧩 <b>Create Website</b>\n\nChoose how the website should be built.\n\n<b>Template Mode</b>\nFast launch with fixed visual templates.\n\n<b>AI Free Creation</b>\nCustom style generated by agent."
}

fn template_mode_text() -> &'static str {
    "📦 <b>Template Mode</b>\n\nSelect one template below.\n\nPrices are shown in USDT. After confirmation, I will ask for logo, token info, links, and domain."
}

fn template_confirmation_text(template: TemplateOption) -> String {
    format!(
        "📦 <b>Template Selected</b>\n\n<b>{}</b>\nPrice: <b>{}</b> USDT\n\nConfirm this template to continue to Step 1: logo upload.",
        html_escape(template.name),
        template.price_usdt
    )
}

fn ask_logo_text() -> &'static str {
    "🖼 <b>Step 1/7 · Logo</b>\n\nUpload your token logo image.\n\nAfter receiving the logo, I will ask for ticker."
}

async fn status_text(user_id: UserId, sessions: &Sessions, pool: &SqlitePool) -> String {
    if let Some(draft) = get_session(user_id, sessions) {
        return format!(
            "📊 <b>Current Order</b>\n\nStatus: <code>{:?}</code>\nMode: <b>{}</b>\nWebsite price: <b>{}</b> USDT",
            draft.step,
            build_mode_label(&draft.build_mode),
            draft.site_price_usdt
        );
    }
    match db::get_user_order(pool, user_id.0 as i64).await {
        Ok(Some(order)) => format!(
            "📊 <b>Order Status</b>\n\nPayment: {}\nSite: {}\nAmount: {} USDT\nChain: {}\nDomain: {}",
            order.payment_status,
            order.site_status,
            order.amount_usdt,
            order.payment_chain.as_deref().unwrap_or("—"),
            order.requested_domain.as_deref().unwrap_or("—"),
        ),
        Ok(None) => "📊 <b>Order Status</b>\n\nNo active order. Start with /create.".to_string(),
        Err(_) => "📊 <b>Order Status</b>\n\nUnable to check order status. Please try again later."
            .to_string(),
    }
}

fn payment_chain_text() -> &'static str {
    "💳 <b>Payment</b>\n\nChoose the USDT chain you will use.\n\nAfter you choose a chain, I will show the address and ask you to paste the TX hash."
}

async fn show_main_menu(
    bot: &Bot,
    chat_id: ChatId,
    config: &TelegramUiConfig,
) -> ResponseResult<()> {
    send_screen(bot, chat_id, main_menu_text(config), main_menu_keyboard()).await
}

async fn edit_main_menu(
    bot: &Bot,
    target: UiMessage,
    config: &TelegramUiConfig,
) -> ResponseResult<()> {
    edit_screen(bot, target, main_menu_text(config), main_menu_keyboard()).await
}

async fn show_create_menu(bot: &Bot, chat_id: ChatId) -> ResponseResult<()> {
    send_screen(bot, chat_id, create_menu_text(), create_mode_keyboard()).await
}

async fn edit_create_menu(bot: &Bot, target: UiMessage) -> ResponseResult<()> {
    edit_screen(bot, target, create_menu_text(), create_mode_keyboard()).await
}

async fn edit_template_mode(bot: &Bot, target: UiMessage) -> ResponseResult<()> {
    edit_screen(bot, target, template_mode_text(), template_keyboard()).await
}

async fn show_template_confirmation(
    bot: &Bot,
    chat_id: ChatId,
    template_id: &str,
) -> ResponseResult<()> {
    let Some(template) = find_template(template_id) else {
        return send_screen(
            bot,
            chat_id,
            "<b>Template not found.</b>\n\nPlease choose again.",
            template_keyboard(),
        )
        .await;
    };

    send_screen(
        bot,
        chat_id,
        template_confirmation_text(template),
        template_confirmation_keyboard(template_id),
    )
    .await
}

async fn edit_template_confirmation(
    bot: &Bot,
    target: UiMessage,
    template_id: &str,
) -> ResponseResult<()> {
    let Some(template) = find_template(template_id) else {
        return edit_screen(
            bot,
            target,
            "<b>Template not found.</b>\n\nPlease choose again.",
            template_keyboard(),
        )
        .await;
    };

    edit_screen(
        bot,
        target,
        template_confirmation_text(template),
        template_confirmation_keyboard(template_id),
    )
    .await
}

async fn edit_ask_logo(bot: &Bot, target: UiMessage) -> ResponseResult<()> {
    edit_screen(bot, target, ask_logo_text(), cancel_keyboard()).await
}

async fn show_status(
    bot: &Bot,
    chat_id: ChatId,
    user_id: UserId,
    sessions: &Sessions,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    send_screen(
        bot,
        chat_id,
        status_text(user_id, sessions, pool).await,
        main_menu_keyboard(),
    )
    .await
}

async fn edit_status(
    bot: &Bot,
    target: UiMessage,
    user_id: UserId,
    sessions: &Sessions,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    edit_screen(
        bot,
        target,
        status_text(user_id, sessions, pool).await,
        main_menu_keyboard(),
    )
    .await
}

async fn edit_payment_chain_menu(bot: &Bot, target: UiMessage) -> ResponseResult<()> {
    edit_screen(bot, target, payment_chain_text(), payment_chain_keyboard()).await
}

async fn edit_contact_admin(
    bot: &Bot,
    target: UiMessage,
    config: &TelegramUiConfig,
) -> ResponseResult<()> {
    edit_screen(
        bot,
        target,
        format!(
            "👤 <b>Contact admin</b>\n\nFor manual handling, quota issues, or premium domains:\n\n{}",
            html_escape(&config.admin_contact)
        ),
        main_menu_keyboard(),
    )
    .await
}

async fn edit_payment_chain_and_ask_tx(
    bot: &Bot,
    target: UiMessage,
    user_id: UserId,
    chain: PaymentChain,
    address: &str,
    sessions: &Sessions,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let Some(mut draft) = get_session(user_id, sessions) else {
        return edit_screen(
            bot,
            target,
            "<b>No active order.</b>\n\nStart with /create.",
            main_menu_keyboard(),
        )
        .await;
    };

    let Some(total) = draft.total_usdt else {
        return edit_screen(
            bot,
            target,
            "<b>Payment not ready.</b>\n\nComplete logo, token links, and website URL before payment.",
            cancel_keyboard(),
        )
        .await;
    };

    if address.trim().is_empty() {
        return edit_screen(
            bot,
            target,
            "<b>Payment address is not configured.</b>\n\nSet the matching USDT receive address in <code>.env</code>, then restart the bot.",
            main_menu_keyboard(),
        )
        .await;
    }

    let db_user_id = if let Some(db_user_id) = draft.db_user_id {
        db_user_id
    } else {
        match db::upsert_user(pool, user_id.0 as i64, None, None, None, true).await {
            Ok(id) => id,
            Err(e) => {
                warn!(?e, user_id = user_id.0 as i64, "failed to upsert user");
                return edit_screen(
                    bot,
                    target,
                    "<b>Internal error.</b>\n\nFailed to create user record. Please try again.",
                    main_menu_keyboard(),
                )
                .await;
            }
        }
    };

    let build_mode = match draft.build_mode {
        BuildModeDraft::Template => crate::models::BuildMode::Template,
        BuildModeDraft::AiFree => crate::models::BuildMode::AiFree,
    };
    let template_id = draft.template_id.as_deref();
    let order_id = match db::create_site_order(
        pool,
        db_user_id,
        build_mode,
        template_id,
        total,
        draft.domain_quote_id,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            warn!(?e, "failed to create site order");
            return edit_screen(
                bot,
                target,
                "<b>Internal error.</b>\n\nFailed to create order. Please try again.",
                main_menu_keyboard(),
            )
            .await;
        }
    };

    if let Err(e) = db::attach_payment_chain(pool, order_id, chain).await {
        warn!(?e, order_id = %order_id, "failed to attach payment chain");
    }

    draft.payment_chain = Some(chain);
    draft.step = DraftStep::WaitingTx;
    draft.order_id = Some(order_id);
    draft.db_user_id = Some(db_user_id);
    set_session(user_id, draft, sessions);

    edit_screen(
        bot,
        target,
        payment_instruction(order_id, total, chain, address),
        cancel_keyboard(),
    )
    .await
}

fn set_session(user_id: UserId, draft: OrderDraft, sessions: &Sessions) {
    sessions
        .lock()
        .expect("sessions lock")
        .insert(user_id, draft);
}

fn get_session(user_id: UserId, sessions: &Sessions) -> Option<OrderDraft> {
    sessions
        .lock()
        .expect("sessions lock")
        .get(&user_id)
        .cloned()
}

fn remove_session(user_id: UserId, sessions: &Sessions) {
    sessions.lock().expect("sessions lock").remove(&user_id);
}

async fn maybe_purchase_domain_after_tx(
    config: &TelegramUiConfig,
    pool: &SqlitePool,
    draft: &OrderDraft,
) -> (bool, String) {
    if !config.namecheap.auto_purchase_after_tx {
        return (
            false,
            "\n\n<b>Domain purchase:</b> skipped. Set <code>NAMECHEAP_AUTO_PURCHASE_AFTER_TX=true</code> to enable automatic Namecheap purchase after TX.".to_string(),
        );
    }

    let Some(domain) = draft.website_url.as_deref() else {
        return (
            false,
            "\n\n<b>Domain purchase:</b> skipped because no domain is attached.".to_string(),
        );
    };
    let Some(quote_id) = draft.domain_quote_id else {
        return (
            false,
            "\n\n<b>Domain purchase:</b> skipped because no persisted quote is attached."
                .to_string(),
        );
    };
    if let Err(error) = db::record_domain_purchase_started(pool, quote_id).await {
        warn!(?error, quote_id = %quote_id, "failed to record domain purchase start");
    }

    let registrar: Box<dyn DomainRegistrar> = if let (Some(base_url), Some(api_key)) = (
        config.api_service_base_url.as_deref(),
        config.api_service_api_key.as_deref(),
    ) {
        Box::new(HttpClientDomainRegistrar::new(
            base_url.to_string(),
            api_key.to_string(),
        ))
    } else {
        Box::new(NamecheapRegistrar::new(config.namecheap.clone()))
    };
    match purchase_domain_with_retries(registrar.as_ref(), domain).await {
        Ok(result) => {
            if let Err(error) = db::record_domain_purchase_success(pool, quote_id, &result).await {
                warn!(?error, quote_id = %quote_id, "failed to record domain purchase success");
            }
            let dns_result = setup_cloudflare_dns_after_purchase_with_retries(config, domain).await;
            let dns_ready = match &dns_result {
                Ok(zone) => {
                    if let Err(error) = db::record_domain_dns_result(pool, quote_id, Ok(zone)).await
                    {
                        warn!(?error, quote_id = %quote_id, "failed to record dns success");
                    }
                    true
                }
                Err(message) => {
                    if let Err(error) =
                        db::record_domain_dns_result(pool, quote_id, Err(message.as_str())).await
                    {
                        warn!(?error, quote_id = %quote_id, "failed to record dns failure");
                    }
                    false
                }
            };
            let dns_message = match dns_result {
                Ok(zone) => format!(
                    "\n<b>DNS configured.</b> Cloudflare zone <code>{}</code>; nameservers sent to Namecheap.",
                    html_escape(&zone.zone_id)
                ),
                Err(message) => message,
            };
            (
                dns_ready,
                format!(
                    "\n\n<b>Domain purchased.</b>\nOrder ID: <code>{}</code>\nTransaction ID: <code>{}</code>{}",
                    html_escape(&result.namecheap_order_id),
                    html_escape(&result.namecheap_transaction_id),
                    dns_message
                ),
            )
        }
        Err(error) => {
            if let Err(db_error) = db::mark_domain_purchase_failed(pool, quote_id).await {
                warn!(?db_error, quote_id = %quote_id, "failed to record domain purchase failure");
            }
            (
                false,
                format!(
                    "\n\n<b>Domain purchase failed.</b>\n{}",
                    html_escape(&domain_error_for_user(&error))
                ),
            )
        }
    }
}

async fn verify_payment_with_retries<V>(
    service: &PaymentService<V>,
    request: PaymentVerificationRequest,
) -> Result<PaymentVerificationOutcome, PaymentVerificationError>
where
    V: PaymentVerifier,
{
    let mut last_error = None;
    for attempt in 1..=EXTERNAL_STEP_MAX_ATTEMPTS {
        match service.verify_tx(request.clone()).await {
            Ok(outcome) => return Ok(outcome),
            Err(error)
                if retryable_payment_error(&error) && attempt < EXTERNAL_STEP_MAX_ATTEMPTS =>
            {
                last_error = Some(error);
                sleep(TokioDuration::from_secs(attempt as u64 * 2)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or(PaymentVerificationError::Provider(
        "payment verification failed after retries".to_string(),
    )))
}

fn retryable_payment_error(error: &PaymentVerificationError) -> bool {
    matches!(error, PaymentVerificationError::Provider(_))
}

async fn quote_domain_with_retries(
    registrar: &dyn DomainRegistrar,
    domain: &str,
) -> Result<crate::domain::DomainQuote, DomainError> {
    let request = DomainQuoteRequest {
        domain: domain.to_string(),
    };
    let mut last_error = None;
    for attempt in 1..=EXTERNAL_STEP_MAX_ATTEMPTS {
        match registrar.quote(&request).await {
            Ok(quote) => return Ok(quote),
            Err(error)
                if retryable_domain_error(&error) && attempt < EXTERNAL_STEP_MAX_ATTEMPTS =>
            {
                warn!(%domain, attempt, error = %error, "domain quote failed; retrying");
                last_error = Some(error);
                sleep(TokioDuration::from_secs(attempt as u64 * 2)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error
        .unwrap_or_else(|| DomainError::Namecheap("domain quote failed after retries".to_string())))
}

async fn purchase_domain_with_retries(
    registrar: &dyn DomainRegistrar,
    domain: &str,
) -> Result<crate::domain::DomainPurchaseResult, DomainError> {
    let request = DomainPurchaseRequest {
        domain: domain.to_string(),
        years: 1,
        privacy_enabled: true,
    };
    let mut last_error = None;
    for attempt in 1..=EXTERNAL_STEP_MAX_ATTEMPTS {
        match registrar.purchase(&request).await {
            Ok(result) => return Ok(result),
            Err(error)
                if retryable_domain_error(&error) && attempt < EXTERNAL_STEP_MAX_ATTEMPTS =>
            {
                warn!(%domain, attempt, error = %error, "domain purchase failed; retrying");
                last_error = Some(error);
                sleep(TokioDuration::from_secs(attempt as u64 * 3)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        DomainError::Namecheap("domain purchase failed after retries".to_string())
    }))
}

async fn setup_cloudflare_dns_after_purchase_with_retries(
    config: &TelegramUiConfig,
    domain: &str,
) -> Result<crate::domain::CloudflareZoneResult, String> {
    let mut last_error = None;
    for attempt in 1..=EXTERNAL_STEP_MAX_ATTEMPTS {
        match setup_cloudflare_dns_after_purchase(config, domain).await {
            Ok(zone) => return Ok(zone),
            Err(error) if attempt < EXTERNAL_STEP_MAX_ATTEMPTS => {
                warn!(%domain, attempt, error = %error, "dns setup failed; retrying");
                last_error = Some(error);
                sleep(TokioDuration::from_secs(attempt as u64 * 3)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        "\n<b>DNS setup failed.</b> Repeated attempts failed. Contact admin.".to_string()
    }))
}

fn retryable_domain_error(error: &DomainError) -> bool {
    matches!(
        error,
        DomainError::Http(_) | DomainError::Namecheap(_) | DomainError::Cloudflare(_)
    )
}

async fn setup_cloudflare_dns_after_purchase(
    config: &TelegramUiConfig,
    domain: &str,
) -> Result<crate::domain::CloudflareZoneResult, String> {
    if let (Some(base_url), Some(api_key)) = (
        config.api_service_base_url.as_deref(),
        config.api_service_api_key.as_deref(),
    ) {
        let dns = HttpClientDnsProvisioner::new(base_url.to_string(), api_key.to_string());
        return match dns.create_zone(domain).await {
            Ok(zone) => match dns
                .set_namecheap_nameservers(domain, &zone.nameservers)
                .await
            {
                Ok(()) => Ok(zone),
                Err(error) => Err(format!(
                    "\n<b>DNS nameserver update failed.</b> {}",
                    html_escape(&domain_error_for_user(&error))
                )),
            },
            Err(error) => Err(format!(
                "\n<b>Cloudflare zone creation failed.</b> {}",
                html_escape(&domain_error_for_user(&error))
            )),
        };
    }

    let cloudflare = CloudflareDnsProvisioner::new(
        config.cloudflare_account_id.clone(),
        config.cloudflare_api_token.clone(),
    );
    let namecheap = NamecheapDnsProvisioner::new(config.namecheap.clone());
    match cloudflare.create_zone(domain).await {
        Ok(zone) => match namecheap
            .set_namecheap_nameservers(domain, &zone.nameservers)
            .await
        {
            Ok(()) => Ok(zone),
            Err(error) => Err(format!(
                "\n<b>DNS nameserver update failed.</b> {}",
                html_escape(&domain_error_for_user(&error))
            )),
        },
        Err(error) => Err(format!(
            "\n<b>Cloudflare zone creation failed.</b> {}",
            html_escape(&domain_error_for_user(&error))
        )),
    }
}

fn website_input_from_draft(draft: &OrderDraft) -> Option<WebsiteInput> {
    Some(WebsiteInput {
        token_ticker: draft.ticker.clone()?,
        token_name: draft.token_name.clone()?,
        contract_address: draft.contract_address.clone()?,
        x_url: draft.x_link.clone()?,
        telegram_url: draft.telegram_link.clone()?,
        logo_asset_path: draft.logo_file_id.clone()?,
        requested_domain: draft.website_url.clone(),
    })
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

fn main_menu_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            "🚀 Create Website",
            "create",
        )],
        vec![InlineKeyboardButton::callback("📊 Order Status", "status")],
        vec![InlineKeyboardButton::callback(
            "👤 Contact Admin",
            "contact_admin",
        )],
    ])
}

fn create_mode_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            "📦 Template Mode | 30-60 USDT",
            "template_mode",
        )],
        vec![InlineKeyboardButton::callback(
            "🤖 AI Free Creation | 50 USDT",
            "ai_mode",
        )],
        vec![InlineKeyboardButton::callback("⬅ Back", "main")],
    ])
}

fn template_keyboard() -> InlineKeyboardMarkup {
    let mut rows = Vec::new();
    for chunk in TEMPLATE_OPTIONS.chunks(2) {
        let mut row = Vec::new();
        for template in chunk {
            let number = template_number(template.id);
            row.push(InlineKeyboardButton::callback(
                format!("{number:02}. {} · {}U", template.name, template.price_usdt),
                format!("select_tpl:{}", template.id),
            ));
        }
        rows.push(row);
    }
    rows.push(vec![InlineKeyboardButton::callback("⬅ Back", "create")]);
    InlineKeyboardMarkup::new(rows)
}

fn template_confirmation_keyboard(template_id: &str) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            "✅ Confirm Template",
            format!("confirm_tpl:{template_id}"),
        )],
        vec![InlineKeyboardButton::callback(
            "📦 Choose Another",
            "template_mode",
        )],
        vec![InlineKeyboardButton::callback("⬅ Back", "create")],
    ])
}

fn payment_chain_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback("💳 USDT ERC20", "pay_erc20")],
        vec![InlineKeyboardButton::callback("💳 USDT TRON", "pay_tron")],
        vec![InlineKeyboardButton::callback("💳 USDT SOL", "pay_sol")],
        vec![InlineKeyboardButton::callback("⬅ Back", "pay_menu")],
    ])
}

fn status_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback("📊 Check Status", "status")],
        vec![InlineKeyboardButton::callback(
            "👤 Contact Admin",
            "contact_admin",
        )],
    ])
}

fn cancel_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "🛑 Cancel Order",
        "cancel",
    )]])
}

fn build_mode_label(build_mode: &BuildModeDraft) -> &'static str {
    match build_mode {
        BuildModeDraft::Template => "Template Mode",
        BuildModeDraft::AiFree => "AI Free Creation",
    }
}

pub fn payment_instruction(
    order_id: Uuid,
    amount_usdt: Decimal,
    chain: PaymentChain,
    address: &str,
) -> String {
    format!(
        "💳 <b>Payment details</b>\n\nOrder: <code>{order_id}</code>\nAmount: <b>{amount_usdt}</b> USDT\nChain: <b>{}</b>\nAddress:\n<code>{}</code>\n\nSend your TX hash in the next message. I will verify amount, recipient, token, confirmations, and duplicate use.",
        chain.as_str(),
        html_escape(address)
    )
}

fn parse_template_deep_link(text: &str) -> Option<&str> {
    text.trim()
        .strip_prefix("/start tpl_")
        .or_else(|| text.trim().strip_prefix("/start=tpl_"))
}

fn normalize_domain(text: &str) -> String {
    text.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.")
        .trim_matches('/')
        .to_ascii_lowercase()
}

fn looks_like_tx_hash(text: &str) -> bool {
    let value = text.trim();
    value.len() >= 16 && value.chars().all(|ch| ch.is_ascii_alphanumeric())
}

fn configured_proxy() -> Option<String> {
    std::env::var("TELOXIDE_PROXY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn find_template(template_id: &str) -> Option<TemplateOption> {
    TEMPLATE_OPTIONS
        .iter()
        .copied()
        .find(|template| template.id == template_id)
}

fn template_number(template_id: &str) -> usize {
    TEMPLATE_OPTIONS
        .iter()
        .position(|template| template.id == template_id)
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn payment_address_for_chain(config: &TelegramUiConfig, chain: PaymentChain) -> &str {
    match chain {
        PaymentChain::Erc20 => &config.usdt_erc20_address,
        PaymentChain::Tron => &config.usdt_tron_address,
        PaymentChain::Sol => &config.usdt_sol_address,
    }
}

fn payment_error_for_user(error: &PaymentVerificationError) -> String {
    match error {
        PaymentVerificationError::NotFound => "Transaction was not found.".to_string(),
        PaymentVerificationError::WrongChain => "Transaction is on the wrong chain.".to_string(),
        PaymentVerificationError::WrongToken => {
            "Transaction is not a supported USDT transfer.".to_string()
        }
        PaymentVerificationError::WrongRecipient => {
            "Transaction was not sent to the configured payment address.".to_string()
        }
        PaymentVerificationError::InsufficientAmount => {
            "Transaction amount is below the required total.".to_string()
        }
        PaymentVerificationError::InsufficientConfirmations => {
            "Transaction does not have enough confirmations yet.".to_string()
        }
        PaymentVerificationError::AlreadyUsed => "Transaction has already been used.".to_string(),
        PaymentVerificationError::ExpiredOrder => {
            "Payment window expired. Start a new order for a fresh quote.".to_string()
        }
        PaymentVerificationError::OldTransaction => {
            "Transaction is older than this order.".to_string()
        }
        PaymentVerificationError::TransactionAfterSubmission => {
            "Transaction timestamp is later than when you submitted the TX hash. Wait until the transaction is confirmed, then submit the TX hash again.".to_string()
        }
        PaymentVerificationError::Provider(value) => {
            format!("Payment provider error after repeated checks: {value}. Contact admin.")
        }
    }
}

fn domain_error_for_user(error: &DomainError) -> String {
    match error {
        DomainError::MissingConfig(value) => {
            format!("Namecheap configuration is missing: {value}")
        }
        DomainError::UnsupportedTld => {
            "Unsupported TLD. Supported: .com, .net, .org, .xyz, .fun, .site, .online, .io, .co, .me, .cc, .app, .dev, .world, .space, .club, .store, .shop".to_string()
        }
        DomainError::Unavailable => "Domain is unavailable.".to_string(),
        DomainError::PremiumRejected => "Premium/EAP domains require manual handling.".to_string(),
        DomainError::Namecheap(value) => format!("Namecheap returned an error: {value}"),
        DomainError::Http(value) => format!("Namecheap HTTP request failed: {value}"),
        DomainError::Parse(value) => format!("Could not parse Namecheap response: {value}"),
        DomainError::Cloudflare(value) => format!("Cloudflare DNS failed: {value}"),
    }
}
