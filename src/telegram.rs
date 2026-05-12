use std::{
    collections::HashMap,
    path::Path,
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
        DomainError, DomainPurchaseRequest, DomainQuoteRequest, DomainRegistrar,
        NamecheapDnsProvisioner, NamecheapRegistrar,
    },
    api_client::{
        HttpClientDnsProvisioner, HttpClientDomainRegistrar,
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
const DRAFT_SESSION_TIMEOUT_MINUTES: i64 = db::ASSET_COLLECTION_TIMEOUT_MINUTES;

#[derive(Clone, Debug)]
pub struct TemplateOption {
    id: String,
    name: String,
    preview_url: String,
}

#[derive(Clone, Copy, Debug)]
struct UiMessage {
    chat_id: ChatId,
    message_id: MessageId,
}

impl TemplateOption {
    fn new(template_number: usize, preview_domain: &str, preview_base_url: &str) -> Self {
        Self {
            id: format!("template_{template_number:02}"),
            name: format!("{template_number:02}.{preview_domain}"),
            preview_url: format!("{preview_base_url}/{template_number:02}"),
        }
    }
}

fn build_template_options(preview_domain: &str, preview_base_url: &str) -> Vec<TemplateOption> {
    (1..=12)
        .map(|n| TemplateOption::new(n, preview_domain, preview_base_url))
        .collect()
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
    alternative_domains: Option<Vec<crate::domain::DomainQuote>>,
    total_usdt: Option<Decimal>,
    payment_chain: Option<PaymentChain>,
    order_id: Option<Uuid>,
    db_user_id: Option<Uuid>,
    updated_at: chrono::DateTime<chrono::Utc>,
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

impl DraftStep {
    fn as_str(&self) -> &'static str {
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
    fn new_ai(price_usdt: Decimal) -> Self {
        Self::new(BuildModeDraft::AiFree, price_usdt, None)
    }

    fn new_template(template: TemplateOption, price_usdt: Decimal) -> Self {
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
            domain_quote_id: None,
            domain_price_usdt: None,
            alternative_domains: None,
            total_usdt: None,
            payment_chain: None,
            order_id: None,
            db_user_id: None,
            updated_at: chrono::Utc::now(),
        }
    }

    fn touch(&mut self) {
        self.updated_at = chrono::Utc::now();
    }

    fn is_expired(&self) -> bool {
        chrono::Utc::now() - self.updated_at
            > chrono::Duration::minutes(DRAFT_SESSION_TIMEOUT_MINUTES)
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
        info!(
            chat_id = %msg.chat.id,
            message_kind = message_kind(&msg),
            "telegram message without user"
        );
        return show_main_menu(&bot, msg.chat.id, &config).await;
    };
    let user_id = user.id;
    let current_step = session_step_name(user_id, &sessions).await;
    info!(
        user_id = user_id.0 as i64,
        chat_id = %msg.chat.id,
        message_kind = message_kind(&msg),
        session_step = current_step.unwrap_or("none"),
        "telegram message received"
    );
    expire_stale_orders_for_user(&pool, user_id).await;

    if let Some(text) = msg.text() {
        if let Some(template_id) = parse_template_deep_link(text) {
            return show_template_confirmation(&bot, msg.chat.id, template_id, &config).await;
        }

        match text.trim() {
            "/start" | "/help" => return show_main_menu(&bot, msg.chat.id, &config).await,
            "/create" => {
                return show_create_menu(
                    &bot,
                    msg.chat.id,
                    &config.template_prices_usdt,
                    config.ai_creation_price_usdt,
                )
                .await;
            }
            "/status" => return show_status(&bot, msg.chat.id, user_id, &sessions, &pool).await,
            "/history" => return show_history(&bot, msg.chat.id, user_id, &pool).await,
            "/continue" => {
                return handle_continue_command(
                    &bot,
                    msg.chat.id,
                    user_id,
                    &sessions,
                    &pool,
                    &config,
                )
                .await;
            }
            "/cancel" => {
                cancel_user_order(&pool, user_id, &sessions).await;
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
    let current_step = session_step_name(query.from.id, &sessions).await;
    info!(
        user_id = query.from.id.0 as i64,
        chat_id = %chat_id,
        callback = query.data.as_deref().unwrap_or("<none>"),
        session_step = current_step.unwrap_or("none"),
        "telegram callback received"
    );

    if !ensure_group_member_for_user(&bot, chat_id, query.from.id, &config).await? {
        return Ok(());
    }
    expire_stale_orders_for_user(&pool, query.from.id).await;

    match query.data.as_deref() {
        Some("main") => edit_main_menu(&bot, target, &config).await,
        Some("create") => {
            edit_create_menu(
                &bot,
                target,
                &config.template_prices_usdt,
                config.ai_creation_price_usdt,
            )
            .await
        }
        Some("template_mode") => edit_template_mode(&bot, target, &config).await,
        Some("ai_mode") => {
            clear_screen_keyboard(&bot, target).await?;
            let draft = match start_new_order_session(
                &pool,
                query.from.id,
                query.from.username.as_deref(),
                Some(query.from.first_name.as_str()),
                query.from.last_name.as_deref(),
                OrderDraft::new_ai(config.ai_creation_price_usdt),
            )
            .await
            {
                Ok(draft) => draft,
                Err(error) => {
                    let msg = if format!("{error:#}").contains("pending order") {
                        warn!(
                            ?error,
                            user_id = query.from.id.0 as i64,
                            "blocked ai order: pending order exists"
                        );
                        "⚠️ <b>Unfinished order.</b>\nUse /continue or /cancel first."
                    } else {
                        warn!(
                            ?error,
                            user_id = query.from.id.0 as i64,
                            "failed to start ai order session"
                        );
                        "<b>Start failed.</b>\nPlease try again."
                    };
                    return edit_screen(&bot, target, msg, main_menu_keyboard()).await;
                }
            };
            set_session(query.from.id, draft, &sessions);
            info!(
                user_id = query.from.id.0 as i64,
                build_mode = "ai_free",
                next_step = DraftStep::WaitingLogo.as_str(),
                "new order session started"
            );
            edit_ask_logo(&bot, target).await
        }
        Some("pay_menu") => edit_payment_chain_menu(&bot, target).await,
        Some("status") => edit_status(&bot, target, query.from.id, &sessions, &pool).await,
        Some("history") => edit_history(&bot, target, query.from.id, &pool).await,
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
            edit_template_confirmation(&bot, target, template_id, &config).await
        }
        Some(data) if data.starts_with("confirm_tpl:") => {
            match find_template(
                &config.template_options,
                data.trim_start_matches("confirm_tpl:"),
            ) {
                Some(template) => {
                    clear_screen_keyboard(&bot, target).await?;
                    let draft = match start_new_order_session(
                        &pool,
                        query.from.id,
                        query.from.username.as_deref(),
                        Some(query.from.first_name.as_str()),
                        query.from.last_name.as_deref(),
                        OrderDraft::new_template(
                            template.clone(),
                            template_price(
                                &config.template_options,
                                &template.id,
                                &config.template_prices_usdt,
                            ),
                        ),
                    )
                    .await
                    {
                        Ok(draft) => draft,
                        Err(error) => {
                            let msg = if format!("{error:#}").contains("pending order") {
                                warn!(
                                    ?error,
                                    user_id = query.from.id.0 as i64,
                                    template_id = template.id,
                                    "blocked template order: pending order exists"
                                );
                                "⚠️ <b>Unfinished order.</b>\nUse /continue or /cancel first."
                            } else {
                                warn!(
                                    ?error,
                                    user_id = query.from.id.0 as i64,
                                    template_id = template.id,
                                    "failed to start template order session"
                                );
                                "<b>Start failed.</b>\nPlease try again."
                            };
                            return edit_screen(&bot, target, msg, main_menu_keyboard()).await;
                        }
                    };
                    set_session(query.from.id, draft, &sessions);
                    info!(
                        user_id = query.from.id.0 as i64,
                        build_mode = "template",
                        template_id = template.id,
                        next_step = DraftStep::WaitingLogo.as_str(),
                        "new order session started"
                    );
                    edit_ask_logo(&bot, target).await
                }
                None => {
                    edit_screen(
                        &bot,
                        target,
                        "<b>Template not found.</b>\nChoose again.",
                        template_keyboard(&config.template_options, &config.template_prices_usdt),
                    )
                    .await
                }
            }
        }
        Some(data) if data.starts_with("alt_domain:") => {
            let domain = data.trim_start_matches("alt_domain:");
            handle_alternative_domain_selection(
                &bot,
                target,
                query.from.id,
                domain,
                &config,
                &sessions,
                &pool,
            )
            .await
        }
        Some("alt_domain_custom") => {
            edit_screen(
                &bot,
                target,
                "✏️ <b>Domain</b>\nExample: <code>pepe.xyz</code>",
                cancel_keyboard(),
            )
            .await
        }
        Some("cancel") => {
            cancel_user_order(&pool, query.from.id, &sessions).await;
            edit_screen(
                &bot,
                target,
                "🛑 <b>Order cancelled</b>",
                main_menu_keyboard(),
            )
            .await
        }
        _ => edit_main_menu(&bot, target, &config).await,
    }
}

async fn start_new_order_session(
    pool: &SqlitePool,
    telegram_user_id: UserId,
    username: Option<&str>,
    first_name: Option<&str>,
    last_name: Option<&str>,
    mut draft: OrderDraft,
) -> anyhow::Result<OrderDraft> {
    let db_user_id = db::upsert_user(
        pool,
        telegram_user_id.0 as i64,
        username,
        first_name,
        last_name,
        true,
    )
    .await?;

    if db::has_pending_order(pool, telegram_user_id.0 as i64).await? {
        anyhow::bail!("has pending order");
    }

    let build_mode = match draft.build_mode {
        BuildModeDraft::Template => crate::models::BuildMode::Template,
        BuildModeDraft::AiFree => crate::models::BuildMode::AiFree,
    };
    let order_id = db::create_site_order(
        pool,
        db_user_id,
        build_mode,
        draft.template_id.as_deref(),
        draft.site_price_usdt,
        None,
    )
    .await?;
    info!(
        user_id = telegram_user_id.0 as i64,
        order_id = %order_id,
        site_price_usdt = %draft.site_price_usdt,
        "draft order persisted before asset collection"
    );
    draft.db_user_id = Some(db_user_id);
    draft.order_id = Some(order_id);
    Ok(draft)
}

async fn expire_stale_orders_for_user(pool: &SqlitePool, user_id: UserId) {
    match db::expire_stale_pending_orders_for_user(pool, user_id.0 as i64).await {
        Ok(expired) if expired > 0 => {
            info!(
                user_id = user_id.0 as i64,
                expired_orders = expired,
                "expired stale pending orders for user"
            );
        }
        Ok(_) => {}
        Err(error) => warn!(
            ?error,
            user_id = user_id.0 as i64,
            "failed to expire stale pending orders"
        ),
    }
}

async fn cancel_user_order(pool: &SqlitePool, user_id: UserId, sessions: &Sessions) {
    let order_id = get_session(user_id, sessions).and_then(|draft| draft.order_id);
    remove_session(user_id, sessions);
    match db::cancel_order_for_user(pool, user_id.0 as i64, order_id).await {
        Ok(cancelled) => {
            info!(
                user_id = user_id.0 as i64,
                order_id = %order_id.map(|id| id.to_string()).unwrap_or_else(|| "<latest>".to_string()),
                cancelled_orders = cancelled,
                "cancelled pending order"
            );
        }
        Err(error) => warn!(
            ?error,
            user_id = user_id.0 as i64,
            order_id = %order_id.map(|id| id.to_string()).unwrap_or_else(|| "<latest>".to_string()),
            "failed to cancel pending order"
        ),
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
        if let Some(text) = msg.text().map(str::trim)
            && looks_like_tx_hash(text)
        {
            match db::get_latest_pending_payment_context_for_user(pool, user_id.0 as i64).await {
                Ok(Some(context)) => {
                    return process_payment_tx_context(PaymentTxProcess {
                        bot: &bot,
                        chat_id: msg.chat.id,
                        user_id,
                        config: &config,
                        sessions: &sessions,
                        pool,
                        context,
                        tx_hash: text,
                    })
                    .await;
                }
                Ok(None) => {}
                Err(error) => {
                    warn!(
                        ?error,
                        user_id = user_id.0 as i64,
                        "failed to recover pending payment context"
                    );
                    return bot
                        .send_message(
                            msg.chat.id,
                            "⚠️ <b>Payment check failed.</b>\nContact admin.",
                        )
                        .parse_mode(ParseMode::Html)
                        .await
                        .map(|_| ());
                }
            }
        }
        return show_main_menu(&bot, msg.chat.id, &config).await;
    };
    if draft.is_expired() {
        warn!(
            user_id = user_id.0 as i64,
            order_id = %draft.order_id.map(|id| id.to_string()).unwrap_or_else(|| "<none>".to_string()),
            last_step = draft.step.as_str(),
            "order session expired while collecting input"
        );
        cancel_user_order(pool, user_id, &sessions).await;
        return bot
            .send_message(
                msg.chat.id,
                "<b>Order expired.</b>\nStart again with /create.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(main_menu_keyboard())
            .await
            .map(|_| ());
    }

    match draft.step {
        DraftStep::WaitingLogo => {
            let Some(photo) = msg.photo().and_then(|photos| photos.last()) else {
                return bot
                    .send_message(msg.chat.id, "🖼 <b>1/7 Logo</b>\nUpload token logo.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            draft.logo_file_id = Some(photo.file.id.0.clone());
            draft.step = DraftStep::WaitingTicker;
            info!(
                user_id = user_id.0 as i64,
                chat_id = %msg.chat.id,
                next_step = DraftStep::WaitingTicker.as_str(),
                "logo received; advancing order wizard"
            );
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                "✅ Logo saved.\n🔤 <b>2/7 Ticker</b>\nExample: <code>PEPE</code>",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        DraftStep::WaitingTicker => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "🔤 <b>2/7 Ticker</b>\nSend ticker.")
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
                        "<b>Invalid ticker.</b>\nUse 2-12 letters/numbers.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            draft.ticker = Some(ticker);
            draft.step = DraftStep::WaitingTokenName;
            info!(
                user_id = user_id.0 as i64,
                chat_id = %msg.chat.id,
                next_step = DraftStep::WaitingTokenName.as_str(),
                "ticker accepted; advancing order wizard"
            );
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                "✅ Ticker saved.\n🏷 <b>3/7 Name</b>\nExample: <code>Pepe Coin</code>",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        DraftStep::WaitingTokenName => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "🏷 <b>3/7 Name</b>\nSend token name.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            if text.len() < 2 || text.len() > 48 {
                return bot
                    .send_message(msg.chat.id, "<b>Invalid name.</b>\nUse 2-48 characters.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            draft.token_name = Some(text.to_string());
            draft.step = DraftStep::WaitingContractAddress;
            info!(
                user_id = user_id.0 as i64,
                chat_id = %msg.chat.id,
                next_step = DraftStep::WaitingContractAddress.as_str(),
                "token name accepted; advancing order wizard"
            );
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                "✅ Name saved.\n🔗 <b>4/7 Contract</b>\nSend contract address.",
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
                        "🔗 <b>4/7 Contract</b>\nSend contract address.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            if validate_contract_address(text).is_err() {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid contract.</b>\nSend EVM, TRON, or SOL address.",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            draft.contract_address = Some(text.to_string());
            draft.step = DraftStep::WaitingXLink;
            info!(
                user_id = user_id.0 as i64,
                chat_id = %msg.chat.id,
                next_step = DraftStep::WaitingXLink.as_str(),
                "contract address accepted; advancing order wizard"
            );
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                "✅ Contract saved.\n✕ <b>5/7 X</b>\n<code>https://x.com/yourproject</code>",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        DraftStep::WaitingXLink => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "✕ <b>5/7 X</b>\nSend X link.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            if validate_x_url(text).is_err() {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid X link.</b>\nUse <code>https://x.com/username</code>",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            draft.x_link = Some(text.to_string());
            draft.step = DraftStep::WaitingTelegramLink;
            info!(
                user_id = user_id.0 as i64,
                chat_id = %msg.chat.id,
                next_step = DraftStep::WaitingTelegramLink.as_str(),
                "x link accepted; advancing order wizard"
            );
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                "✅ X saved.\n💬 <b>6/7 Telegram</b>\n<code>https://t.me/yourgroup</code>",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        DraftStep::WaitingTelegramLink => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "💬 <b>6/7 Telegram</b>\nSend Telegram link.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            if validate_telegram_url(text).is_err() {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid Telegram link.</b>\nUse <code>https://t.me/yourgroup</code>",
                    )
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }
            draft.telegram_link = Some(text.to_string());
            draft.step = DraftStep::WaitingWebsiteUrl;
            info!(
                user_id = user_id.0 as i64,
                chat_id = %msg.chat.id,
                next_step = DraftStep::WaitingWebsiteUrl.as_str(),
                "telegram link accepted; advancing order wizard"
            );
            set_session(user_id, draft, &sessions);
            bot.send_message(
                msg.chat.id,
                "✅ Telegram saved.\n🌐 <b>7/7 Domain</b>\nExample: <code>pepe.xyz</code>",
            )
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ())
        }
        DraftStep::WaitingWebsiteUrl => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "🌐 <b>7/7 Domain</b>\nSend domain.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            let domain = normalize_domain(text);
            info!(
                user_id = user_id.0 as i64,
                chat_id = %msg.chat.id,
                domain = %domain,
                "checking requested domain"
            );
            if validate_domain(&domain).is_err() {
                return bot
                    .send_message(
                        msg.chat.id,
                        "<b>Invalid domain.</b>\nTry .com, .xyz, .fun, or .site.",
                    )
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
                    info!(
                        user_id = user_id.0 as i64,
                        chat_id = %msg.chat.id,
                        domain = %quote.domain,
                        price_usd = %quote.price_usd,
                        "domain quote available"
                    );
                    let db_user_id =
                        match db::upsert_user(pool, user_id.0 as i64, None, None, None, true).await
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
                                        "<b>Quote failed.</b>\nPlease try again.",
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
                        Ok(quote_id) => {
                            info!(
                                user_id = user_id.0 as i64,
                                quote_id = %quote_id,
                                domain = %quote.domain,
                                "domain quote persisted"
                            );
                            if let Some(order_id) = draft.order_id
                                && let Err(error) = db::prepare_order_for_payment(
                                    pool,
                                    order_id,
                                    total,
                                    Some(quote_id),
                                )
                                .await
                            {
                                warn!(
                                    ?error,
                                    user_id = user_id.0 as i64,
                                    order_id = %order_id,
                                    quote_id = %quote_id,
                                    "failed to attach domain quote to pending order"
                                );
                                return bot
                                    .send_message(
                                        msg.chat.id,
                                        "<b>Quote save failed.</b>\nPlease try again.",
                                    )
                                    .parse_mode(ParseMode::Html)
                                    .await
                                    .map(|_| ());
                            }
                            draft.domain_quote_id = Some(quote_id)
                        }
                        Err(e) => {
                            warn!(?e, "failed to record domain quote");
                            return bot
                                .send_message(
                                    msg.chat.id,
                                    "<b>Quote failed.</b>\nPlease try again.",
                                )
                                .parse_mode(ParseMode::Html)
                                .await
                                .map(|_| ());
                        }
                    }
                    draft.total_usdt = Some(total);
                    draft.step = DraftStep::ReadyToPay;
                    info!(
                        user_id = user_id.0 as i64,
                        chat_id = %msg.chat.id,
                        domain = %quote.domain,
                        total_usdt = %total,
                        next_step = DraftStep::ReadyToPay.as_str(),
                        "order ready for payment chain selection"
                    );
                    set_session(user_id, draft.clone(), &sessions);
                    bot.send_message(
                        msg.chat.id,
                        format!(
                            "✅ <b>Domain available</b>\nDomain: <b>{}</b> USDT\nWebsite: <b>{}</b> USDT\nTotal: <b>{}</b> USDT\n\nChoose payment chain.",
                            quote.price_usd, draft.site_price_usdt, total
                        ),
                    )
                    .parse_mode(ParseMode::Html)
                    .reply_markup(payment_chain_keyboard())
                    .await
                    .map(|_| ())
                }
                Ok(_) | Err(_) => {
                    info!(
                        user_id = user_id.0 as i64,
                        chat_id = %msg.chat.id,
                        domain = %domain,
                        "requested domain unavailable; checking alternatives"
                    );
                    let tld = domain.rsplit_once('.').map(|(_, t)| t).unwrap_or("");
                    let base_name = domain
                        .rsplit_once('.')
                        .map(|(name, _)| name)
                        .unwrap_or(&domain);
                    let alternatives =
                        check_alternative_domains(registrar.as_ref(), base_name, tld).await;

                    if alternatives.is_empty() {
                        bot.send_message(
                            msg.chat.id,
                            "<b>Domain unavailable.</b>\nSend another domain.",
                        )
                        .parse_mode(ParseMode::Html)
                        .await
                        .map(|_| ())
                    } else {
                        draft.alternative_domains = Some(alternatives.clone());
                        set_session(user_id, draft.clone(), &sessions);
                        bot.send_message(
                            msg.chat.id,
                            format!("🔍 <b>{} is taken.</b>\nChoose one:", html_escape(&domain),),
                        )
                        .parse_mode(ParseMode::Html)
                        .reply_markup(alternative_domains_keyboard(&alternatives))
                        .await
                        .map(|_| ())
                    }
                }
            }
        }
        DraftStep::ReadyToPay => bot
            .send_message(msg.chat.id, "💳 <b>Payment</b>\nChoose a chain.")
            .parse_mode(ParseMode::Html)
            .reply_markup(payment_chain_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingTx => {
            let Some(text) = msg.text().map(str::trim) else {
                return bot
                    .send_message(msg.chat.id, "💳 <b>Payment</b>\nPaste TX hash.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };
            if !looks_like_tx_hash(text) {
                return bot
                    .send_message(msg.chat.id, "<b>Invalid TX.</b>\nPaste transaction hash.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            }

            let Some(order_id) = draft.order_id else {
                return bot
                    .send_message(msg.chat.id, "⚠️ <b>No order found.</b>\nUse /create.")
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ());
            };

            let context = match db::get_payment_context_by_order(pool, order_id).await {
                Ok(context) => context,
                Err(error) => {
                    warn!(?error, order_id = %order_id, "failed to load payment context");
                    return bot
                        .send_message(
                            msg.chat.id,
                            "⚠️ <b>Payment check failed.</b>\nContact admin.",
                        )
                        .parse_mode(ParseMode::Html)
                        .await
                        .map(|_| ());
                }
            };

            process_payment_tx_context(PaymentTxProcess {
                bot: &bot,
                chat_id: msg.chat.id,
                user_id,
                config: &config,
                sessions: &sessions,
                pool,
                context,
                tx_hash: text,
            })
            .await
        }
        DraftStep::PaymentConfirmed => bot
            .send_message(msg.chat.id, "<b>Payment confirmed.</b>\nWebsite is queued.")
            .parse_mode(ParseMode::Html)
            .reply_markup(status_keyboard())
            .await
            .map(|_| ()),
    }
}

struct PaymentTxProcess<'a> {
    bot: &'a Bot,
    chat_id: ChatId,
    user_id: UserId,
    config: &'a TelegramUiConfig,
    sessions: &'a Sessions,
    pool: &'a SqlitePool,
    context: db::PaymentOrderContext,
    tx_hash: &'a str,
}

async fn process_payment_tx_context(request: PaymentTxProcess<'_>) -> ResponseResult<()> {
    let PaymentTxProcess {
        bot,
        chat_id,
        user_id,
        config,
        sessions,
        pool,
        context,
        tx_hash,
    } = request;
    let order_id = context.order_id;
    let chain = context.payment_chain;
    let mut status_parts: Vec<String> = vec![];
    info!(
        user_id = user_id.0 as i64,
        order_id = %order_id,
        chain = chain.as_str(),
        required_amount_usdt = %context.amount_usdt,
        "payment tx submitted; starting verification"
    );

    if context.payment_status == "paid" {
        info!(
            user_id = user_id.0 as i64,
            order_id = %order_id,
            "payment tx ignored because order is already paid"
        );
        return bot
            .send_message(chat_id, "<b>Payment confirmed.</b>\nWebsite is queued.")
            .parse_mode(ParseMode::Html)
            .reply_markup(status_keyboard())
            .await
            .map(|_| ());
    }

    match db::tx_hash_already_used(pool, chain, tx_hash).await {
        Ok(true) => {
            warn!(
                user_id = user_id.0 as i64,
                order_id = %order_id,
                chain = chain.as_str(),
                "reused tx rejected"
            );
            return bot
                .send_message(chat_id, "<b>TX already used.</b>\nSend a new TX.")
                .parse_mode(ParseMode::Html)
                .await
                .map(|_| ());
        }
        Ok(false) => {
            info!(
                user_id = user_id.0 as i64,
                order_id = %order_id,
                chain = chain.as_str(),
                "tx reuse check passed"
            );
        }
        Err(error) => warn!(?error, "failed to check tx reuse"),
    }

    let expected_to_address = payment_address_for_chain(config, chain).trim().to_string();
    if expected_to_address.is_empty() {
        return bot
            .send_message(chat_id, "<b>Payment address missing.</b>\nContact admin.")
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
    let payment_expires_at = context.payment_expires_at.unwrap_or_else(|| {
        warn!(order_id = %order_id, "payment_expires_at is NULL, falling back to created_at + payment window");
        context.created_at + chrono::Duration::minutes(db::PAYMENT_WINDOW_MINUTES)
    });
    let payment_started_at = context.payment_started_at.unwrap_or_else(|| {
        warn!(order_id = %order_id, "payment_started_at is NULL, falling back from payment_expires_at");
        payment_expires_at - chrono::Duration::minutes(db::PAYMENT_WINDOW_MINUTES)
    });
    let verify_request = PaymentVerificationRequest {
        order_id,
        chain,
        tx_hash: tx_hash.to_string(),
        expected_to_address,
        required_amount_usdt: context.amount_usdt,
        payment_started_at,
        submitted_at: tx_submitted_at,
        expires_at: payment_expires_at,
    };
    let verification = verify_payment_with_retries(&service, verify_request).await;
    if let Err(error) = db::record_payment_attempt(
        pool,
        order_id,
        context.user_id,
        chain,
        tx_hash,
        verification.as_ref(),
    )
    .await
    {
        warn!(?error, order_id = %order_id, "failed to record payment attempt");
    }

    let outcome = match verification {
        Ok(outcome) => {
            info!(
                user_id = user_id.0 as i64,
                order_id = %order_id,
                chain = chain.as_str(),
                observed_amount_usdt = %outcome.observed_amount_usdt,
                confirmations = outcome.confirmations.map(|value| value as i64),
                confirmed_at = %outcome.confirmed_at,
                "payment verified on chain"
            );
            outcome
        }
        Err(error) => {
            warn!(
                user_id = user_id.0 as i64,
                order_id = %order_id,
                chain = chain.as_str(),
                error = %error,
                "payment verification failed"
            );
            return bot
                .send_message(
                    chat_id,
                    format!(
                        "❌ <b>Payment not confirmed.</b>\n{}",
                        html_escape(&payment_error_for_user(&error))
                    ),
                )
                .parse_mode(ParseMode::Html)
                .await
                .map(|_| ());
        }
    };

    if let Err(error) =
        db::confirm_payment_and_mark_paid(pool, order_id, chain, tx_hash, &outcome).await
    {
        warn!(?error, order_id = %order_id, "failed to confirm payment transaction");
        return bot
            .send_message(chat_id, "⚠️ <b>Payment record failed.</b>\nContact admin.")
            .parse_mode(ParseMode::Html)
            .await
            .map(|_| ());
    }
    info!(
        user_id = user_id.0 as i64,
        order_id = %order_id,
        chain = chain.as_str(),
        "payment recorded and order marked paid"
    );
    status_parts.push(format!(
        "Paid: {} USDT. Confirmations: {}.",
        outcome.observed_amount_usdt,
        outcome
            .confirmations
            .map(|value| value.to_string())
            .unwrap_or_else(|| "confirmed".to_string())
    ));

    match (context.website_id, context.website_input) {
        (Some(website_id), Some(mut input)) => {
            if !input.logo_asset_path.trim().is_empty()
                && !Path::new(&input.logo_asset_path).exists()
            {
                info!(
                    user_id = user_id.0 as i64,
                    order_id = %order_id,
                    website_id = %website_id,
                    "downloading telegram logo before queueing generation"
                );
                match download_telegram_logo(bot, &input.logo_asset_path, &order_id.to_string())
                    .await
                {
                    Ok(path) => {
                        info!(
                            user_id = user_id.0 as i64,
                            order_id = %order_id,
                            website_id = %website_id,
                            logo_path = %path,
                            "telegram logo downloaded"
                        );
                        if let Err(error) =
                            db::update_website_logo_path(pool, website_id, &path).await
                        {
                            warn!(?error, website_id = %website_id, "failed to persist downloaded logo path");
                        }
                        input.logo_asset_path = path;
                    }
                    Err(error) => {
                        warn!(?error, order_id = %order_id, "failed to download logo; keeping stored logo reference");
                    }
                }
            }

            let (domain_ready, purchase_message) = maybe_purchase_domain_after_tx(
                config,
                pool,
                context.domain_quote_id,
                input.requested_domain.as_deref(),
            )
            .await;
            if !purchase_message.is_empty() {
                status_parts.push(purchase_message.trim().to_string());
            }
            if !domain_ready {
                warn!(
                    user_id = user_id.0 as i64,
                    order_id = %order_id,
                    website_id = %website_id,
                    "domain not ready; order moved toward manual review"
                );
                if let Err(error) = db::mark_order_manual_review(pool, order_id).await {
                    warn!(?error, order_id = %order_id, "failed to mark manual review");
                }
                status_parts.push("Manual review required. Contact admin.".to_string());
            } else if let Err(error) =
                enqueue_generation_job_with_retries(pool, order_id, website_id).await
            {
                warn!(?error, order_id = %order_id, "failed to enqueue generation job after retries");
                let _ = db::mark_order_manual_review(pool, order_id).await;
                status_parts.push("⚠️ Failed to queue generation. Contact admin.".to_string());
            } else {
                info!(
                    user_id = user_id.0 as i64,
                    order_id = %order_id,
                    website_id = %website_id,
                    "generation job queued"
                );
                status_parts.push("Website queued.".to_string());
            }
        }
        _ => {
            warn!(order_id = %order_id, "paid order is missing persisted website input");
            let _ = db::mark_order_manual_review(pool, order_id).await;
            status_parts.push("⚠️ Website input missing. Contact admin.".to_string());
        }
    }

    if let Some(mut draft) = get_session(user_id, sessions) {
        draft.step = DraftStep::PaymentConfirmed;
        set_session(user_id, draft, sessions);
    }

    bot.send_message(
        chat_id,
        format!("✅ <b>Payment confirmed.</b>\n{}", status_parts.join("\n")),
    )
    .parse_mode(ParseMode::Html)
    .reply_markup(status_keyboard())
    .await
    .map(|_| ())
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
                "<b>Access required</b>\nJoin: https://t.me/MeMe_Web_Support",
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
        "🚀 <b>Auto Meme Web Bot</b>\nCreate meme websites.\n\nTemplates: {}",
        html_escape(&config.template_catalog_url)
    )
}

fn create_menu_text() -> &'static str {
    "🧩 <b>Create Website</b>\nChoose a mode."
}

fn template_mode_text() -> &'static str {
    "📦 <b>Template Mode</b>\nChoose a template."
}

fn template_confirmation_text(template: &TemplateOption, price_usdt: Decimal) -> String {
    format!(
        "📦 <b>{}</b>\nPrice: <b>{}</b> USDT\n🔗 <a href=\"{}\">Preview template</a>",
        html_escape(&template.name),
        price_usdt,
        &template.preview_url,
    )
}

fn ask_logo_text() -> &'static str {
    "🖼 <b>1/7 Logo</b>\nUpload token logo."
}

async fn status_text(user_id: UserId, sessions: &Sessions, pool: &SqlitePool) -> String {
    if let Some(draft) = get_session(user_id, sessions) {
        return format!(
            "📊 <b>Current Order</b>\nStatus: <code>{:?}</code>\nMode: <b>{}</b>\nPrice: <b>{}</b> USDT",
            draft.step,
            build_mode_label(&draft.build_mode),
            draft.site_price_usdt
        );
    }
    match db::get_user_order(pool, user_id.0 as i64).await {
        Ok(Some(order)) => format!(
            "📊 <b>Order Status</b>\nPayment: {}\nSite: {}\nAmount: {} USDT\nChain: {}\nDomain: {}",
            order.payment_status,
            order.site_status,
            order.amount_usdt,
            order.payment_chain.as_deref().unwrap_or("—"),
            order.requested_domain.as_deref().unwrap_or("—"),
        ),
        Ok(None) => "📊 <b>Order Status</b>\nNo active order. Use /create.".to_string(),
        Err(_) => "📊 <b>Order Status</b>\nCheck failed. Try later.".to_string(),
    }
}

async fn history_text(user_id: UserId, pool: &SqlitePool) -> String {
    match db::get_user_paid_order_history(pool, user_id.0 as i64, 10).await {
        Ok(items) if items.is_empty() => "🧾 <b>History</b>\nNo paid orders.".to_string(),
        Ok(items) => {
            let mut lines = vec!["🧾 <b>Paid Orders</b>".to_string()];
            for (index, item) in items.iter().enumerate() {
                let domain = item
                    .custom_domain
                    .as_deref()
                    .or(item.requested_domain.as_deref())
                    .or(item.cloudflare_pages_url.as_deref())
                    .unwrap_or("—");
                let chain = item.payment_chain.as_deref().unwrap_or("—");
                let paid_at = item
                    .paid_at
                    .map(|value| value.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "—".to_string());
                lines.push(format!(
                    "{}. {} · {} USDT · {} · {} · {}",
                    index + 1,
                    html_escape(domain),
                    item.amount_usdt,
                    chain,
                    item.site_status,
                    paid_at
                ));
            }
            lines.join("\n")
        }
        Err(error) => {
            warn!(
                ?error,
                user_id = user_id.0 as i64,
                "failed to load paid order history"
            );
            "🧾 <b>History</b>\nCheck failed. Try later.".to_string()
        }
    }
}

fn payment_chain_text() -> &'static str {
    "💳 <b>Payment</b>\nChoose USDT chain."
}

async fn handle_continue_command(
    bot: &Bot,
    chat_id: ChatId,
    user_id: UserId,
    sessions: &Sessions,
    pool: &SqlitePool,
    _config: &TelegramUiConfig,
) -> ResponseResult<()> {
    if let Some(draft) = get_session(user_id, sessions) {
        if draft.is_expired() {
            cancel_user_order(pool, user_id, sessions).await;
            return bot
                .send_message(chat_id, "<b>Order expired.</b>\nUse /create.")
                .parse_mode(ParseMode::Html)
                .reply_markup(main_menu_keyboard())
                .await
                .map(|_| ());
        }
        return continue_step_screen(bot, chat_id, &draft).await;
    }

    match db::get_pending_order_for_user(pool, user_id.0 as i64).await {
        Ok(Some(order)) => {
            let build_mode = match order.build_mode {
                Some(crate::models::BuildMode::Template) => BuildModeDraft::Template,
                _ => BuildModeDraft::AiFree,
            };
            let draft = OrderDraft {
                build_mode,
                step: DraftStep::ReadyToPay,
                site_price_usdt: order.amount_usdt,
                template_id: order.template_id.clone(),
                logo_file_id: None,
                ticker: None,
                token_name: None,
                contract_address: None,
                x_link: None,
                telegram_link: None,
                website_url: None,
                domain_quote_id: None,
                domain_price_usdt: None,
                alternative_domains: None,
                total_usdt: Some(order.amount_usdt),
                payment_chain: None,
                order_id: Some(order.id),
                db_user_id: Some(order.user_id),
                updated_at: chrono::Utc::now(),
            };
            set_session(user_id, draft.clone(), sessions);
            continue_step_screen(bot, chat_id, &draft).await
        }
        Ok(None) => bot
            .send_message(chat_id, "<b>No active order.</b>\nUse /create.")
            .parse_mode(ParseMode::Html)
            .reply_markup(main_menu_keyboard())
            .await
            .map(|_| ()),
        Err(error) => {
            warn!(
                ?error,
                user_id = user_id.0 as i64,
                "failed to load pending order"
            );
            bot.send_message(chat_id, "<b>Check failed.</b>\nTry later.")
                .parse_mode(ParseMode::Html)
                .reply_markup(main_menu_keyboard())
                .await
                .map(|_| ())
        }
    }
}

async fn continue_step_screen(
    bot: &Bot,
    chat_id: ChatId,
    draft: &OrderDraft,
) -> ResponseResult<()> {
    match draft.step {
        DraftStep::WaitingLogo => bot
            .send_message(chat_id, ask_logo_text())
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingTicker => bot
            .send_message(chat_id, "🔤 <b>2/7 Ticker</b>\nSend token ticker. Eg: PEPE")
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingTokenName => bot
            .send_message(
                chat_id,
                "🏷 <b>3/7 Token Name</b>\nSend token name. Eg: Pepe Coin",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingContractAddress => bot
            .send_message(
                chat_id,
                "📝 <b>4/7 Contract Address</b>\nPaste contract address.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingXLink => bot
            .send_message(
                chat_id,
                "🐦 <b>5/7 X (Twitter) Link</b>\nPaste X link or send '-' to skip.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingTelegramLink => bot
            .send_message(
                chat_id,
                "📱 <b>6/7 Telegram Link</b>\nPaste TG link or send '-' to skip.",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingWebsiteUrl => bot
            .send_message(
                chat_id,
                "🌐 <b>7/7 Domain</b>\nSend domain name.\nEg: mytoken.xyz",
            )
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        DraftStep::ReadyToPay => bot
            .send_message(chat_id, payment_chain_text())
            .parse_mode(ParseMode::Html)
            .reply_markup(payment_chain_keyboard())
            .await
            .map(|_| ()),
        DraftStep::WaitingTx => bot
            .send_message(chat_id, "💳 <b>Payment</b>\nPaste TX hash.")
            .parse_mode(ParseMode::Html)
            .reply_markup(cancel_keyboard())
            .await
            .map(|_| ()),
        DraftStep::PaymentConfirmed => bot
            .send_message(chat_id, "<b>Payment confirmed.</b>\nWebsite is queued.")
            .parse_mode(ParseMode::Html)
            .reply_markup(status_keyboard())
            .await
            .map(|_| ()),
    }
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

async fn show_create_menu(
    bot: &Bot,
    chat_id: ChatId,
    template_prices_usdt: &[Decimal],
    ai_creation_price_usdt: Decimal,
) -> ResponseResult<()> {
    send_screen(
        bot,
        chat_id,
        create_menu_text(),
        create_mode_keyboard(template_prices_usdt, ai_creation_price_usdt),
    )
    .await
}

async fn edit_create_menu(
    bot: &Bot,
    target: UiMessage,
    template_prices_usdt: &[Decimal],
    ai_creation_price_usdt: Decimal,
) -> ResponseResult<()> {
    edit_screen(
        bot,
        target,
        create_menu_text(),
        create_mode_keyboard(template_prices_usdt, ai_creation_price_usdt),
    )
    .await
}

async fn edit_template_mode(
    bot: &Bot,
    target: UiMessage,
    config: &TelegramUiConfig,
) -> ResponseResult<()> {
    edit_screen(
        bot,
        target,
        template_mode_text(),
        template_keyboard(&config.template_options, &config.template_prices_usdt),
    )
    .await
}

async fn show_template_confirmation(
    bot: &Bot,
    chat_id: ChatId,
    template_id: &str,
    config: &TelegramUiConfig,
) -> ResponseResult<()> {
    let Some(template) = find_template(&config.template_options, template_id) else {
        return send_screen(
            bot,
            chat_id,
            "<b>Template not found.</b>\nChoose again.",
            template_keyboard(&config.template_options, &config.template_prices_usdt),
        )
        .await;
    };
    let price = template_price(
        &config.template_options,
        template_id,
        &config.template_prices_usdt,
    );

    send_screen(
        bot,
        chat_id,
        template_confirmation_text(&template, price),
        template_confirmation_keyboard(template_id, &template.preview_url),
    )
    .await
}

async fn edit_template_confirmation(
    bot: &Bot,
    target: UiMessage,
    template_id: &str,
    config: &TelegramUiConfig,
) -> ResponseResult<()> {
    let Some(template) = find_template(&config.template_options, template_id) else {
        return edit_screen(
            bot,
            target,
            "<b>Template not found.</b>\nChoose again.",
            template_keyboard(&config.template_options, &config.template_prices_usdt),
        )
        .await;
    };
    let price = template_price(
        &config.template_options,
        template_id,
        &config.template_prices_usdt,
    );

    edit_screen(
        bot,
        target,
        template_confirmation_text(&template, price),
        template_confirmation_keyboard(template_id, &template.preview_url),
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

async fn show_history(
    bot: &Bot,
    chat_id: ChatId,
    user_id: UserId,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    send_screen(
        bot,
        chat_id,
        history_text(user_id, pool).await,
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

async fn edit_history(
    bot: &Bot,
    target: UiMessage,
    user_id: UserId,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    edit_screen(
        bot,
        target,
        history_text(user_id, pool).await,
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
            "👤 <b>Contact admin</b>\n{}",
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
            "<b>No active order.</b>\nUse /create.",
            main_menu_keyboard(),
        )
        .await;
    };
    if draft.is_expired() {
        cancel_user_order(pool, user_id, sessions).await;
        return edit_screen(
            bot,
            target,
            "<b>Order expired.</b>\nUse /create.",
            main_menu_keyboard(),
        )
        .await;
    }

    let Some(total) = draft.total_usdt else {
        return edit_screen(
            bot,
            target,
            "<b>Payment not ready.</b>\nComplete all steps first.",
            cancel_keyboard(),
        )
        .await;
    };

    if address.trim().is_empty() {
        return edit_screen(
            bot,
            target,
            "<b>Payment address missing.</b>\nContact admin.",
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
                    "<b>Order failed.</b>\nPlease try again.",
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
    let order_id = if let Some(order_id) = draft.order_id {
        order_id
    } else {
        let order_id = match db::get_or_create_site_order(
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
                    "<b>Order failed.</b>\nPlease try again.",
                    main_menu_keyboard(),
                )
                .await;
            }
        };
        draft.order_id = Some(order_id);
        info!(
            user_id = user_id.0 as i64,
            order_id = %order_id,
            total_usdt = %total,
            chain = chain.as_str(),
            "site order created before payment"
        );
        set_session(user_id, draft.clone(), sessions);
        order_id
    };

    let Some(input) = website_input_from_draft(&draft) else {
        return edit_screen(
            bot,
            target,
            "<b>Payment not ready.</b>\nComplete all steps first.",
            cancel_keyboard(),
        )
        .await;
    };
    if let Err(error) =
        db::prepare_order_for_payment(pool, order_id, total, draft.domain_quote_id).await
    {
        warn!(
            ?error,
            user_id = user_id.0 as i64,
            order_id = %order_id,
            "failed to update order total and domain quote before payment"
        );
        return edit_screen(
            bot,
            target,
            "<b>Payment setup failed.</b>\nUse /create.",
            main_menu_keyboard(),
        )
        .await;
    }
    let website_id = match db::upsert_website_for_order(pool, db_user_id, order_id, &input).await {
        Ok(id) => {
            info!(
                user_id = user_id.0 as i64,
                order_id = %order_id,
                website_id = %id,
                domain = input.requested_domain.as_deref().unwrap_or("<none>"),
                "website input persisted before payment"
            );
            id
        }
        Err(error) => {
            warn!(?error, order_id = %order_id, "failed to persist website input before payment");
            return edit_screen(
                bot,
                target,
                "<b>Order save failed.</b>\nPlease try again.",
                main_menu_keyboard(),
            )
            .await;
        }
    };
    if let Some(quote_id) = draft.domain_quote_id
        && let Err(error) = db::attach_quote_website(pool, quote_id, website_id).await
    {
        warn!(?error, quote_id = %quote_id, website_id = %website_id, "failed to attach quote to website");
    }

    let payment_window = match db::attach_payment_chain(pool, order_id, chain).await {
        Ok(window) => {
            info!(
                user_id = user_id.0 as i64,
                order_id = %order_id,
                chain = chain.as_str(),
                payment_started_at = %window.started_at,
                payment_expires_at = %window.expires_at,
                "payment window started"
            );
            window
        }
        Err(e) => {
            warn!(?e, order_id = %order_id, "failed to attach payment chain");
            return edit_screen(
                bot,
                target,
                "<b>Payment start failed.</b>\nPlease try again.",
                main_menu_keyboard(),
            )
            .await;
        }
    };

    draft.payment_chain = Some(chain);
    draft.step = DraftStep::WaitingTx;
    draft.order_id = Some(order_id);
    draft.db_user_id = Some(db_user_id);
    info!(
        user_id = user_id.0 as i64,
        order_id = %order_id,
        chain = chain.as_str(),
        next_step = DraftStep::WaitingTx.as_str(),
        "waiting for payment transaction"
    );
    set_session(user_id, draft, sessions);

    edit_screen(
        bot,
        target,
        payment_instruction(order_id, total, chain, address, &payment_window),
        cancel_keyboard(),
    )
    .await
}

async fn handle_alternative_domain_selection(
    bot: &Bot,
    target: UiMessage,
    user_id: UserId,
    domain: &str,
    _config: &TelegramUiConfig,
    sessions: &Sessions,
    pool: &SqlitePool,
) -> ResponseResult<()> {
    let Some(mut draft) = get_session(user_id, sessions) else {
        return edit_screen(
            bot,
            target,
            "<b>No active order.</b>\nUse /create.",
            main_menu_keyboard(),
        )
        .await;
    };
    if draft.is_expired() {
        cancel_user_order(pool, user_id, sessions).await;
        return edit_screen(
            bot,
            target,
            "<b>Order expired.</b>\nUse /create.",
            main_menu_keyboard(),
        )
        .await;
    }

    let Some(alternatives) = &draft.alternative_domains else {
        return edit_screen(
            bot,
            target,
            "<b>Session expired.</b>\nUse /create.",
            main_menu_keyboard(),
        )
        .await;
    };

    let Some(quote) = alternatives.iter().find(|q| q.domain == domain).cloned() else {
        return edit_screen(
            bot,
            target,
            "<b>Domain expired.</b>\nUse /create.",
            main_menu_keyboard(),
        )
        .await;
    };

    let site_price_usdt = draft.site_price_usdt;

    let db_user_id = match db::upsert_user(pool, user_id.0 as i64, None, None, None, true).await {
        Ok(id) => id,
        Err(e) => {
            warn!(
                ?e,
                user_id = user_id.0 as i64,
                "failed to upsert user for alt domain"
            );
            return edit_screen(
                bot,
                target,
                "<b>Selection failed.</b>\nPlease try again.",
                main_menu_keyboard(),
            )
            .await;
        }
    };

    let domain_quote_id =
        match db::record_domain_quote(pool, db_user_id, &quote, quote.price_usd).await {
            Ok(id) => id,
            Err(e) => {
                warn!(?e, "failed to record domain quote for alt domain");
                return edit_screen(
                    bot,
                    target,
                    "<b>Selection failed.</b>\nPlease try again.",
                    main_menu_keyboard(),
                )
                .await;
            }
        };

    let total = site_price_usdt + quote.price_usd;
    if let Some(order_id) = draft.order_id
        && let Err(error) =
            db::prepare_order_for_payment(pool, order_id, total, Some(domain_quote_id)).await
    {
        warn!(
            ?error,
            user_id = user_id.0 as i64,
            order_id = %order_id,
            quote_id = %domain_quote_id,
            "failed to attach alternative domain quote to pending order"
        );
        return edit_screen(
            bot,
            target,
            "<b>Quote save failed.</b>\nPlease try again.",
            main_menu_keyboard(),
        )
        .await;
    }
    draft.website_url = Some(domain.to_string());
    draft.db_user_id = Some(db_user_id);
    draft.domain_price_usdt = Some(quote.price_usd);
    draft.domain_quote_id = Some(domain_quote_id);
    draft.total_usdt = Some(total);
    draft.step = DraftStep::ReadyToPay;
    draft.alternative_domains = None;
    set_session(user_id, draft.clone(), sessions);

    edit_screen(
        bot,
        target,
        format!(
            "✅ <b>Domain selected</b>\n{domain}\nDomain: <b>{}</b> USDT\nWebsite: <b>{}</b> USDT\nTotal: <b>{}</b> USDT\n\nChoose payment chain.",
            quote.price_usd, site_price_usdt, total
        ),
        payment_chain_keyboard(),
    )
    .await
}

fn set_session(user_id: UserId, mut draft: OrderDraft, sessions: &Sessions) {
    draft.touch();
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

async fn session_step_name(user_id: UserId, sessions: &Sessions) -> Option<&'static str> {
    sessions
        .lock()
        .expect("sessions lock")
        .get(&user_id)
        .map(|draft| draft.step.as_str())
}

fn message_kind(msg: &Message) -> &'static str {
    if msg.text().is_some() {
        "text"
    } else if msg.photo().is_some() {
        "photo"
    } else if msg.document().is_some() {
        "document"
    } else {
        "other"
    }
}

fn remove_session(user_id: UserId, sessions: &Sessions) {
    sessions.lock().expect("sessions lock").remove(&user_id);
}

async fn maybe_purchase_domain_after_tx(
    config: &TelegramUiConfig,
    pool: &SqlitePool,
    quote_id: Option<Uuid>,
    domain: Option<&str>,
) -> (bool, String) {
    if !config.namecheap.auto_purchase_after_tx {
        info!(
            quote_id = %quote_id.map(|id| id.to_string()).unwrap_or_else(|| "<none>".to_string()),
            domain = domain.unwrap_or("<none>"),
            "automatic domain purchase disabled"
        );
        return (
            false,
            "<b>Domain:</b> purchase disabled. Contact admin.".to_string(),
        );
    }

    let Some(domain) = domain else {
        warn!(
            quote_id = %quote_id.map(|id| id.to_string()).unwrap_or_else(|| "<none>".to_string()),
            "domain purchase skipped because no domain is attached"
        );
        return (false, "<b>Domain:</b> missing. Contact admin.".to_string());
    };
    let Some(quote_id) = quote_id else {
        warn!(
            domain = %domain,
            "domain purchase skipped because no persisted quote is attached"
        );
        return (
            false,
            "<b>Domain:</b> quote missing. Contact admin.".to_string(),
        );
    };
    info!(
        quote_id = %quote_id,
        domain = %domain,
        "starting post-payment domain purchase flow"
    );
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

    let quote_record = match db::get_domain_quote_for_purchase(pool, quote_id).await {
        Ok(record) => record,
        Err(error) => {
            warn!(?error, quote_id = %quote_id, "failed to load persisted domain quote");
            let _ = db::mark_domain_purchase_manual_review(
                pool,
                quote_id,
                "persisted domain quote could not be loaded before purchase",
            )
            .await;
            return (false, "<b>Domain:</b> manual review required.".to_string());
        }
    };
    if !quote_record.domain.eq_ignore_ascii_case(domain) {
        warn!(
            quote_id = %quote_id,
            requested_domain = %domain,
            quoted_domain = %quote_record.domain,
            "domain quote mismatch before purchase"
        );
        let _ = db::mark_domain_purchase_manual_review(
            pool,
            quote_id,
            "persisted quote domain does not match requested domain",
        )
        .await;
        return (
            false,
            "<b>Domain:</b> quote mismatch. Contact admin.".to_string(),
        );
    }

    let price_tolerance = Decimal::new(1, 2);
    match quote_domain_with_retries(registrar.as_ref(), domain).await {
        Ok(current_quote) if current_quote.available && !current_quote.is_premium => {
            info!(
                quote_id = %quote_id,
                domain = %domain,
                current_price_usd = %current_quote.price_usd,
                paid_quote_usdt = %quote_record.quoted_amount_usdt,
                "domain price refreshed before purchase"
            );
            if current_quote.price_usd > quote_record.quoted_amount_usdt + price_tolerance {
                let reason = format!(
                    "current Namecheap price {} exceeds paid domain quote {}",
                    current_quote.price_usd, quote_record.quoted_amount_usdt
                );
                let _ = db::mark_domain_purchase_manual_review(pool, quote_id, &reason).await;
                return (
                    false,
                    format!(
                        "<b>Domain:</b> price changed (<code>{}</code> > <code>{}</code>). Contact admin.",
                        current_quote.price_usd, quote_record.quoted_amount_usdt
                    ),
                );
            }
        }
        Ok(_) => {
            let _ = db::mark_domain_purchase_manual_review(
                pool,
                quote_id,
                "domain became unavailable or premium before purchase",
            )
            .await;
            return (
                false,
                "<b>Domain:</b> unavailable. Contact admin.".to_string(),
            );
        }
        Err(error) => {
            let _ = db::mark_domain_purchase_manual_review(
                pool,
                quote_id,
                "domain price refresh failed before purchase",
            )
            .await;
            return (
                false,
                format!(
                    "<b>Domain:</b> price check failed. {}",
                    html_escape(&domain_error_for_user(&error))
                ),
            );
        }
    }

    if let Err(error) = db::record_domain_purchase_started(pool, quote_id).await {
        warn!(?error, quote_id = %quote_id, "failed to record domain purchase start");
    }
    info!(
        quote_id = %quote_id,
        domain = %domain,
        "submitting domain purchase"
    );
    match purchase_domain_with_retries(registrar.as_ref(), domain).await {
        Ok(result) => {
            info!(
                quote_id = %quote_id,
                domain = %domain,
                namecheap_order_id = %result.namecheap_order_id,
                charged_amount_usd = %result.charged_amount_usd,
                "domain purchase succeeded"
            );
            if let Err(error) = db::record_domain_purchase_success(pool, quote_id, &result).await {
                warn!(?error, quote_id = %quote_id, "failed to record domain purchase success");
            }
            if result.charged_amount_usd > quote_record.quoted_amount_usdt + price_tolerance {
                let reason = format!(
                    "Namecheap charged {} after paid domain quote {} (initial Namecheap quote {})",
                    result.charged_amount_usd,
                    quote_record.quoted_amount_usdt,
                    quote_record.namecheap_price_usd
                );
                let _ = db::mark_domain_purchase_manual_review(pool, quote_id, &reason).await;
                return (
                    false,
                    format!(
                        "<b>Domain bought.</b> Charge <code>{}</code> exceeds quote <code>{}</code>. Contact admin.",
                        result.charged_amount_usd, quote_record.quoted_amount_usdt
                    ),
                );
            }
            let dns_result = setup_cloudflare_dns_after_purchase_with_retries(config, domain).await;
            let dns_ready = match &dns_result {
                Ok(zone) => {
                    info!(
                        quote_id = %quote_id,
                        domain = %domain,
                        cloudflare_zone_id = %zone.zone_id,
                        nameservers = ?zone.nameservers,
                        "dns setup succeeded after domain purchase"
                    );
                    if let Err(error) = db::record_domain_dns_result(pool, quote_id, Ok(zone)).await
                    {
                        warn!(?error, quote_id = %quote_id, "failed to record dns success");
                    }
                    true
                }
                Err(message) => {
                    warn!(
                        quote_id = %quote_id,
                        domain = %domain,
                        error = %message,
                        "dns setup failed after domain purchase"
                    );
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
                    "<b>DNS configured.</b> Zone <code>{}</code>.",
                    html_escape(&zone.zone_id)
                ),
                Err(message) => message,
            };
            (
                dns_ready,
                format!(
                    "<b>Domain purchased.</b>\nOrder: <code>{}</code>\nTX: <code>{}</code>\n{}",
                    html_escape(&result.namecheap_order_id),
                    html_escape(&result.namecheap_transaction_id),
                    dns_message
                ),
            )
        }
        Err(error) => {
            warn!(
                quote_id = %quote_id,
                domain = %domain,
                error = %error,
                "domain purchase failed"
            );
            if matches!(&error, DomainError::AlreadyRegisteredToAccount) {
                if let Err(db_error) = db::mark_domain_purchase_manual_review(
                    pool,
                    quote_id,
                    "domain appears registered in Namecheap account; reconcile order manually",
                )
                .await
                {
                    warn!(?db_error, quote_id = %quote_id, "failed to record domain purchase manual review");
                }
            } else if let Err(db_error) = db::mark_domain_purchase_failed(pool, quote_id).await {
                warn!(?db_error, quote_id = %quote_id, "failed to record domain purchase failure");
            }
            (
                false,
                format!(
                    "<b>Domain failed.</b>\n{}",
                    html_escape(&domain_error_for_user(&error))
                ),
            )
        }
    }
}

async fn enqueue_generation_job_with_retries(
    pool: &SqlitePool,
    order_id: Uuid,
    website_id: Uuid,
) -> anyhow::Result<Uuid> {
    let mut last_error = None;
    for attempt in 1..=EXTERNAL_STEP_MAX_ATTEMPTS {
        match db::enqueue_generation_job(pool, order_id, website_id).await {
            Ok(id) => return Ok(id),
            Err(error) if attempt < EXTERNAL_STEP_MAX_ATTEMPTS => {
                warn!(?error, order_id = %order_id, attempt, "failed to enqueue generation job; retrying");
                last_error = Some(error);
                sleep(TokioDuration::from_secs(attempt as u64)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error
        .unwrap_or_else(|| anyhow::anyhow!("enqueue generation job failed after retries")))
}

async fn download_telegram_logo(
    bot: &Bot,
    file_id: &str,
    order_id: &str,
) -> anyhow::Result<String> {
    let file = bot
        .get_file(teloxide::types::FileId::from(file_id.to_string()))
        .await?;
    let ext = Path::new(&file.path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_else(|| ".png".to_string());
    let dir = "./logos";
    tokio::fs::create_dir_all(dir).await?;
    let dest = format!("{dir}/{order_id}{ext}");
    let url = format!(
        "https://api.telegram.org/file/bot{}/{}",
        bot.token(),
        file.path
    );
    let response = reqwest::get(&url).await?;
    let bytes = response.bytes().await?;
    tokio::fs::write(&dest, &bytes).await?;
    info!(file_id = %file_id, dest = %dest, "downloaded logo from Telegram");
    Ok(dest)
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
        info!(
            order_id = %request.order_id,
            chain = request.chain.as_str(),
            attempt,
            "checking payment transaction"
        );
        match service.verify_tx(request.clone()).await {
            Ok(outcome) => {
                info!(
                    order_id = %request.order_id,
                    chain = request.chain.as_str(),
                    attempt,
                    "payment transaction check succeeded"
                );
                return Ok(outcome);
            }
            Err(error)
                if retryable_payment_error(&error) && attempt < EXTERNAL_STEP_MAX_ATTEMPTS =>
            {
                warn!(
                    order_id = %request.order_id,
                    chain = request.chain.as_str(),
                    attempt,
                    error = %error,
                    "payment transaction check failed; retrying"
                );
                last_error = Some(error);
                sleep(TokioDuration::from_secs(attempt as u64 * 2)).await;
            }
            Err(error) => {
                warn!(
                    order_id = %request.order_id,
                    chain = request.chain.as_str(),
                    attempt,
                    error = %error,
                    "payment transaction check failed"
                );
                return Err(error);
            }
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
        info!(%domain, attempt, "checking domain quote");
        match registrar.quote(&request).await {
            Ok(quote) => {
                info!(
                    %domain,
                    attempt,
                    available = quote.available,
                    premium = quote.is_premium,
                    price_usd = %quote.price_usd,
                    "domain quote check succeeded"
                );
                return Ok(quote);
            }
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
        info!(%domain, attempt, "purchasing domain");
        match registrar.purchase(&request).await {
            Ok(result) => {
                info!(
                    %domain,
                    attempt,
                    namecheap_order_id = %result.namecheap_order_id,
                    charged_amount_usd = %result.charged_amount_usd,
                    "domain purchase call succeeded"
                );
                return Ok(result);
            }
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
        info!(%domain, attempt, "setting up cloudflare/namecheap dns");
        match setup_cloudflare_dns_after_purchase(config, domain).await {
            Ok(zone) => {
                info!(
                    %domain,
                    attempt,
                    cloudflare_zone_id = %zone.zone_id,
                    nameservers = ?zone.nameservers,
                    "dns setup call succeeded"
                );
                return Ok(zone);
            }
            Err(error) if attempt < EXTERNAL_STEP_MAX_ATTEMPTS => {
                warn!(%domain, attempt, error = %error, "dns setup failed; retrying");
                last_error = Some(error);
                sleep(TokioDuration::from_secs(attempt as u64 * 3)).await;
            }
            Err(error) => {
                warn!(%domain, attempt, error = %error, "dns setup failed");
                return Err(error);
            }
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
        info!(%domain, "creating cloudflare zone through api-server");
        return match dns.create_zone(domain).await {
            Ok(zone) => match dns
                .set_namecheap_nameservers(domain, &zone.nameservers)
                .await
            {
                Ok(()) => {
                    info!(
                        %domain,
                        cloudflare_zone_id = %zone.zone_id,
                        nameservers = ?zone.nameservers,
                        "namecheap nameservers updated through api-server"
                    );
                    Ok(zone)
                }
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

    let namecheap = NamecheapDnsProvisioner::new(config.namecheap.clone());
    info!(%domain, "creating cloudflare zone directly");
    match crate::deployment::cloudflare::create_cloudflare_zone(
        &config.cloudflare_account_id,
        &config.cloudflare_api_token,
        domain,
    )
    .await
    {
        Ok(zone) => match namecheap
            .set_namecheap_nameservers(domain, &zone.nameservers)
            .await
        {
            Ok(()) => {
                info!(
                    %domain,
                    cloudflare_zone_id = %zone.zone_id,
                    nameservers = ?zone.nameservers,
                    "namecheap nameservers updated directly"
                );
                Ok(zone)
            }
            Err(error) => Err(format!(
                "\n<b>DNS nameserver update failed.</b> {}",
                html_escape(&domain_error_for_user(&error))
            )),
        },
        Err(error) => {
            let domain_err: DomainError = error.into();
            Err(format!(
                "\n<b>Cloudflare zone creation failed.</b> {}",
                html_escape(&domain_error_for_user(&domain_err))
            ))
        }
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
        vec![InlineKeyboardButton::callback("🧾 Paid History", "history")],
        vec![InlineKeyboardButton::callback(
            "👤 Contact Admin",
            "contact_admin",
        )],
    ])
}

fn create_mode_keyboard(
    template_prices_usdt: &[Decimal],
    ai_creation_price_usdt: Decimal,
) -> InlineKeyboardMarkup {
    let (min, max) = template_prices_usdt
        .iter()
        .fold((Decimal::MAX, Decimal::ZERO), |(min, max), &p| {
            (min.min(p), max.max(p))
        });
    let template_label = if min == max {
        format!("📦 Template Mode | {} USDT", min)
    } else {
        format!("📦 Template Mode | {}-{} USDT", min, max)
    };
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            template_label,
            "template_mode",
        )],
        vec![InlineKeyboardButton::callback(
            format!("🤖 Free Creation | {} USDT", ai_creation_price_usdt),
            "ai_mode",
        )],
        vec![InlineKeyboardButton::callback("⬅ Back", "main")],
    ])
}

fn template_keyboard(options: &[TemplateOption], prices: &[Decimal]) -> InlineKeyboardMarkup {
    let mut rows = Vec::new();
    for chunk in options.chunks(2) {
        let mut row = Vec::new();
        for template in chunk {
            let number = template_number(options, &template.id);
            let price = template_price(options, &template.id, prices);
            row.push(InlineKeyboardButton::callback(
                format!("{number:02}. {} · {}U", template.name, price),
                format!("select_tpl:{}", template.id),
            ));
        }
        rows.push(row);
    }
    rows.push(vec![InlineKeyboardButton::callback("⬅ Back", "create")]);
    InlineKeyboardMarkup::new(rows)
}

fn template_confirmation_keyboard(template_id: &str, preview_url: &str) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            "✅ Confirm Template",
            format!("confirm_tpl:{template_id}"),
        )],
        vec![InlineKeyboardButton::url(
            "🔗 Preview Template",
            url::Url::parse(preview_url).expect("valid preview url"),
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
        BuildModeDraft::AiFree => "Free Creation",
    }
}

pub fn payment_instruction(
    order_id: Uuid,
    amount_usdt: Decimal,
    chain: PaymentChain,
    address: &str,
    payment_window: &db::PaymentWindow,
) -> String {
    format!(
        "💳 <b>Pay</b>\nOrder: <code>{order_id}</code>\nAmount: <b>{amount_usdt}</b> USDT\nChain: <b>{}</b>\nAddress:\n<code>{}</code>\n\nExpires: <code>{}</code> UTC\nSend TX hash after payment.",
        chain.as_str(),
        html_escape(address),
        payment_window.expires_at.format("%Y-%m-%d %H:%M:%S")
    )
}

fn parse_template_deep_link(text: &str) -> Option<&str> {
    text.trim()
        .strip_prefix("/start tpl_")
        .or_else(|| text.trim().strip_prefix("/start=tpl_"))
}

async fn check_alternative_domains(
    registrar: &dyn DomainRegistrar,
    base_name: &str,
    exclude_tld: &str,
) -> Vec<crate::domain::DomainQuote> {
    let mut alternatives = Vec::new();
    for tld in crate::domain::SUPPORTED_TLDS {
        if *tld == exclude_tld {
            continue;
        }
        let domain = format!("{base_name}.{tld}");
        let request = DomainQuoteRequest { domain };
        if let Ok(quote) = registrar.quote(&request).await
            && quote.available
            && !quote.is_premium
        {
            alternatives.push(quote);
        }
    }
    alternatives.sort_by_key(|quote| quote.price_usd);
    alternatives
}

fn alternative_domains_keyboard(
    alternatives: &[crate::domain::DomainQuote],
) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
    for quote in alternatives.iter().take(12) {
        rows.push(vec![InlineKeyboardButton::callback(
            format!("{} — {} USDT", quote.domain, quote.price_usd.round_dp(2)),
            format!("alt_domain:{}", quote.domain),
        )]);
    }
    rows.push(vec![InlineKeyboardButton::callback(
        "✏️ Enter another domain",
        "alt_domain_custom",
    )]);
    InlineKeyboardMarkup::new(rows)
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

fn find_template(options: &[TemplateOption], template_id: &str) -> Option<TemplateOption> {
    options
        .iter()
        .find(|template| template.id == template_id)
        .cloned()
}

fn template_number(options: &[TemplateOption], template_id: &str) -> usize {
    options
        .iter()
        .position(|template| template.id == template_id)
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn template_price(options: &[TemplateOption], template_id: &str, prices: &[Decimal]) -> Decimal {
    let index = options
        .iter()
        .position(|template| template.id == template_id)
        .unwrap_or(0);
    prices.get(index).copied().unwrap_or(Decimal::ZERO)
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
        PaymentVerificationError::NotFound => "TX not found.".to_string(),
        PaymentVerificationError::WrongChain => "Wrong chain.".to_string(),
        PaymentVerificationError::WrongToken => "Not a supported USDT transfer.".to_string(),
        PaymentVerificationError::WrongRecipient => "Wrong recipient.".to_string(),
        PaymentVerificationError::InsufficientAmount => "Amount too low.".to_string(),
        PaymentVerificationError::InsufficientConfirmations => {
            "Not enough confirmations.".to_string()
        }
        PaymentVerificationError::AlreadyUsed => "TX already used.".to_string(),
        PaymentVerificationError::ExpiredOrder => "Payment window expired.".to_string(),
        PaymentVerificationError::OldTransaction => {
            "TX is older than this payment request.".to_string()
        }
        PaymentVerificationError::TransactionAfterSubmission => {
            "TX is not confirmed yet. Try again later.".to_string()
        }
        PaymentVerificationError::Provider(value) => {
            format!("Payment provider error: {value}. Contact admin.")
        }
    }
}

fn domain_error_for_user(error: &DomainError) -> String {
    match error {
        DomainError::MissingConfig(value) => {
            format!("Namecheap configuration is missing: {value}")
        }
        DomainError::UnsupportedTld => {
            "Unsupported TLD. Supported: .com, .net, .org, .xyz, .fun, .site, .online, .io, .co, .me, .cc, .app, .dev, .world, .space, .club, .store, .shop, .vip".to_string()
        }
        DomainError::Unavailable => "Domain is unavailable.".to_string(),
        DomainError::AlreadyRegisteredToAccount => {
            "Domain appears registered in the Namecheap account. Contact admin for reconciliation."
                .to_string()
        }
        DomainError::PremiumRejected => "Premium/EAP domains require manual handling.".to_string(),
        DomainError::Namecheap(value) => format!("Namecheap returned an error: {value}"),
        DomainError::Http(value) => format!("Namecheap HTTP request failed: {value}"),
        DomainError::Parse(value) => format!("Could not parse Namecheap response: {value}"),
        DomainError::Cloudflare(value) => format!("Cloudflare DNS failed: {value}"),
    }
}
