use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use rust_decimal::Decimal;
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
use tracing::info;
use uuid::Uuid;

use crate::{
    config::{Config, GenerationProviderName, NamecheapConfig},
    deployment::{CloudflarePagesDeployer, DeploymentRequest, SiteDeployer},
    domain::{
        DomainError, DomainPurchaseRequest, DomainQuoteRequest, DomainRegistrar, NamecheapRegistrar,
    },
    generation::{CliGenerationProvider, GenerationRouter, SiteGenerationRequest},
    models::{PaymentChain, WebsiteInput},
    validation::{
        validate_contract_address, validate_domain, validate_telegram_url, validate_x_url,
    },
};

type Sessions = Arc<Mutex<HashMap<UserId, OrderDraft>>>;

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
    pub frontend_run_agent_after_tx: bool,
    pub frontend_deploy_after_agent: bool,
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
    pub template_catalog_url: String,
    pub admin_contact: String,
    pub usdt_erc20_address: String,
    pub usdt_tron_address: String,
    pub usdt_sol_address: String,
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
            frontend_run_agent_after_tx: config.frontend_run_agent_after_tx,
            frontend_deploy_after_agent: config.frontend_deploy_after_agent,
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
            template_catalog_url: config.template_catalog_url.clone(),
            admin_contact: config.admin_contact.clone(),
            usdt_erc20_address: config.usdt_erc20_address.clone(),
            usdt_tron_address: config.usdt_tron_address.clone(),
            usdt_sol_address: config.usdt_sol_address.clone(),
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
    domain_price_usdt: Option<Decimal>,
    total_usdt: Option<Decimal>,
    payment_chain: Option<PaymentChain>,
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
            domain_price_usdt: None,
            total_usdt: None,
            payment_chain: None,
        }
    }
}

pub async fn run_frontend(config: Config) -> anyhow::Result<()> {
    if config.telegram_bot_token.trim().is_empty() {
        anyhow::bail!("TELEGRAM_BOT_TOKEN is required to run the Telegram frontend");
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
        .dependencies(dptree::deps![ui_config, sessions])
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
            "/status" => return show_status(&bot, msg.chat.id, user_id, &sessions).await,
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

    handle_order_step(bot, msg, user_id, config, sessions).await
}

async fn handle_callback(
    bot: Bot,
    query: CallbackQuery,
    config: Arc<TelegramUiConfig>,
    sessions: Sessions,
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
        Some("status") => edit_status(&bot, target, query.from.id, &sessions).await,
        Some("contact_admin") => edit_contact_admin(&bot, target, &config).await,
        Some("pay_erc20") => {
            edit_payment_chain_and_ask_tx(
                &bot,
                target,
                query.from.id,
                PaymentChain::Erc20,
                &config.usdt_erc20_address,
                &sessions,
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
                namecheap: config.namecheap.clone(),
                frontend_run_agent_after_tx: config.frontend_run_agent_after_tx,
                frontend_deploy_after_agent: config.frontend_deploy_after_agent,
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
                    .send_message(msg.chat.id, "<b>Invalid or unsupported domain.</b>\n\nSupported TLDs:\n<code>.com</code> <code>.xyz</code> <code>.fun</code> <code>.site</code> <code>.online</code>")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }

            let registrar = NamecheapRegistrar::new(config.namecheap.clone());
            match registrar
                .quote(&DomainQuoteRequest {
                    domain: domain.clone(),
                })
                .await
            {
                Ok(quote) if quote.available && !quote.is_premium => {
                    draft.website_url = Some(domain);
                    draft.domain_price_usdt = Some(quote.price_usd);
                    let total = draft.site_price_usdt + quote.price_usd;
                    draft.total_usdt = Some(total);
                    draft.step = DraftStep::ReadyToPay;
                    set_session(user_id, draft.clone(), &sessions);
                    bot.send_message(
                        msg.chat.id,
                        format!(
                            "✅ <b>Domain available.</b>\n\nDomain price: <b>{}</b> USDT\nWebsite price: <b>{}</b> USDT\n\nTotal: <b>{}</b> USDT\n\nChoose payment chain, then send TX hash.\nPayment check is bypassed for this test build.",
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
                            "<b>Domain check failed.</b>\n\n{}\n\nPlease send another domain.",
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
            let purchase_message = maybe_purchase_domain_after_tx(&config.namecheap, &draft).await;
            let generation_message = maybe_run_agent_after_tx(&config, &draft).await;
            draft.step = DraftStep::PaymentConfirmed;
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                format!(
                    "✅ <b>TX received.</b>\n\nPayment check is bypassed in this test build.\n\n<b>Payment confirmed.</b>\nYour website is queued for generation.{}{}",
                    purchase_message, generation_message
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

fn status_text(user_id: UserId, sessions: &Sessions) -> String {
    match get_session(user_id, sessions) {
        Some(draft) => format!(
            "📊 <b>Current Order</b>\n\nStatus: <code>{:?}</code>\nMode: <b>{}</b>\nWebsite price: <b>{}</b> USDT",
            draft.step,
            build_mode_label(&draft.build_mode),
            draft.site_price_usdt
        ),
        None => "📊 <b>Order Status</b>\n\nNo active frontend test order.".to_string(),
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

async fn show_template_mode(bot: &Bot, chat_id: ChatId) -> ResponseResult<()> {
    send_screen(bot, chat_id, template_mode_text(), template_keyboard()).await
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

async fn ask_logo(bot: &Bot, chat_id: ChatId) -> ResponseResult<()> {
    send_screen(bot, chat_id, ask_logo_text(), cancel_keyboard()).await
}

async fn edit_ask_logo(bot: &Bot, target: UiMessage) -> ResponseResult<()> {
    edit_screen(bot, target, ask_logo_text(), cancel_keyboard()).await
}

async fn show_status(
    bot: &Bot,
    chat_id: ChatId,
    user_id: UserId,
    sessions: &Sessions,
) -> ResponseResult<()> {
    send_screen(
        bot,
        chat_id,
        status_text(user_id, sessions),
        main_menu_keyboard(),
    )
    .await
}

async fn edit_status(
    bot: &Bot,
    target: UiMessage,
    user_id: UserId,
    sessions: &Sessions,
) -> ResponseResult<()> {
    edit_screen(
        bot,
        target,
        status_text(user_id, sessions),
        main_menu_keyboard(),
    )
    .await
}

async fn show_payment_chain_menu(bot: &Bot, chat_id: ChatId) -> ResponseResult<()> {
    send_screen(bot, chat_id, payment_chain_text(), payment_chain_keyboard()).await
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

    draft.payment_chain = Some(chain);
    draft.step = DraftStep::WaitingTx;
    set_session(user_id, draft, sessions);

    let display_address = if address.trim().is_empty() {
        "configured address missing in .env"
    } else {
        address
    };

    edit_screen(
        bot,
        target,
        payment_instruction(Uuid::new_v4(), total, chain, display_address),
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

async fn maybe_purchase_domain_after_tx(config: &NamecheapConfig, draft: &OrderDraft) -> String {
    if !config.auto_purchase_after_tx {
        return "\n\n<b>Domain purchase:</b> skipped. Set <code>NAMECHEAP_AUTO_PURCHASE_AFTER_TX=true</code> to enable automatic Namecheap purchase after TX.".to_string();
    }

    let Some(domain) = draft.website_url.as_deref() else {
        return "\n\n<b>Domain purchase:</b> skipped because no domain is attached.".to_string();
    };

    let registrar = NamecheapRegistrar::new(config.clone());
    match registrar
        .purchase(&DomainPurchaseRequest {
            domain: domain.to_string(),
            years: 1,
            privacy_enabled: true,
        })
        .await
    {
        Ok(result) => format!(
            "\n\n<b>Domain purchased.</b>\nOrder ID: <code>{}</code>\nTransaction ID: <code>{}</code>",
            html_escape(&result.namecheap_order_id),
            html_escape(&result.namecheap_transaction_id)
        ),
        Err(error) => format!(
            "\n\n<b>Domain purchase failed.</b>\n{}",
            html_escape(&domain_error_for_user(&error))
        ),
    }
}

async fn maybe_run_agent_after_tx(config: &TelegramUiConfig, draft: &OrderDraft) -> String {
    if !config.frontend_run_agent_after_tx {
        return "\n\n<b>Agent generation:</b> skipped. Set <code>FRONTEND_RUN_AGENT_AFTER_TX=true</code> to run the agent after TX in frontend test mode.".to_string();
    }

    let Some(input) = website_input_from_draft(draft) else {
        return "\n\n<b>Agent generation failed.</b>\nOrder data is incomplete.".to_string();
    };

    let order_id = Uuid::new_v4();
    let output_dir = std::path::PathBuf::from(&config.generated_sites_dir)
        .join(format!("frontend-test-{order_id}"));
    let router = GenerationRouter::new(
        config.generation_primary_provider,
        Box::new(CliGenerationProvider::gemini(
            config.gemini_agent_command.clone(),
            config.gemini_agent_args.clone(),
            config.agent_timeout_seconds,
        )),
        Box::new(CliGenerationProvider::codex(
            config.codex_agent_command.clone(),
            config.codex_agent_args.clone(),
            config.agent_timeout_seconds,
        )),
    );

    let build_mode = match draft.build_mode {
        BuildModeDraft::Template => "template",
        BuildModeDraft::AiFree => "ai_free",
    };

    let request = SiteGenerationRequest {
        order_id,
        build_mode: build_mode.to_string(),
        template_local_path: draft.template_id.clone(),
        input,
        output_dir,
    };

    match router.generate(&request).await {
        Ok(result) => {
            let deploy_message =
                maybe_deploy_after_agent(config, draft, order_id, &result.output_dir).await;
            format!(
                "\n\n<b>Agent completed.</b>\nProvider: <code>{}</code>\nOutput:\n<code>{}</code>{}",
                html_escape(&result.provider_name),
                html_escape(&result.output_dir.display().to_string()),
                deploy_message
            )
        }
        Err(error) => format!(
            "\n\n<b>Agent generation failed.</b>\n<code>{}</code>",
            html_escape(&error.to_string())
        ),
    }
}

async fn maybe_deploy_after_agent(
    config: &TelegramUiConfig,
    draft: &OrderDraft,
    order_id: Uuid,
    output_dir: &std::path::Path,
) -> String {
    if !config.frontend_deploy_after_agent {
        return "\n\n<b>Cloudflare deploy:</b> skipped. Set <code>FRONTEND_DEPLOY_AFTER_AGENT=true</code> to deploy after agent completion.".to_string();
    }

    let Some(domain) = draft.website_url.as_deref() else {
        return "\n\n<b>Cloudflare deploy failed.</b>\nNo custom domain is attached.".to_string();
    };

    let project_name = cloudflare_project_name(domain, order_id);
    let deployer = CloudflarePagesDeployer::new(
        config.cloudflare_account_id.clone(),
        config.cloudflare_api_token.clone(),
        config.cloudflare_wrangler_command.clone(),
        config.cloudflare_wrangler_args.clone(),
    );
    match deployer
        .deploy(&DeploymentRequest {
            repo_owner: "direct-upload".to_string(),
            repo_name: project_name.clone(),
            github_token: String::new(),
            site_dir: output_dir.display().to_string(),
            cloudflare_project_name: project_name,
            production_branch: "main".to_string(),
            custom_domain: Some(domain.to_string()),
        })
        .await
    {
        Ok(result) => format!(
            "\n\n<b>Cloudflare Pages deployed.</b>\nPages URL:\n<code>{}</code>\nCustom domain:\n<code>{}</code>",
            html_escape(&result.cloudflare_pages_url),
            html_escape(result.custom_domain.as_deref().unwrap_or(domain))
        ),
        Err(error) => format!(
            "\n\n<b>Cloudflare deploy failed.</b>\n<code>{}</code>",
            html_escape(&error.to_string())
        ),
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

fn cloudflare_project_name(domain: &str, order_id: Uuid) -> String {
    let base = domain
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    let suffix = order_id.simple().to_string();
    format!("{}-{}", base, &suffix[..8])
        .chars()
        .take(58)
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
        "💳 <b>Payment details</b>\n\nOrder: <code>{order_id}</code>\nAmount: <b>{amount_usdt}</b> USDT\nChain: <b>{}</b>\nAddress:\n<code>{}</code>\n\nSend your TX hash in the next message.\n\nPayment check is bypassed for this frontend test build.",
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

fn domain_error_for_user(error: &DomainError) -> String {
    match error {
        DomainError::MissingConfig(value) => {
            format!("Namecheap configuration is missing: {value}")
        }
        DomainError::UnsupportedTld => {
            "Unsupported TLD. Supported: .com, .xyz, .fun, .site, .online".to_string()
        }
        DomainError::Unavailable => "Domain is unavailable.".to_string(),
        DomainError::PremiumRejected => "Premium/EAP domains require manual handling.".to_string(),
        DomainError::Namecheap(value) => format!("Namecheap returned an error: {value}"),
        DomainError::Http(value) => format!("Namecheap HTTP request failed: {value}"),
        DomainError::Parse(value) => format!("Could not parse Namecheap response: {value}"),
        DomainError::Cloudflare(value) => format!("Cloudflare DNS failed: {value}"),
    }
}
